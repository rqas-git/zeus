//! Minimal ChatGPT Codex Responses client.

use std::time::Duration;

use anyhow::Context;
use anyhow::Result;
use reqwest::blocking::Client;
use reqwest::header;
use serde::Deserialize;
use serde_json::json;
use serde_json::Value;

use crate::auth::CodexAuth;

const CODEX_RESPONSES_URL: &str = "https://chatgpt.com/backend-api/codex/responses";
const CODEX_VERSION: &str = "0.128.0";
const MODEL: &str = "gpt-5.5";

/// Blocking client for one-message ChatGPT requests through Codex OAuth.
pub(crate) struct ChatGptClient {
    auth: CodexAuth,
    http: Client,
}

impl ChatGptClient {
    /// Creates a client with hardcoded backend configuration.
    ///
    /// # Errors
    /// Returns an error if the HTTP client cannot be constructed.
    pub(crate) fn new(auth: CodexAuth) -> Result<Self> {
        let http = Client::builder()
            .timeout(Duration::from_secs(120))
            .build()
            .context("failed to build HTTP client")?;

        Ok(Self { auth, http })
    }

    /// Sends a user message and returns the assistant text.
    ///
    /// # Errors
    /// Returns an error for transport failures, backend errors, or malformed SSE.
    pub(crate) fn send_message(&self, message: &str) -> Result<String> {
        let body = json!({
            "model": MODEL,
            "instructions": "You are a concise assistant.",
            "input": [{
                "type": "message",
                "role": "user",
                "content": [{
                    "type": "input_text",
                    "text": message,
                }],
            }],
            "tools": [],
            "tool_choice": "auto",
            "parallel_tool_calls": false,
            "store": false,
            "stream": true,
        });

        let response = self
            .http
            .post(CODEX_RESPONSES_URL)
            .bearer_auth(self.auth.access_token())
            .header("ChatGPT-Account-ID", self.auth.account_id())
            .header("version", CODEX_VERSION)
            .header(header::ACCEPT, "text/event-stream")
            .json(&body)
            .send()
            .context("failed to send request to ChatGPT Codex backend")?;

        let status = response.status();
        let response_text = response
            .text()
            .context("failed to read ChatGPT Codex response")?;

        anyhow::ensure!(
            status.is_success(),
            "ChatGPT Codex backend returned {status}: {}",
            truncate_for_error(&response_text)
        );

        extract_assistant_text(&response_text)
    }
}

fn extract_assistant_text(stream: &str) -> Result<String> {
    let mut text = String::new();
    let mut fallback_text = String::new();
    let mut completed = false;

    for block in stream.split("\n\n") {
        let Some(data) = sse_data(block) else {
            continue;
        };
        if data == "[DONE]" {
            completed = true;
            continue;
        }

        let event: StreamEvent = serde_json::from_str(&data)
            .with_context(|| format!("failed to parse SSE data: {}", truncate_for_error(&data)))?;

        match event.kind.as_str() {
            "response.output_text.delta" => {
                if let Some(delta) = event.delta {
                    text.push_str(&delta);
                }
            }
            "response.output_item.done" => {
                if text.is_empty() {
                    fallback_text.push_str(&assistant_text_from_item(event.item.as_ref()));
                }
            }
            "response.failed" => {
                let message = response_error_message(event.response.as_ref())
                    .unwrap_or_else(|| "response failed".to_string());
                anyhow::bail!("{message}");
            }
            "response.completed" => {
                completed = true;
            }
            _ => {}
        }
    }

    anyhow::ensure!(completed, "response stream ended before completion");
    if text.is_empty() {
        text = fallback_text;
    }
    anyhow::ensure!(!text.trim().is_empty(), "assistant response was empty");
    Ok(text)
}

fn sse_data(block: &str) -> Option<String> {
    let lines = block
        .lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .collect::<Vec<_>>();

    if lines.is_empty() {
        None
    } else {
        Some(lines.join("\n"))
    }
}

fn assistant_text_from_item(item: Option<&Value>) -> String {
    let Some(item) = item else {
        return String::new();
    };
    if item.get("role").and_then(Value::as_str) != Some("assistant") {
        return String::new();
    }

    item.get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|content| content.get("type").and_then(Value::as_str) == Some("output_text"))
        .filter_map(|content| content.get("text").and_then(Value::as_str))
        .collect::<String>()
}

fn response_error_message(response: Option<&Value>) -> Option<String> {
    response?
        .get("error")?
        .get("message")?
        .as_str()
        .map(ToOwned::to_owned)
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

#[derive(Debug, Deserialize)]
struct StreamEvent {
    #[serde(rename = "type")]
    kind: String,
    delta: Option<String>,
    response: Option<Value>,
    item: Option<Value>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_text_deltas() {
        let stream = r#"event: response.output_text.delta
data: {"type":"response.output_text.delta","delta":"hello"}

event: response.output_text.delta
data: {"type":"response.output_text.delta","delta":" world"}

event: response.completed
data: {"type":"response.completed","response":{"id":"resp_1"}}

"#;

        assert_eq!(extract_assistant_text(stream).unwrap(), "hello world");
    }

    #[test]
    fn falls_back_to_done_message_item() {
        let stream = r#"event: response.output_item.done
data: {"type":"response.output_item.done","item":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"done text"}]}}

event: response.completed
data: {"type":"response.completed","response":{"id":"resp_1"}}

"#;

        assert_eq!(extract_assistant_text(stream).unwrap(), "done text");
    }

    #[test]
    fn reports_backend_failure() {
        let stream = r#"event: response.failed
data: {"type":"response.failed","response":{"error":{"message":"nope"}}}

"#;

        let error = extract_assistant_text(stream).unwrap_err().to_string();
        assert_eq!(error, "nope");
    }
}
