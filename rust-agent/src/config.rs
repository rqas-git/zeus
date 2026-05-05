//! Runtime configuration for the agent harness.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::Result;
use serde::Deserialize;

use crate::storage::default_database_path;
use crate::tools::ToolPolicy;
use crate::tools::DEFAULT_FFF_SEARCH_CONCURRENCY;
use crate::tools::MAX_FFF_SEARCH_CONCURRENCY;

const DEFAULT_CODEX_RESPONSES_URL: &str = "https://chatgpt.com/backend-api/codex/responses";
const DEFAULT_CODEX_ORIGINATOR: &str = "codex_cli_rs";
const DEFAULT_CODEX_VERSION: &str = "0.128.0";
const DEFAULT_MODEL: &str = "gpt-5.5";
const DEFAULT_ALLOWED_MODELS: &[&str] = &["gpt-5.5", "gpt-5.4", "gpt-5.4-mini"];
const DEFAULT_REASONING_EFFORT: &str = "medium";
const DEFAULT_REASONING_EFFORTS: &[&str] = &["low", "medium", "high", "xhigh"];
const CODEX_MODELS_CACHE_FILE: &str = "models_cache.json";
const DEFAULT_INSTRUCTIONS: &str = "You are a concise assistant.";
const DEFAULT_REQUEST_TIMEOUT_SECS: u64 = 120;
const DEFAULT_CONTEXT_MAX_MESSAGES: usize = 40;
const DEFAULT_CONTEXT_MAX_BYTES: usize = 64 * 1024;
const DEFAULT_HISTORY_MAX_MESSAGES: usize = 200;
const DEFAULT_HISTORY_MAX_BYTES: usize = 256 * 1024;
const DEFAULT_DELTA_FLUSH_INTERVAL_MS: u64 = 16;
const DEFAULT_DELTA_FLUSH_BYTES: usize = 4096;
const DEFAULT_CACHE_HEALTH_TELEMETRY: bool = false;
const DEFAULT_SERVER_HTTP_ADDR: &str = "127.0.0.1:4096";
const DEFAULT_SERVER_H3_ADDR: &str = "127.0.0.1:4433";
const DEFAULT_SERVER_EVENT_QUEUE_CAPACITY: usize = 1024;
const DEFAULT_SERVER_MAX_SESSIONS: usize = 128;
const DEFAULT_SERVER_MAX_EVENT_CHANNELS: usize = 128;
const DEFAULT_SERVER_H3_MAX_CONCURRENT_STREAMS: u32 = 256;
const DEFAULT_SERVER_H3_IDLE_TIMEOUT_SECS: u64 = 60;
const DEFAULT_TOOL_MODE: &str = "read-only";

/// Configuration for one running rust-agent process.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct AppConfig {
    pub(crate) client: ClientConfig,
    pub(crate) context: ContextWindowConfig,
    pub(crate) models: ModelConfig,
    pub(crate) output: OutputConfig,
    pub(crate) server: ServerConfig,
    pub(crate) storage: StorageConfig,
    pub(crate) telemetry: TelemetryConfig,
    pub(crate) tools: ToolConfig,
}

impl AppConfig {
    /// Loads configuration from environment variables.
    ///
    /// # Errors
    /// Returns an error if a numeric environment variable is invalid.
    pub(crate) fn from_env() -> Result<Self> {
        Ok(Self {
            client: ClientConfig::from_env()?,
            context: ContextWindowConfig::from_env()?,
            models: ModelConfig::from_env()?,
            output: OutputConfig::from_env()?,
            server: ServerConfig::from_env()?,
            storage: StorageConfig::from_env()?,
            telemetry: TelemetryConfig::from_env()?,
            tools: ToolConfig::from_env()?,
        })
    }
}

/// Configuration for ChatGPT Codex backend requests.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ClientConfig {
    instructions: String,
    responses_url: String,
    originator: String,
    version: String,
    request_timeout: Duration,
    prompt_cache_namespace: String,
}

impl ClientConfig {
    /// Loads client configuration from environment variables.
    ///
    /// # Errors
    /// Returns an error if `RUST_AGENT_REQUEST_TIMEOUT_SECS` is invalid.
    pub(crate) fn from_env() -> Result<Self> {
        let request_timeout_secs = env_parse_u64(
            "RUST_AGENT_REQUEST_TIMEOUT_SECS",
            DEFAULT_REQUEST_TIMEOUT_SECS,
        )?;

        Ok(Self {
            instructions: env_string("RUST_AGENT_INSTRUCTIONS", DEFAULT_INSTRUCTIONS),
            responses_url: env_string("RUST_AGENT_RESPONSES_URL", DEFAULT_CODEX_RESPONSES_URL),
            originator: env_string("RUST_AGENT_ORIGINATOR", DEFAULT_CODEX_ORIGINATOR),
            version: env_string("RUST_AGENT_VERSION", DEFAULT_CODEX_VERSION),
            request_timeout: Duration::from_secs(request_timeout_secs),
            prompt_cache_namespace: env_string("RUST_AGENT_PROMPT_CACHE_NAMESPACE", "rust-agent"),
        })
    }

    /// Creates client configuration with explicit values.
    #[cfg(test)]
    pub(crate) fn new(
        instructions: impl Into<String>,
        responses_url: impl Into<String>,
        originator: impl Into<String>,
        version: impl Into<String>,
        request_timeout: Duration,
        prompt_cache_namespace: impl Into<String>,
    ) -> Self {
        Self {
            instructions: instructions.into(),
            responses_url: responses_url.into(),
            originator: originator.into(),
            version: version.into(),
            request_timeout,
            prompt_cache_namespace: prompt_cache_namespace.into(),
        }
    }

    /// Returns the configured assistant instructions.
    pub(crate) fn instructions(&self) -> &str {
        &self.instructions
    }

    /// Returns the ChatGPT Codex Responses endpoint.
    pub(crate) fn responses_url(&self) -> &str {
        &self.responses_url
    }

    /// Returns the Codex originator header value.
    pub(crate) fn originator(&self) -> &str {
        &self.originator
    }

    /// Returns the Codex version header value.
    pub(crate) fn version(&self) -> &str {
        &self.version
    }

    /// Returns the total request timeout.
    pub(crate) fn request_timeout(&self) -> Duration {
        self.request_timeout
    }

    /// Builds a stable prompt-cache key for a session.
    pub(crate) fn prompt_cache_key(&self, session_id: u64, model: &str) -> String {
        format!("{}-{session_id}-{model}", self.prompt_cache_namespace)
    }
}

impl Default for ClientConfig {
    fn default() -> Self {
        Self {
            instructions: DEFAULT_INSTRUCTIONS.to_string(),
            responses_url: DEFAULT_CODEX_RESPONSES_URL.to_string(),
            originator: DEFAULT_CODEX_ORIGINATOR.to_string(),
            version: DEFAULT_CODEX_VERSION.to_string(),
            request_timeout: Duration::from_secs(DEFAULT_REQUEST_TIMEOUT_SECS),
            prompt_cache_namespace: "rust-agent".to_string(),
        }
    }
}

/// Model defaults and allowlist enforced by the backend service.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ModelConfig {
    default_model: String,
    allowed_models: Vec<String>,
    default_reasoning_effort: String,
    reasoning_efforts: Vec<String>,
}

impl ModelConfig {
    /// Loads model configuration from environment variables.
    ///
    /// # Errors
    /// Returns an error if no allowed models are configured or the default is not allowed.
    pub(crate) fn from_env() -> Result<Self> {
        let default_model = normalized_model(env_string("RUST_AGENT_MODEL", DEFAULT_MODEL))?;
        let allowed_models = match env_model_list("RUST_AGENT_ALLOWED_MODELS") {
            Some(models) => {
                let models = normalized_model_list(models)?;
                anyhow::ensure!(
                    models.iter().any(|model| model == &default_model),
                    "RUST_AGENT_MODEL={default_model:?} is not in RUST_AGENT_ALLOWED_MODELS"
                );
                models
            }
            None => {
                let mut models =
                    load_codex_model_allowlist().unwrap_or_else(default_allowed_models);
                if !models.iter().any(|model| model == &default_model) {
                    models.push(default_model.clone());
                }
                models
            }
        };
        let (default_reasoning_effort, reasoning_efforts) =
            load_codex_reasoning_config(&allowed_models, &default_model)
                .unwrap_or_else(default_reasoning_config);

        Ok(Self {
            default_model,
            allowed_models,
            default_reasoning_effort,
            reasoning_efforts,
        })
    }

    /// Creates model configuration with explicit values.
    #[cfg(test)]
    pub(crate) fn new(
        default_model: impl Into<String>,
        allowed_models: impl IntoIterator<Item = impl Into<String>>,
    ) -> Result<Self> {
        let default_model = normalized_model(default_model)?;
        let allowed_models = normalized_model_list(allowed_models)?;
        anyhow::ensure!(
            allowed_models.iter().any(|model| model == &default_model),
            "default model {default_model:?} is not in allowed models"
        );
        Ok(Self {
            default_model,
            allowed_models,
            default_reasoning_effort: DEFAULT_REASONING_EFFORT.to_string(),
            reasoning_efforts: default_reasoning_efforts(),
        })
    }

    /// Returns the default model slug for new sessions.
    pub(crate) fn default_model(&self) -> &str {
        &self.default_model
    }

    /// Returns the backend allowlist for model changes.
    pub(crate) fn allowed_models(&self) -> &[String] {
        &self.allowed_models
    }

    /// Returns the default reasoning effort for new turns.
    pub(crate) fn default_reasoning_effort(&self) -> &str {
        &self.default_reasoning_effort
    }

    /// Returns reasoning efforts advertised by the Codex model cache.
    pub(crate) fn reasoning_efforts(&self) -> &[String] {
        &self.reasoning_efforts
    }

    /// Returns the canonical allowed model matching a requested slug.
    ///
    /// # Errors
    /// Returns an error if the model is empty or not in the allowlist.
    pub(crate) fn allowed_model(&self, model: &str) -> Result<&str> {
        let requested = model.trim();
        anyhow::ensure!(!requested.is_empty(), "model cannot be empty");

        self.allowed_models
            .iter()
            .find(|candidate| candidate.as_str() == requested)
            .map(String::as_str)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "unsupported model {requested:?}; allowed models: {}",
                    self.allowed_models.join(", ")
                )
            })
    }

    /// Returns the canonical allowed reasoning effort matching a requested value.
    ///
    /// # Errors
    /// Returns an error if the effort is empty or unsupported.
    pub(crate) fn allowed_reasoning_effort(&self, effort: &str) -> Result<&str> {
        let requested = effort.trim();
        anyhow::ensure!(!requested.is_empty(), "reasoning effort cannot be empty");

        self.reasoning_efforts
            .iter()
            .find(|candidate| candidate.as_str() == requested)
            .map(String::as_str)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "unsupported reasoning effort {requested:?}; allowed reasoning efforts: {}",
                    self.reasoning_efforts.join(", ")
                )
            })
    }
}

impl Default for ModelConfig {
    fn default() -> Self {
        Self {
            default_model: DEFAULT_MODEL.to_string(),
            allowed_models: default_allowed_models(),
            default_reasoning_effort: DEFAULT_REASONING_EFFORT.to_string(),
            reasoning_efforts: default_reasoning_efforts(),
        }
    }
}

/// Bounds applied before sending history to the model.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ContextWindowConfig {
    max_messages: usize,
    max_bytes: usize,
    history_max_messages: usize,
    history_max_bytes: usize,
}

impl ContextWindowConfig {
    /// Loads context-window configuration from environment variables.
    ///
    /// # Errors
    /// Returns an error if a numeric environment variable is invalid.
    pub(crate) fn from_env() -> Result<Self> {
        Ok(Self {
            max_messages: env_parse_usize(
                "RUST_AGENT_CONTEXT_MAX_MESSAGES",
                DEFAULT_CONTEXT_MAX_MESSAGES,
            )?
            .max(1),
            max_bytes: env_parse_usize("RUST_AGENT_CONTEXT_MAX_BYTES", DEFAULT_CONTEXT_MAX_BYTES)?
                .max(1),
            history_max_messages: env_parse_usize(
                "RUST_AGENT_HISTORY_MAX_MESSAGES",
                DEFAULT_HISTORY_MAX_MESSAGES,
            )?
            .max(1),
            history_max_bytes: env_parse_usize(
                "RUST_AGENT_HISTORY_MAX_BYTES",
                DEFAULT_HISTORY_MAX_BYTES,
            )?
            .max(1),
        })
    }

    /// Creates context-window bounds.
    #[cfg(test)]
    pub(crate) const fn new(max_messages: usize, max_bytes: usize) -> Self {
        Self {
            max_messages,
            max_bytes,
            history_max_messages: DEFAULT_HISTORY_MAX_MESSAGES,
            history_max_bytes: DEFAULT_HISTORY_MAX_BYTES,
        }
    }

    /// Creates context-window and history-retention bounds.
    #[cfg(test)]
    pub(crate) const fn with_history_limits(
        max_messages: usize,
        max_bytes: usize,
        history_max_messages: usize,
        history_max_bytes: usize,
    ) -> Self {
        Self {
            max_messages,
            max_bytes,
            history_max_messages,
            history_max_bytes,
        }
    }

    /// Returns the maximum number of messages sent to the model.
    pub(crate) const fn max_messages(self) -> usize {
        self.max_messages
    }

    /// Returns the approximate text byte budget sent to the model.
    pub(crate) const fn max_bytes(self) -> usize {
        self.max_bytes
    }

    /// Returns the maximum number of messages retained in session memory.
    pub(crate) const fn history_max_messages(self) -> usize {
        self.history_max_messages
    }

    /// Returns the approximate text byte budget retained in session memory.
    pub(crate) const fn history_max_bytes(self) -> usize {
        self.history_max_bytes
    }
}

impl Default for ContextWindowConfig {
    fn default() -> Self {
        Self {
            max_messages: DEFAULT_CONTEXT_MAX_MESSAGES,
            max_bytes: DEFAULT_CONTEXT_MAX_BYTES,
            history_max_messages: DEFAULT_HISTORY_MAX_MESSAGES,
            history_max_bytes: DEFAULT_HISTORY_MAX_BYTES,
        }
    }
}

/// Controls how streaming deltas are flushed to stdout.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct OutputConfig {
    delta_flush_interval: Duration,
    delta_flush_bytes: usize,
}

impl OutputConfig {
    /// Loads output configuration from environment variables.
    ///
    /// # Errors
    /// Returns an error if a numeric environment variable is invalid.
    pub(crate) fn from_env() -> Result<Self> {
        Ok(Self {
            delta_flush_interval: Duration::from_millis(env_parse_u64(
                "RUST_AGENT_DELTA_FLUSH_INTERVAL_MS",
                DEFAULT_DELTA_FLUSH_INTERVAL_MS,
            )?),
            delta_flush_bytes: env_parse_usize(
                "RUST_AGENT_DELTA_FLUSH_BYTES",
                DEFAULT_DELTA_FLUSH_BYTES,
            )?
            .max(1),
        })
    }

    /// Creates output buffering configuration.
    #[cfg(test)]
    pub(crate) const fn new(delta_flush_interval: Duration, delta_flush_bytes: usize) -> Self {
        Self {
            delta_flush_interval,
            delta_flush_bytes,
        }
    }

    /// Returns the maximum delay before buffered deltas are flushed.
    pub(crate) const fn delta_flush_interval(self) -> Duration {
        self.delta_flush_interval
    }

    /// Returns the buffered byte threshold for flushing deltas.
    pub(crate) const fn delta_flush_bytes(self) -> usize {
        self.delta_flush_bytes
    }
}

impl Default for OutputConfig {
    fn default() -> Self {
        Self {
            delta_flush_interval: Duration::from_millis(DEFAULT_DELTA_FLUSH_INTERVAL_MS),
            delta_flush_bytes: DEFAULT_DELTA_FLUSH_BYTES,
        }
    }
}

/// Network configuration for the local server mode.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ServerConfig {
    http_addr: SocketAddr,
    h3_addr: SocketAddr,
    tls_cert_path: Option<PathBuf>,
    tls_key_path: Option<PathBuf>,
    auth_token: Option<String>,
    event_queue_capacity: usize,
    max_sessions: usize,
    max_event_channels: usize,
    h3_max_concurrent_streams: u32,
    h3_idle_timeout: Duration,
    parent_pid: Option<libc::pid_t>,
}

impl ServerConfig {
    /// Loads local server configuration from environment variables.
    ///
    /// # Errors
    /// Returns an error if an address or numeric environment variable is invalid.
    pub(crate) fn from_env() -> Result<Self> {
        Ok(Self {
            http_addr: env_parse_socket_addr(
                "RUST_AGENT_SERVER_HTTP_ADDR",
                DEFAULT_SERVER_HTTP_ADDR,
            )?,
            h3_addr: env_parse_socket_addr("RUST_AGENT_SERVER_H3_ADDR", DEFAULT_SERVER_H3_ADDR)?,
            tls_cert_path: env_optional_path("RUST_AGENT_SERVER_TLS_CERT"),
            tls_key_path: env_optional_path("RUST_AGENT_SERVER_TLS_KEY"),
            auth_token: env_optional_string("RUST_AGENT_SERVER_TOKEN"),
            event_queue_capacity: env_parse_usize(
                "RUST_AGENT_SERVER_EVENT_QUEUE_CAPACITY",
                DEFAULT_SERVER_EVENT_QUEUE_CAPACITY,
            )?
            .max(1),
            max_sessions: env_parse_usize(
                "RUST_AGENT_SERVER_MAX_SESSIONS",
                DEFAULT_SERVER_MAX_SESSIONS,
            )?
            .max(1),
            max_event_channels: env_parse_usize(
                "RUST_AGENT_SERVER_MAX_EVENT_CHANNELS",
                DEFAULT_SERVER_MAX_EVENT_CHANNELS,
            )?
            .max(1),
            h3_max_concurrent_streams: env_parse_u32(
                "RUST_AGENT_SERVER_H3_MAX_CONCURRENT_STREAMS",
                DEFAULT_SERVER_H3_MAX_CONCURRENT_STREAMS,
            )?
            .max(1),
            h3_idle_timeout: Duration::from_secs(env_parse_u64(
                "RUST_AGENT_SERVER_H3_IDLE_TIMEOUT_SECS",
                DEFAULT_SERVER_H3_IDLE_TIMEOUT_SECS,
            )?),
            parent_pid: env_optional_parent_pid("RUST_AGENT_PARENT_PID")?,
        })
    }

    /// Creates local server configuration with explicit bind addresses.
    #[cfg(test)]
    pub(crate) fn new(http_addr: SocketAddr, h3_addr: SocketAddr) -> Self {
        Self {
            http_addr,
            h3_addr,
            tls_cert_path: None,
            tls_key_path: None,
            auth_token: Some("test-token".to_string()),
            event_queue_capacity: DEFAULT_SERVER_EVENT_QUEUE_CAPACITY,
            max_sessions: DEFAULT_SERVER_MAX_SESSIONS,
            max_event_channels: DEFAULT_SERVER_MAX_EVENT_CHANNELS,
            h3_max_concurrent_streams: DEFAULT_SERVER_H3_MAX_CONCURRENT_STREAMS,
            h3_idle_timeout: Duration::from_secs(DEFAULT_SERVER_H3_IDLE_TIMEOUT_SECS),
            parent_pid: None,
        }
    }

    /// Returns the plain HTTP compatibility listener address.
    pub(crate) const fn http_addr(&self) -> SocketAddr {
        self.http_addr
    }

    /// Returns the native HTTP/3 listener address.
    pub(crate) const fn h3_addr(&self) -> SocketAddr {
        self.h3_addr
    }

    /// Returns the configured TLS certificate path.
    pub(crate) fn tls_cert_path(&self) -> Option<&std::path::Path> {
        self.tls_cert_path.as_deref()
    }

    /// Returns the configured TLS private key path.
    pub(crate) fn tls_key_path(&self) -> Option<&std::path::Path> {
        self.tls_key_path.as_deref()
    }

    /// Returns the configured server bearer token, if one was supplied.
    pub(crate) fn auth_token(&self) -> Option<&str> {
        self.auth_token.as_deref()
    }

    /// Returns the bounded event queue capacity per session event stream.
    pub(crate) const fn event_queue_capacity(&self) -> usize {
        self.event_queue_capacity
    }

    /// Returns the maximum number of sessions retained by server mode.
    pub(crate) const fn max_sessions(&self) -> usize {
        self.max_sessions
    }

    /// Returns the maximum number of session event channels retained by server mode.
    pub(crate) const fn max_event_channels(&self) -> usize {
        self.max_event_channels
    }

    /// Returns the H3 concurrent bidirectional stream limit.
    pub(crate) const fn h3_max_concurrent_streams(&self) -> u32 {
        self.h3_max_concurrent_streams
    }

    /// Returns the QUIC idle timeout for H3 connections.
    pub(crate) const fn h3_idle_timeout(&self) -> Duration {
        self.h3_idle_timeout
    }

    /// Returns the optional parent process to watch for server shutdown.
    pub(crate) const fn parent_pid(&self) -> Option<libc::pid_t> {
        self.parent_pid
    }
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            http_addr: DEFAULT_SERVER_HTTP_ADDR
                .parse()
                .expect("default HTTP server address must parse"),
            h3_addr: DEFAULT_SERVER_H3_ADDR
                .parse()
                .expect("default H3 server address must parse"),
            tls_cert_path: None,
            tls_key_path: None,
            auth_token: None,
            event_queue_capacity: DEFAULT_SERVER_EVENT_QUEUE_CAPACITY,
            max_sessions: DEFAULT_SERVER_MAX_SESSIONS,
            max_event_channels: DEFAULT_SERVER_MAX_EVENT_CHANNELS,
            h3_max_concurrent_streams: DEFAULT_SERVER_H3_MAX_CONCURRENT_STREAMS,
            h3_idle_timeout: Duration::from_secs(DEFAULT_SERVER_H3_IDLE_TIMEOUT_SECS),
            parent_pid: None,
        }
    }
}

/// Local durable storage configuration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct StorageConfig {
    database_path: PathBuf,
}

impl StorageConfig {
    /// Loads durable storage configuration from environment variables.
    ///
    /// # Errors
    /// Returns an error when the default rust-agent home cannot be resolved.
    pub(crate) fn from_env() -> Result<Self> {
        Ok(Self {
            database_path: env_optional_path("RUST_AGENT_STATE_DB")
                .map(Ok)
                .unwrap_or_else(default_database_path)?,
        })
    }

    /// Returns the SQLite session database path.
    pub(crate) fn database_path(&self) -> &std::path::Path {
        &self.database_path
    }
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            database_path: default_database_path()
                .unwrap_or_else(|_| PathBuf::from(".rust-agent").join("sessions.db")),
        }
    }
}

/// Controls which built-in tools are exposed to the model.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ToolConfig {
    policy: ToolPolicy,
    search_concurrency: usize,
}

impl ToolConfig {
    /// Loads tool configuration from environment variables.
    ///
    /// # Errors
    /// Returns an error if `RUST_AGENT_TOOL_MODE` is unsupported.
    pub(crate) fn from_env() -> Result<Self> {
        let search_concurrency = env_parse_usize(
            "RUST_AGENT_TOOL_SEARCH_CONCURRENCY",
            DEFAULT_FFF_SEARCH_CONCURRENCY,
        )?;
        Ok(Self {
            policy: parse_tool_policy(&env_string("RUST_AGENT_TOOL_MODE", DEFAULT_TOOL_MODE))?,
            search_concurrency: validate_tool_search_concurrency(search_concurrency)?,
        })
    }

    /// Returns the active tool permission policy.
    pub(crate) const fn policy(self) -> ToolPolicy {
        self.policy
    }

    /// Returns the maximum number of concurrent FFF searches.
    pub(crate) const fn search_concurrency(self) -> usize {
        self.search_concurrency
    }
}

impl Default for ToolConfig {
    fn default() -> Self {
        Self {
            policy: ToolPolicy::ReadOnly,
            search_concurrency: DEFAULT_FFF_SEARCH_CONCURRENCY,
        }
    }
}

/// Controls optional terminal telemetry output.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct TelemetryConfig {
    cache_health: bool,
}

impl TelemetryConfig {
    /// Loads telemetry configuration from environment variables.
    ///
    /// # Errors
    /// Returns an error if a boolean environment variable is invalid.
    pub(crate) fn from_env() -> Result<Self> {
        Ok(Self {
            cache_health: env_parse_bool(
                "RUST_AGENT_CACHE_HEALTH",
                DEFAULT_CACHE_HEALTH_TELEMETRY,
            )?,
        })
    }

    /// Returns whether cache-health lines are printed by the terminal harness.
    pub(crate) const fn cache_health(self) -> bool {
        self.cache_health
    }
}

fn env_string(name: &str, default: &str) -> String {
    std::env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| default.to_string())
}

fn env_optional_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

fn env_optional_string(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn env_optional_parent_pid(name: &str) -> Result<Option<libc::pid_t>> {
    let Some(raw) = std::env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
    else {
        return Ok(None);
    };
    parse_parent_pid(name, &raw).map(Some)
}

fn parse_parent_pid(name: &str, raw: &str) -> Result<libc::pid_t> {
    let pid = raw
        .trim()
        .parse::<libc::pid_t>()
        .map_err(|error| anyhow::anyhow!("failed to parse {name}={raw:?}: {error}"))?;
    anyhow::ensure!(
        pid > 1,
        "failed to parse {name}={raw:?}: expected process id greater than 1"
    );
    Ok(pid)
}

fn env_parse_socket_addr(name: &str, default: &str) -> Result<SocketAddr> {
    let raw = env_string(name, default);
    raw.parse()
        .map_err(|error| anyhow::anyhow!("failed to parse {name}={raw:?}: {error}"))
}

fn env_parse_u32(name: &str, default: u32) -> Result<u32> {
    parse_env(name, default)
}

fn env_model_list(name: &str) -> Option<Vec<String>> {
    std::env::var(name).ok().map(|value| {
        value
            .split(',')
            .map(str::trim)
            .filter(|model| !model.is_empty())
            .map(str::to_string)
            .collect()
    })
}

fn default_allowed_models() -> Vec<String> {
    DEFAULT_ALLOWED_MODELS
        .iter()
        .map(ToString::to_string)
        .collect()
}

fn default_reasoning_config() -> (String, Vec<String>) {
    (
        DEFAULT_REASONING_EFFORT.to_string(),
        default_reasoning_efforts(),
    )
}

fn default_reasoning_efforts() -> Vec<String> {
    DEFAULT_REASONING_EFFORTS
        .iter()
        .map(ToString::to_string)
        .collect()
}

fn load_codex_model_allowlist() -> Option<Vec<String>> {
    let path = codex_models_cache_path()?;
    let contents = std::fs::read(path).ok()?;
    codex_model_allowlist_from_cache(&contents).ok()
}

fn load_codex_reasoning_config(
    allowed_models: &[String],
    default_model: &str,
) -> Option<(String, Vec<String>)> {
    let path = codex_models_cache_path()?;
    let contents = std::fs::read(path).ok()?;
    codex_reasoning_config_from_cache(&contents, allowed_models, default_model).ok()
}

fn codex_models_cache_path() -> Option<PathBuf> {
    if let Some(home) = env_optional_path("CODEX_HOME") {
        return Some(home.join(CODEX_MODELS_CACHE_FILE));
    }

    std::env::var_os("HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .map(|home| home.join(".codex").join(CODEX_MODELS_CACHE_FILE))
}

fn codex_model_allowlist_from_cache(contents: &[u8]) -> Result<Vec<String>> {
    let cache: CodexModelsCache = serde_json::from_slice(contents)?;
    let models = cache
        .models
        .into_iter()
        .filter(|model| model.visibility.as_deref().unwrap_or("list") == "list")
        .map(|model| model.slug);
    normalized_model_list(models)
}

fn codex_reasoning_config_from_cache(
    contents: &[u8],
    allowed_models: &[String],
    default_model: &str,
) -> Result<(String, Vec<String>)> {
    let cache: CodexModelsCache = serde_json::from_slice(contents)?;
    let mut default_reasoning_effort = None;
    let mut reasoning_efforts = Vec::new();

    for model in cache.models {
        if model.visibility.as_deref().unwrap_or("list") != "list"
            || !allowed_models
                .iter()
                .any(|allowed_model| allowed_model == &model.slug)
        {
            continue;
        }
        if model.slug == default_model {
            default_reasoning_effort = model.default_reasoning_level;
        }
        for level in model.supported_reasoning_levels {
            reasoning_efforts.push(level.effort);
        }
    }

    let reasoning_efforts = normalized_reasoning_effort_list(reasoning_efforts)?;
    let default_reasoning_effort = default_reasoning_effort
        .filter(|effort| reasoning_efforts.iter().any(|allowed| allowed == effort))
        .or_else(|| {
            reasoning_efforts
                .iter()
                .find(|effort| effort.as_str() == DEFAULT_REASONING_EFFORT)
                .cloned()
        })
        .or_else(|| reasoning_efforts.first().cloned())
        .expect("normalized reasoning effort list cannot be empty");

    Ok((default_reasoning_effort, reasoning_efforts))
}

#[derive(Deserialize)]
struct CodexModelsCache {
    models: Vec<CodexCachedModel>,
}

#[derive(Deserialize)]
struct CodexCachedModel {
    slug: String,
    #[serde(default)]
    visibility: Option<String>,
    #[serde(default)]
    default_reasoning_level: Option<String>,
    #[serde(default)]
    supported_reasoning_levels: Vec<CodexCachedReasoningLevel>,
}

#[derive(Deserialize)]
struct CodexCachedReasoningLevel {
    effort: String,
}

fn normalized_model(model: impl Into<String>) -> Result<String> {
    let model = model.into();
    let model = model.trim();
    anyhow::ensure!(!model.is_empty(), "model cannot be empty");
    Ok(model.to_string())
}

fn normalized_model_list(
    models: impl IntoIterator<Item = impl Into<String>>,
) -> Result<Vec<String>> {
    let mut normalized = Vec::new();
    for model in models {
        let model = normalized_model(model)?;
        if !normalized.iter().any(|existing| existing == &model) {
            normalized.push(model);
        }
    }
    anyhow::ensure!(!normalized.is_empty(), "allowed models cannot be empty");
    Ok(normalized)
}

fn normalized_reasoning_effort(effort: impl Into<String>) -> Result<String> {
    let effort = effort.into();
    let effort = effort.trim();
    anyhow::ensure!(!effort.is_empty(), "reasoning effort cannot be empty");
    Ok(effort.to_string())
}

fn normalized_reasoning_effort_list(
    efforts: impl IntoIterator<Item = impl Into<String>>,
) -> Result<Vec<String>> {
    let mut normalized = Vec::new();
    for effort in efforts {
        let effort = normalized_reasoning_effort(effort)?;
        if !normalized.iter().any(|existing| existing == &effort) {
            normalized.push(effort);
        }
    }
    anyhow::ensure!(
        !normalized.is_empty(),
        "allowed reasoning efforts cannot be empty"
    );
    Ok(normalized)
}

fn env_parse_u64(name: &str, default: u64) -> Result<u64> {
    parse_env(name, default)
}

fn env_parse_usize(name: &str, default: usize) -> Result<usize> {
    parse_env(name, default)
}

fn env_parse_bool(name: &str, default: bool) -> Result<bool> {
    let Some(raw) = std::env::var(name).ok().filter(|value| !value.is_empty()) else {
        return Ok(default);
    };
    match raw.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Ok(true),
        "0" | "false" | "no" | "off" => Ok(false),
        _ => anyhow::bail!("failed to parse {name}={raw:?}: expected boolean"),
    }
}

fn parse_tool_policy(raw: &str) -> Result<ToolPolicy> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "read-only" | "read_only" | "readonly" => Ok(ToolPolicy::ReadOnly),
        "workspace-write" | "workspace_write" | "write" => Ok(ToolPolicy::WorkspaceWrite),
        "workspace-exec" | "workspace_exec" | "exec" => Ok(ToolPolicy::WorkspaceExec),
        _ => anyhow::bail!(
            "failed to parse RUST_AGENT_TOOL_MODE={raw:?}: expected read-only, workspace-write, or workspace-exec"
        ),
    }
}

fn validate_tool_search_concurrency(value: usize) -> Result<usize> {
    anyhow::ensure!(
        (1..=MAX_FFF_SEARCH_CONCURRENCY).contains(&value),
        "failed to parse RUST_AGENT_TOOL_SEARCH_CONCURRENCY={value}: expected 1..={MAX_FFF_SEARCH_CONCURRENCY}"
    );
    Ok(value)
}

fn parse_env<T>(name: &str, default: T) -> Result<T>
where
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
{
    let Some(raw) = std::env::var(name).ok().filter(|value| !value.is_empty()) else {
        return Ok(default);
    };
    raw.parse()
        .map_err(|error| anyhow::anyhow!("failed to parse {name}={raw:?}: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_tool_policy_modes() {
        assert_eq!(
            parse_tool_policy("read-only").unwrap(),
            ToolPolicy::ReadOnly
        );
        assert_eq!(
            parse_tool_policy("workspace-write").unwrap(),
            ToolPolicy::WorkspaceWrite
        );
        assert_eq!(
            parse_tool_policy("workspace-exec").unwrap(),
            ToolPolicy::WorkspaceExec
        );
        assert!(parse_tool_policy("network").is_err());
    }

    #[test]
    fn validates_tool_search_concurrency() {
        assert_eq!(validate_tool_search_concurrency(1).unwrap(), 1);
        assert_eq!(
            validate_tool_search_concurrency(MAX_FFF_SEARCH_CONCURRENCY).unwrap(),
            MAX_FFF_SEARCH_CONCURRENCY
        );
        assert!(validate_tool_search_concurrency(0).is_err());
        assert!(validate_tool_search_concurrency(MAX_FFF_SEARCH_CONCURRENCY + 1).is_err());
    }

    #[test]
    fn validates_parent_pid() {
        assert_eq!(parse_parent_pid("RUST_AGENT_PARENT_PID", "2").unwrap(), 2);
        assert!(parse_parent_pid("RUST_AGENT_PARENT_PID", "1").is_err());
        assert!(parse_parent_pid("RUST_AGENT_PARENT_PID", "0").is_err());
        assert!(parse_parent_pid("RUST_AGENT_PARENT_PID", "not-a-pid").is_err());
    }

    #[test]
    fn parses_visible_codex_models_from_cache() {
        let models = codex_model_allowlist_from_cache(
            br#"{
                "models": [
                    {"slug":"new-frontier","visibility":"list"},
                    {"slug":"internal-review","visibility":"hide"},
                    {"slug":"new-frontier","visibility":"list"},
                    {"slug":"new-fast","visibility":"list"}
                ]
            }"#,
        )
        .unwrap();

        assert_eq!(models, ["new-frontier", "new-fast"]);
    }

    #[test]
    fn parses_reasoning_efforts_for_visible_allowed_models_from_cache() {
        let allowed_models = vec!["new-frontier".to_string(), "new-fast".to_string()];
        let (default_effort, efforts) = codex_reasoning_config_from_cache(
            br#"{
                "models": [
                    {
                        "slug":"new-frontier",
                        "visibility":"list",
                        "default_reasoning_level":"medium",
                        "supported_reasoning_levels":[
                            {"effort":"low"},
                            {"effort":"medium"}
                        ]
                    },
                    {
                        "slug":"internal-review",
                        "visibility":"hide",
                        "default_reasoning_level":"high",
                        "supported_reasoning_levels":[
                            {"effort":"high"}
                        ]
                    },
                    {
                        "slug":"new-fast",
                        "visibility":"list",
                        "supported_reasoning_levels":[
                            {"effort":"medium"},
                            {"effort":"xhigh"}
                        ]
                    }
                ]
            }"#,
            &allowed_models,
            "new-frontier",
        )
        .unwrap();

        assert_eq!(default_effort, "medium");
        assert_eq!(efforts, ["low", "medium", "xhigh"]);
    }
}
