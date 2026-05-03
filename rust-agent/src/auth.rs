//! Local Codex OAuth credential loading.

use std::fs;
use std::path::Path;
use std::path::PathBuf;

use anyhow::Context;
use anyhow::Result;
use serde::Deserialize;

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

        anyhow::ensure!(!tokens.access_token.trim().is_empty(), "access token is empty");
        anyhow::ensure!(!account_id.trim().is_empty(), "account id is empty");

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
