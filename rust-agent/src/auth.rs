//! Local Codex OAuth credential loading.

use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use anyhow::Context;
use anyhow::Result;
use base64::engine::general_purpose::URL_SAFE;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use serde::Deserialize;

const EXPIRATION_BUFFER_SECONDS: u64 = 60;

/// OAuth credentials needed by the ChatGPT Codex backend.
#[derive(Clone)]
pub(crate) struct CodexAuth {
    access_token: String,
    account_id: String,
}

impl CodexAuth {
    /// Loads OAuth credentials from the default Codex auth file.
    ///
    /// # Errors
    /// Returns an error if `$HOME/.codex/auth.json` is missing or incomplete.
    pub(crate) fn load_default() -> Result<Self> {
        let home = std::env::var_os("HOME").context("HOME is not set")?;
        let path = PathBuf::from(home).join(".codex").join("auth.json");
        Self::load_from(path)
    }

    /// Loads OAuth credentials from a Codex auth file.
    ///
    /// # Errors
    /// Returns an error if the file cannot be read, parsed, or lacks tokens.
    pub(crate) fn load_from(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let raw = fs::read_to_string(path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        let auth_file: AuthFile = serde_json::from_str(&raw)
            .with_context(|| format!("failed to parse {}", path.display()))?;
        let tokens = auth_file
            .tokens
            .context("Codex auth file does not contain OAuth tokens")?;
        let account_id = tokens
            .account_id
            .context("Codex auth file does not contain a ChatGPT account id")?;

        anyhow::ensure!(
            !tokens.access_token.trim().is_empty(),
            "access token is empty"
        );
        anyhow::ensure!(!account_id.trim().is_empty(), "account id is empty");
        reject_expired_access_token(&tokens.access_token)?;

        Ok(Self {
            access_token: tokens.access_token,
            account_id,
        })
    }

    pub(crate) fn access_token(&self) -> &str {
        &self.access_token
    }

    pub(crate) fn account_id(&self) -> &str {
        &self.account_id
    }
}

#[derive(Debug, Deserialize)]
struct AuthFile {
    tokens: Option<TokenData>,
}

#[derive(Debug, Deserialize)]
struct TokenData {
    access_token: String,
    account_id: Option<String>,
}

fn reject_expired_access_token(access_token: &str) -> Result<()> {
    let expires_at = access_token_expiration(access_token)?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before Unix epoch")?
        .as_secs();

    anyhow::ensure!(
        expires_at > now.saturating_add(EXPIRATION_BUFFER_SECONDS),
        "Codex OAuth access token is expired or expires too soon; run `codex login status` to refresh it, or `codex login` if login status fails"
    );

    Ok(())
}

fn access_token_expiration(access_token: &str) -> Result<u64> {
    let payload = access_token
        .split('.')
        .nth(1)
        .context("Codex OAuth access token is not a JWT; run `codex login` to refresh auth")?;
    let bytes = URL_SAFE_NO_PAD
        .decode(payload)
        .or_else(|_| URL_SAFE.decode(payload))
        .context("failed to decode Codex OAuth access token; run `codex login` to refresh auth")?;
    let claims: JwtClaims = serde_json::from_slice(&bytes)
        .context("failed to parse Codex OAuth access token; run `codex login` to refresh auth")?;

    Ok(claims.exp)
}

#[derive(Debug, Deserialize)]
struct JwtClaims {
    exp: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine;

    #[test]
    fn reads_expiration_from_access_token() {
        let token = jwt_with_exp(1_900_000_000);

        assert_eq!(access_token_expiration(&token).unwrap(), 1_900_000_000);
    }

    #[test]
    fn rejects_expired_access_token() {
        let token = jwt_with_exp(1);
        let error = reject_expired_access_token(&token).unwrap_err().to_string();

        assert!(error.contains("codex login status"));
    }

    fn jwt_with_exp(exp: u64) -> String {
        let header = URL_SAFE_NO_PAD.encode(r#"{"alg":"none"}"#);
        let payload = URL_SAFE_NO_PAD.encode(format!(r#"{{"exp":{exp}}}"#));
        format!("{header}.{payload}.")
    }
}
