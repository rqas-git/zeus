//! Runtime configuration for the agent harness.

use std::time::Duration;

use anyhow::Result;

const DEFAULT_CODEX_RESPONSES_URL: &str = "https://chatgpt.com/backend-api/codex/responses";
const DEFAULT_CODEX_ORIGINATOR: &str = "codex_cli_rs";
const DEFAULT_CODEX_VERSION: &str = "0.128.0";
const DEFAULT_MODEL: &str = "gpt-5.5";
const DEFAULT_INSTRUCTIONS: &str = "You are a concise assistant.";
const DEFAULT_REQUEST_TIMEOUT_SECS: u64 = 120;
const DEFAULT_CONTEXT_MAX_MESSAGES: usize = 40;
const DEFAULT_CONTEXT_MAX_BYTES: usize = 64 * 1024;
const DEFAULT_DELTA_FLUSH_INTERVAL_MS: u64 = 16;
const DEFAULT_DELTA_FLUSH_BYTES: usize = 4096;

/// Configuration for one running rust-agent process.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AppConfig {
    pub(crate) client: ClientConfig,
    pub(crate) context: ContextWindowConfig,
    pub(crate) output: OutputConfig,
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
            output: OutputConfig::from_env()?,
        })
    }
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            client: ClientConfig::default(),
            context: ContextWindowConfig::default(),
            output: OutputConfig::default(),
        }
    }
}

/// Configuration for ChatGPT Codex backend requests.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ClientConfig {
    model: String,
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
            model: env_string("RUST_AGENT_MODEL", DEFAULT_MODEL),
            instructions: env_string("RUST_AGENT_INSTRUCTIONS", DEFAULT_INSTRUCTIONS),
            responses_url: env_string("RUST_AGENT_RESPONSES_URL", DEFAULT_CODEX_RESPONSES_URL),
            originator: env_string("RUST_AGENT_ORIGINATOR", DEFAULT_CODEX_ORIGINATOR),
            version: env_string("RUST_AGENT_VERSION", DEFAULT_CODEX_VERSION),
            request_timeout: Duration::from_secs(request_timeout_secs),
            prompt_cache_namespace: env_string("RUST_AGENT_PROMPT_CACHE_NAMESPACE", "rust-agent"),
        })
    }

    /// Returns the configured model slug.
    pub(crate) fn model(&self) -> &str {
        &self.model
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
    pub(crate) fn prompt_cache_key(&self, session_id: u64) -> String {
        format!("{}-{session_id}", self.prompt_cache_namespace)
    }
}

impl Default for ClientConfig {
    fn default() -> Self {
        Self {
            model: DEFAULT_MODEL.to_string(),
            instructions: DEFAULT_INSTRUCTIONS.to_string(),
            responses_url: DEFAULT_CODEX_RESPONSES_URL.to_string(),
            originator: DEFAULT_CODEX_ORIGINATOR.to_string(),
            version: DEFAULT_CODEX_VERSION.to_string(),
            request_timeout: Duration::from_secs(DEFAULT_REQUEST_TIMEOUT_SECS),
            prompt_cache_namespace: "rust-agent".to_string(),
        }
    }
}

/// Bounds applied before sending history to the model.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ContextWindowConfig {
    max_messages: usize,
    max_bytes: usize,
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
        })
    }

    /// Creates context-window bounds.
    #[cfg(test)]
    pub(crate) const fn new(max_messages: usize, max_bytes: usize) -> Self {
        Self {
            max_messages,
            max_bytes,
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
}

impl Default for ContextWindowConfig {
    fn default() -> Self {
        Self {
            max_messages: DEFAULT_CONTEXT_MAX_MESSAGES,
            max_bytes: DEFAULT_CONTEXT_MAX_BYTES,
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

fn env_string(name: &str, default: &str) -> String {
    std::env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| default.to_string())
}

fn env_parse_u64(name: &str, default: u64) -> Result<u64> {
    parse_env(name, default)
}

fn env_parse_usize(name: &str, default: usize) -> Result<usize> {
    parse_env(name, default)
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
