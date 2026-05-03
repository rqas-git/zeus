//! Owned ChatGPT OAuth storage and refresh handling.

use std::fmt::Write as _;
use std::path::Path;
use std::path::PathBuf;
use std::time::Duration;
use std::time::Instant;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use anyhow::Context;
use anyhow::Result;
use base64::Engine;
use reqwest::header;
use reqwest::Client;
use reqwest::StatusCode;
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;
use tokio::sync::Mutex;

const DEFAULT_AUTH_ISSUER: &str = "https://auth.openai.com";
const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const ACCESS_TOKEN_REFRESH_BUFFER_SECONDS: u64 = 60;
const MAX_REFRESH_AGE_SECONDS: u64 = 8 * 24 * 60 * 60;
const DEVICE_CODE_TIMEOUT: Duration = Duration::from_secs(15 * 60);

/// Short-lived credentials required by the model backend.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AuthCredentials {
    access_token: String,
    account_id: String,
}

impl AuthCredentials {
    fn new(access_token: String, account_id: String) -> Self {
        Self {
            access_token,
            account_id,
        }
    }

    /// Returns the bearer token for the ChatGPT backend request.
    pub(crate) fn access_token(&self) -> &str {
        &self.access_token
    }

    /// Returns the selected ChatGPT account/workspace id.
    pub(crate) fn account_id(&self) -> &str {
        &self.account_id
    }
}

/// Device-code authorization data to show to the user.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DeviceLogin {
    verification_url: String,
    user_code: String,
    device_auth_id: String,
    interval_seconds: u64,
}

impl DeviceLogin {
    /// Returns the browser URL where the user enters the code.
    pub(crate) fn verification_url(&self) -> &str {
        &self.verification_url
    }

    /// Returns the one-time user code for device authorization.
    pub(crate) fn user_code(&self) -> &str {
        &self.user_code
    }
}

/// Current local login status.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum AuthStatus {
    LoggedOut,
    LoggedIn {
        account_id: String,
        expires_at_unix: u64,
    },
}

/// Result of local logout and best-effort remote token revocation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LogoutResult {
    removed: bool,
    revoke_error: Option<String>,
}

impl LogoutResult {
    fn new(removed: bool, revoke_error: Option<String>) -> Self {
        Self {
            removed,
            revoke_error,
        }
    }

    /// Returns true when a local auth file was removed.
    pub(crate) fn removed(&self) -> bool {
        self.removed
    }

    /// Returns a non-fatal remote revoke error, if revocation failed.
    pub(crate) fn revoke_error(&self) -> Option<&str> {
        self.revoke_error.as_deref()
    }
}

/// Manages first-party rust-agent login state.
pub(crate) struct AuthManager {
    storage: AuthStorage,
    http: Client,
    issuer: String,
    refresh_lock: Mutex<()>,
}

impl AuthManager {
    /// Creates an auth manager using the default rust-agent auth file.
    ///
    /// # Errors
    /// Returns an error when the auth home or HTTP client cannot be prepared.
    pub(crate) fn new_default() -> Result<Self> {
        Self::new(AuthStorage::new(default_auth_file()?))
    }

    fn new(storage: AuthStorage) -> Result<Self> {
        Self::with_issuer(storage, DEFAULT_AUTH_ISSUER)
    }

    fn with_issuer(storage: AuthStorage, issuer: impl Into<String>) -> Result<Self> {
        let http = Client::builder()
            .timeout(Duration::from_secs(60))
            .build()
            .context("failed to build auth HTTP client")?;
        Ok(Self {
            storage,
            http,
            issuer: issuer.into().trim_end_matches('/').to_string(),
            refresh_lock: Mutex::new(()),
        })
    }

    #[cfg(test)]
    pub(crate) fn for_test(auth_file: PathBuf, issuer: String) -> Result<Self> {
        Self::with_issuer(AuthStorage::new(auth_file), issuer)
    }

    /// Returns fresh credentials, refreshing the local access token when needed.
    ///
    /// # Errors
    /// Returns an error when no login exists, token parsing fails, or refresh fails.
    pub(crate) async fn credentials(&self) -> Result<AuthCredentials> {
        let auth_file = self.load_required().await?;
        if auth_file.needs_refresh(now_unix()) {
            return self.refresh_locked(false).await;
        }
        auth_file.credentials()
    }

    /// Refreshes credentials even if the stored access token appears usable.
    ///
    /// # Errors
    /// Returns an error when no login exists or the refresh request fails.
    pub(crate) async fn refresh(&self) -> Result<AuthCredentials> {
        self.refresh_locked(true).await
    }

    /// Starts device-code login and returns the code to display.
    ///
    /// # Errors
    /// Returns an error when the device-code request fails.
    pub(crate) async fn start_device_login(&self) -> Result<DeviceLogin> {
        let url = format!("{}/api/accounts/deviceauth/usercode", self.issuer);
        let response = self
            .http
            .post(url)
            .json(&DeviceCodeRequest {
                client_id: CLIENT_ID,
            })
            .send()
            .await
            .context("failed to request device login code")?;

        if !response.status().is_success() {
            let status = response.status();
            let body = error_response_body(response).await;
            anyhow::bail!(
                "device login code request returned {status}: {}",
                truncate_for_error(&body)
            );
        }

        let response: DeviceCodeResponse = response
            .json()
            .await
            .context("failed to parse device login code response")?;
        Ok(DeviceLogin {
            verification_url: format!("{}/codex/device", self.issuer),
            user_code: response.user_code,
            device_auth_id: response.device_auth_id,
            interval_seconds: response.interval_seconds,
        })
    }

    /// Polls for authorization, exchanges the code for tokens, and stores them.
    ///
    /// # Errors
    /// Returns an error when polling times out, exchange fails, or tokens are invalid.
    pub(crate) async fn complete_device_login(
        &self,
        login: DeviceLogin,
    ) -> Result<AuthCredentials> {
        let authorization = self.poll_device_login(&login).await?;
        let tokens = self.exchange_authorization_code(authorization).await?;
        let account_id = account_id_from_id_token(&tokens.id_token)?
            .context("login response did not include a ChatGPT account id")?;
        let auth_file = AuthFile::new(StoredTokens {
            id_token: tokens.id_token,
            access_token: tokens.access_token,
            refresh_token: tokens.refresh_token,
            account_id: Some(account_id),
        });
        ensure_access_token_is_usable(&auth_file.tokens.access_token)?;
        let credentials = auth_file.credentials()?;
        self.storage.save(&auth_file).await?;
        Ok(credentials)
    }

    /// Returns the current local login status, refreshing stale tokens first.
    ///
    /// # Errors
    /// Returns an error when stored auth is corrupt or refresh fails.
    pub(crate) async fn status(&self) -> Result<AuthStatus> {
        let Some(auth_file) = self.storage.load().await? else {
            return Ok(AuthStatus::LoggedOut);
        };

        let auth_file = if auth_file.needs_refresh(now_unix()) {
            self.refresh_locked(false).await?;
            self.load_required().await?
        } else {
            auth_file
        };

        Ok(AuthStatus::LoggedIn {
            account_id: auth_file.credentials()?.account_id,
            expires_at_unix: access_token_expiration(&auth_file.tokens.access_token)?,
        })
    }

    /// Revokes the stored refresh token when possible and removes local auth.
    ///
    /// # Errors
    /// Returns an error only when local auth removal fails.
    pub(crate) async fn logout(&self) -> Result<LogoutResult> {
        let auth_file = match self.storage.load().await {
            Ok(auth_file) => auth_file,
            Err(error) => {
                let removed = self.storage.remove().await?;
                return Ok(LogoutResult::new(
                    removed,
                    Some(format!("could not read stored auth for revoke: {error}")),
                ));
            }
        };
        let mut revoke_error = None;
        if let Some(auth_file) = &auth_file {
            if let Err(error) = self.revoke_tokens(&auth_file.tokens).await {
                revoke_error = Some(error.to_string());
            }
        }
        let removed = self.storage.remove().await?;
        Ok(LogoutResult::new(removed, revoke_error))
    }

    async fn refresh_locked(&self, force: bool) -> Result<AuthCredentials> {
        let _guard = self.refresh_lock.lock().await;
        let mut auth_file = self.load_required().await?;
        if !force && !auth_file.needs_refresh(now_unix()) {
            return auth_file.credentials();
        }

        anyhow::ensure!(
            !auth_file.tokens.refresh_token.trim().is_empty(),
            "auth refresh token is missing; run `rust-agent login --device-code`"
        );

        let response = self
            .http
            .post(format!("{}/oauth/token", self.issuer))
            .json(&RefreshRequest {
                client_id: CLIENT_ID,
                grant_type: "refresh_token",
                refresh_token: &auth_file.tokens.refresh_token,
            })
            .send()
            .await
            .context("failed to refresh auth token")?;

        if !response.status().is_success() {
            let status = response.status();
            let body = error_response_body(response).await;
            anyhow::bail!(
                "auth refresh returned {status}: {}",
                truncate_for_error(&body)
            );
        }

        let response: RefreshResponse = response
            .json()
            .await
            .context("failed to parse auth refresh response")?;
        if let Some(id_token) = response.id_token {
            auth_file.tokens.account_id =
                account_id_from_id_token(&id_token)?.or(auth_file.tokens.account_id);
            auth_file.tokens.id_token = id_token;
        }
        if let Some(access_token) = response.access_token {
            auth_file.tokens.access_token = access_token;
        }
        if let Some(refresh_token) = response.refresh_token {
            auth_file.tokens.refresh_token = refresh_token;
        }
        auth_file.last_refresh_unix = Some(now_unix());
        ensure_access_token_is_usable(&auth_file.tokens.access_token)?;

        let credentials = auth_file.credentials()?;
        self.storage.save(&auth_file).await?;
        Ok(credentials)
    }

    async fn poll_device_login(&self, login: &DeviceLogin) -> Result<DeviceAuthorization> {
        let url = format!("{}/api/accounts/deviceauth/token", self.issuer);
        let started = Instant::now();
        let poll_interval = Duration::from_secs(login.interval_seconds.max(1));

        loop {
            let response = self
                .http
                .post(&url)
                .json(&DeviceTokenRequest {
                    device_auth_id: &login.device_auth_id,
                    user_code: &login.user_code,
                })
                .send()
                .await
                .context("failed to poll device login")?;
            let status = response.status();
            if status.is_success() {
                return response
                    .json()
                    .await
                    .context("failed to parse device login authorization response");
            }
            if status == StatusCode::FORBIDDEN || status == StatusCode::NOT_FOUND {
                if started.elapsed() >= DEVICE_CODE_TIMEOUT {
                    anyhow::bail!("device login timed out after 15 minutes");
                }
                let remaining = DEVICE_CODE_TIMEOUT.saturating_sub(started.elapsed());
                tokio::time::sleep(poll_interval.min(remaining)).await;
                continue;
            }

            let body = error_response_body(response).await;
            anyhow::bail!(
                "device login polling returned {status}: {}",
                truncate_for_error(&body)
            );
        }
    }

    async fn exchange_authorization_code(
        &self,
        authorization: DeviceAuthorization,
    ) -> Result<TokenExchangeResponse> {
        let redirect_uri = format!("{}/deviceauth/callback", self.issuer);
        let body = form_body(&[
            ("grant_type", "authorization_code"),
            ("code", &authorization.authorization_code),
            ("redirect_uri", &redirect_uri),
            ("client_id", CLIENT_ID),
            ("code_verifier", &authorization.code_verifier),
        ]);
        let response = self
            .http
            .post(format!("{}/oauth/token", self.issuer))
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(body)
            .send()
            .await
            .context("failed to exchange device authorization code")?;

        if !response.status().is_success() {
            let status = response.status();
            let body = error_response_body(response).await;
            anyhow::bail!(
                "device authorization code exchange returned {status}: {}",
                truncate_for_error(&body)
            );
        }

        response
            .json()
            .await
            .context("failed to parse device authorization token response")
    }

    async fn revoke_tokens(&self, tokens: &StoredTokens) -> Result<()> {
        if !tokens.refresh_token.trim().is_empty() {
            return self
                .revoke_token(&tokens.refresh_token, "refresh_token", Some(CLIENT_ID))
                .await;
        }
        if !tokens.access_token.trim().is_empty() {
            return self
                .revoke_token(&tokens.access_token, "access_token", None)
                .await;
        }
        Ok(())
    }

    async fn revoke_token(
        &self,
        token: &str,
        token_type_hint: &'static str,
        client_id: Option<&'static str>,
    ) -> Result<()> {
        let response = self
            .http
            .post(format!("{}/oauth/revoke", self.issuer))
            .json(&RevokeRequest {
                token,
                token_type_hint,
                client_id,
            })
            .send()
            .await
            .context("failed to revoke auth token")?;

        if !response.status().is_success() {
            let status = response.status();
            let body = error_response_body(response).await;
            anyhow::bail!(
                "auth revoke returned {status}: {}",
                truncate_for_error(&body)
            );
        }
        Ok(())
    }

    async fn load_required(&self) -> Result<AuthFile> {
        self.storage
            .load()
            .await?
            .context("not logged in; run `rust-agent login --device-code`")
    }
}

#[derive(Debug, Serialize)]
struct DeviceCodeRequest<'a> {
    client_id: &'a str,
}

#[derive(Debug, Deserialize)]
struct DeviceCodeResponse {
    device_auth_id: String,
    #[serde(alias = "usercode")]
    user_code: String,
    #[serde(
        rename = "interval",
        default,
        deserialize_with = "deserialize_interval_seconds"
    )]
    interval_seconds: u64,
}

#[derive(Debug, Serialize)]
struct DeviceTokenRequest<'a> {
    device_auth_id: &'a str,
    user_code: &'a str,
}

#[derive(Debug, Deserialize)]
struct DeviceAuthorization {
    authorization_code: String,
    #[allow(dead_code)]
    code_challenge: String,
    code_verifier: String,
}

#[derive(Debug, Serialize)]
struct RefreshRequest<'a> {
    client_id: &'a str,
    grant_type: &'a str,
    refresh_token: &'a str,
}

#[derive(Debug, Deserialize)]
struct RefreshResponse {
    id_token: Option<String>,
    access_token: Option<String>,
    refresh_token: Option<String>,
}

#[derive(Debug, Serialize)]
struct RevokeRequest<'a> {
    token: &'a str,
    token_type_hint: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    client_id: Option<&'static str>,
}

#[derive(Debug, Deserialize)]
struct TokenExchangeResponse {
    id_token: String,
    access_token: String,
    refresh_token: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
struct AuthFile {
    tokens: StoredTokens,
    #[serde(default)]
    last_refresh_unix: Option<u64>,
}

impl AuthFile {
    fn new(tokens: StoredTokens) -> Self {
        Self {
            tokens,
            last_refresh_unix: Some(now_unix()),
        }
    }

    fn credentials(&self) -> Result<AuthCredentials> {
        anyhow::ensure!(
            !self.tokens.access_token.trim().is_empty(),
            "auth access token is missing; run `rust-agent login --device-code`"
        );
        let account_id = match &self.tokens.account_id {
            Some(account_id) if !account_id.trim().is_empty() => account_id.clone(),
            _ => account_id_from_id_token(&self.tokens.id_token)?
                .context("auth account id is missing; run `rust-agent login --device-code`")?,
        };
        Ok(AuthCredentials::new(
            self.tokens.access_token.clone(),
            account_id,
        ))
    }

    fn needs_refresh(&self, now_unix: u64) -> bool {
        if access_token_expires_soon(&self.tokens.access_token, now_unix).unwrap_or(true) {
            return true;
        }
        self.last_refresh_unix.is_none_or(|last_refresh| {
            now_unix.saturating_sub(last_refresh) > MAX_REFRESH_AGE_SECONDS
        })
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
struct StoredTokens {
    id_token: String,
    access_token: String,
    refresh_token: String,
    #[serde(default)]
    account_id: Option<String>,
}

#[derive(Clone, Debug)]
struct AuthStorage {
    path: PathBuf,
}

impl AuthStorage {
    fn new(path: PathBuf) -> Self {
        Self { path }
    }

    async fn load(&self) -> Result<Option<AuthFile>> {
        match tokio::fs::read(&self.path).await {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .with_context(|| format!("failed to parse auth file {}", self.path.display()))
                .map(Some),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error)
                .with_context(|| format!("failed to read auth file {}", self.path.display())),
        }
    }

    async fn save(&self, auth_file: &AuthFile) -> Result<()> {
        let parent = self
            .path
            .parent()
            .context("auth file path has no parent directory")?;
        tokio::fs::create_dir_all(parent)
            .await
            .with_context(|| format!("failed to create auth directory {}", parent.display()))?;
        let temp_path = self.path.with_extension("json.tmp");
        let bytes =
            serde_json::to_vec_pretty(auth_file).context("failed to serialize auth file")?;
        write_private_file(&temp_path, &bytes)
            .with_context(|| format!("failed to write auth file {}", temp_path.display()))?;
        tokio::fs::rename(&temp_path, &self.path)
            .await
            .with_context(|| format!("failed to replace auth file {}", self.path.display()))
    }

    async fn remove(&self) -> Result<bool> {
        match tokio::fs::remove_file(&self.path).await {
            Ok(()) => Ok(true),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error)
                .with_context(|| format!("failed to remove auth file {}", self.path.display())),
        }
    }
}

fn default_auth_file() -> Result<PathBuf> {
    if let Some(home) = std::env::var_os("RUST_AGENT_HOME") {
        return Ok(PathBuf::from(home).join("auth.json"));
    }
    let home = std::env::var_os("HOME").context("HOME is not set")?;
    Ok(PathBuf::from(home).join(".rust-agent").join("auth.json"))
}

#[cfg(unix)]
fn write_private_file(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;

    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

#[cfg(not(unix))]
fn write_private_file(path: &Path, bytes: &[u8]) -> Result<()> {
    std::fs::write(path, bytes)?;
    Ok(())
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn access_token_expires_soon(access_token: &str, now_unix: u64) -> Result<bool> {
    Ok(access_token_expiration(access_token)?
        <= now_unix.saturating_add(ACCESS_TOKEN_REFRESH_BUFFER_SECONDS))
}

fn ensure_access_token_is_usable(access_token: &str) -> Result<()> {
    anyhow::ensure!(
        !access_token_expires_soon(access_token, now_unix()).unwrap_or(true),
        "auth access token is expired or invalid; run `rust-agent login --device-code`"
    );
    Ok(())
}

fn access_token_expiration(access_token: &str) -> Result<u64> {
    let claims: ExpirationClaims = parse_jwt_payload(access_token)?;
    claims
        .exp
        .context("auth access token does not include an expiration")
}

fn account_id_from_id_token(id_token: &str) -> Result<Option<String>> {
    let claims: IdTokenClaims = parse_jwt_payload(id_token)?;
    Ok(claims.auth.and_then(|auth| auth.chatgpt_account_id))
}

fn parse_jwt_payload<T: DeserializeOwned>(jwt: &str) -> Result<T> {
    let mut parts = jwt.split('.');
    let header = parts.next();
    let payload = parts.next();
    let signature = parts.next();
    anyhow::ensure!(
        header.is_some() && payload.is_some() && signature.is_some() && parts.next().is_none(),
        "invalid JWT format"
    );
    let payload = payload.context("invalid JWT format")?;
    anyhow::ensure!(!payload.is_empty(), "invalid JWT payload");
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .or_else(|_| base64::engine::general_purpose::URL_SAFE.decode(payload))
        .context("failed to decode JWT payload")?;
    serde_json::from_slice(&bytes).context("failed to parse JWT payload")
}

#[derive(Debug, Deserialize)]
struct ExpirationClaims {
    exp: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct IdTokenClaims {
    #[serde(rename = "https://api.openai.com/auth")]
    auth: Option<AuthClaims>,
}

#[derive(Debug, Deserialize)]
struct AuthClaims {
    chatgpt_account_id: Option<String>,
}

fn deserialize_interval_seconds<'de, D>(deserializer: D) -> std::result::Result<u64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Value::deserialize(deserializer)?;
    match value {
        Value::Number(number) => number
            .as_u64()
            .ok_or_else(|| serde::de::Error::custom("interval must be a positive integer")),
        Value::String(value) => value
            .trim()
            .parse::<u64>()
            .map_err(serde::de::Error::custom),
        Value::Null => Ok(0),
        _ => Err(serde::de::Error::custom(
            "interval must be a string or integer",
        )),
    }
}

fn form_body(fields: &[(&str, &str)]) -> String {
    let mut body = String::new();
    for (index, (key, value)) in fields.iter().enumerate() {
        if index > 0 {
            body.push('&');
        }
        form_encode_into(&mut body, key);
        body.push('=');
        form_encode_into(&mut body, value);
    }
    body
}

fn form_encode_into(output: &mut String, value: &str) {
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                output.push(byte as char);
            }
            b' ' => output.push('+'),
            _ => {
                let _ = write!(output, "%{byte:02X}");
            }
        }
    }
}

async fn error_response_body(response: reqwest::Response) -> String {
    response.text().await.unwrap_or_else(|_| String::new())
}

fn truncate_for_error(value: &str) -> String {
    const LIMIT: usize = 500;
    let trimmed = value.trim();
    if trimmed.len() <= LIMIT {
        trimmed.to_string()
    } else {
        let cutoff = trimmed
            .char_indices()
            .map(|(index, _)| index)
            .take_while(|index| *index <= LIMIT)
            .last()
            .unwrap_or(0);
        format!("{}...", &trimmed[..cutoff])
    }
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    use super::*;
    use crate::test_http::TestResponse;
    use crate::test_http::TestServer;

    #[test]
    fn reads_expiration_from_access_token() {
        let token = jwt(&serde_json::json!({"exp": 1_700_000_000_u64}));

        assert_eq!(access_token_expiration(&token).unwrap(), 1_700_000_000);
    }

    #[test]
    fn reads_account_id_from_id_token() {
        let token = id_token("account-a");

        assert_eq!(
            account_id_from_id_token(&token).unwrap().as_deref(),
            Some("account-a")
        );
    }

    #[test]
    fn encodes_form_body() {
        assert_eq!(
            form_body(&[("redirect_uri", "http://127.0.0.1/a b"), ("code", "a+b")]),
            "redirect_uri=http%3A%2F%2F127.0.0.1%2Fa+b&code=a%2Bb"
        );
    }

    #[tokio::test]
    async fn saves_loads_and_removes_private_auth_file() {
        let auth_path = temp_auth_file("storage");
        let storage = AuthStorage::new(auth_path.clone());
        let auth_file = AuthFile::new(tokens_with_exp("account-a", now_unix() + 600, "refresh"));

        storage.save(&auth_file).await.unwrap();

        #[cfg(unix)]
        assert_eq!(
            std::fs::metadata(&auth_path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(storage.load().await.unwrap(), Some(auth_file));
        assert!(storage.remove().await.unwrap());
        assert_eq!(storage.load().await.unwrap(), None);

        remove_parent(&auth_path);
    }

    #[tokio::test]
    async fn refreshes_expired_access_token() {
        let auth_path = temp_auth_file("refresh");
        let storage = AuthStorage::new(auth_path.clone());
        storage
            .save(&AuthFile {
                tokens: tokens_with_exp("account-old", now_unix() - 60, "refresh-old"),
                last_refresh_unix: Some(now_unix()),
            })
            .await
            .unwrap();
        let refreshed_access = access_token(now_unix() + 600);
        let refreshed_id = id_token("account-new");
        let server = TestServer::new(vec![TestResponse::json(
            200,
            serde_json::json!({
                "id_token": refreshed_id,
                "access_token": refreshed_access,
                "refresh_token": "refresh-new"
            })
            .to_string(),
        )]);
        let auth = AuthManager::for_test(auth_path.clone(), server.url()).unwrap();

        let credentials = auth.credentials().await.unwrap();

        assert_eq!(credentials.account_id(), "account-new");
        assert_eq!(credentials.access_token(), refreshed_access);
        let saved = storage.load().await.unwrap().unwrap();
        assert_eq!(saved.tokens.refresh_token, "refresh-new");
        assert_eq!(saved.tokens.account_id.as_deref(), Some("account-new"));
        let requests = server.requests();
        assert_eq!(requests[0].path, "/oauth/token");
        assert!(requests[0].body.contains(r#""grant_type":"refresh_token""#));
        assert!(requests[0]
            .body
            .contains(r#""refresh_token":"refresh-old""#));

        remove_parent(&auth_path);
    }

    #[tokio::test]
    async fn device_code_login_persists_tokens() {
        let auth_path = temp_auth_file("device");
        let access = access_token(now_unix() + 600);
        let id = id_token("account-device");
        let server = TestServer::new(vec![
            TestResponse::json(
                200,
                r#"{"device_auth_id":"device-1","user_code":"ABCD-EFGH","interval":"0"}"#,
            ),
            TestResponse::json(
                200,
                r#"{"authorization_code":"auth-code","code_challenge":"challenge","code_verifier":"verifier"}"#,
            ),
            TestResponse::json(
                200,
                serde_json::json!({
                    "id_token": id,
                    "access_token": access,
                    "refresh_token": "refresh-device"
                })
                .to_string(),
            ),
        ]);
        let auth = AuthManager::for_test(auth_path.clone(), server.url()).unwrap();

        let login = auth.start_device_login().await.unwrap();
        assert_eq!(
            login.verification_url(),
            format!("{}/codex/device", server.url())
        );
        assert_eq!(login.user_code(), "ABCD-EFGH");
        assert_eq!(login.interval_seconds, 0);
        let credentials = auth.complete_device_login(login).await.unwrap();

        assert_eq!(credentials.account_id(), "account-device");
        let saved = AuthStorage::new(auth_path.clone())
            .load()
            .await
            .unwrap()
            .unwrap();
        assert_eq!(saved.tokens.account_id.as_deref(), Some("account-device"));
        let requests = server.requests();
        assert_eq!(requests[0].path, "/api/accounts/deviceauth/usercode");
        assert_eq!(requests[1].path, "/api/accounts/deviceauth/token");
        assert_eq!(requests[2].path, "/oauth/token");
        assert!(requests[2].body.contains("grant_type=authorization_code"));
        assert!(requests[2].body.contains("code_verifier=verifier"));

        remove_parent(&auth_path);
    }

    #[tokio::test]
    async fn logout_removes_auth_when_revoke_fails() {
        let auth_path = temp_auth_file("logout");
        let storage = AuthStorage::new(auth_path.clone());
        storage
            .save(&AuthFile::new(tokens_with_exp(
                "account-a",
                now_unix() + 600,
                "refresh-a",
            )))
            .await
            .unwrap();
        let server = TestServer::new(vec![TestResponse::json(500, r#"{"error":"nope"}"#)]);
        let auth = AuthManager::for_test(auth_path.clone(), server.url()).unwrap();

        let result = auth.logout().await.unwrap();

        assert!(result.removed());
        assert!(result
            .revoke_error()
            .unwrap()
            .contains("auth revoke returned"));
        assert_eq!(storage.load().await.unwrap(), None);
        assert_eq!(server.requests()[0].path, "/oauth/revoke");

        remove_parent(&auth_path);
    }

    #[tokio::test]
    async fn logout_removes_corrupt_auth_file() {
        let auth_path = temp_auth_file("logout-corrupt");
        std::fs::create_dir_all(auth_path.parent().unwrap()).unwrap();
        std::fs::write(&auth_path, b"not-json").unwrap();
        let auth =
            AuthManager::for_test(auth_path.clone(), "http://127.0.0.1:9".to_string()).unwrap();

        let result = auth.logout().await.unwrap();

        assert!(result.removed());
        assert!(result
            .revoke_error()
            .unwrap()
            .contains("could not read stored auth for revoke"));
        assert!(!auth_path.exists());

        remove_parent(&auth_path);
    }

    fn tokens_with_exp(account_id: &str, exp: u64, refresh_token: &str) -> StoredTokens {
        StoredTokens {
            id_token: id_token(account_id),
            access_token: access_token(exp),
            refresh_token: refresh_token.to_string(),
            account_id: Some(account_id.to_string()),
        }
    }

    fn id_token(account_id: &str) -> String {
        jwt(&serde_json::json!({
            "https://api.openai.com/auth": {
                "chatgpt_account_id": account_id
            }
        }))
    }

    fn access_token(exp: u64) -> String {
        jwt(&serde_json::json!({ "exp": exp }))
    }

    fn jwt(payload: &Value) -> String {
        let header = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(r#"{"alg":"none"}"#);
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(serde_json::to_vec(payload).unwrap());
        format!("{header}.{payload}.signature")
    }

    fn temp_auth_file(name: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir()
            .join(format!(
                "rust-agent-auth-{name}-{}-{unique}",
                std::process::id()
            ))
            .join("auth.json")
    }

    fn remove_parent(path: &Path) {
        if let Some(parent) = path.parent() {
            let _ = std::fs::remove_dir_all(parent);
        }
    }
}
