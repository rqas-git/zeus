//! Minimal ChatGPT Codex Responses client.

use std::io::BufRead;
use std::io::BufReader;
use std::time::Duration;

use anyhow::Context;
use anyhow::Result;
use reqwest::blocking::Client;
use reqwest::header;
use serde::Deserialize;
use serde::Serialize;
use serde_json::json;
use serde_json::Value;

use crate::auth::CodexAuth;

const CODEX_RESPONSES_URL: &str = "https://chatgpt.com/backend-api/codex/responses";
const CODEX_ORIGINATOR: &str = "codex_cli_rs";
const CODEX_VERSION: &str = "0.128.0";
const MODEL: &str = "gpt-5.5";

/// One message in the current in-memory conversation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ConversationMessage {
    role: ConversationRole,
    text: String,
}

impl ConversationMessage {
    /// Creates a user message.
    pub(crate) fn user(text: impl Into<String>) -> Self {
        Self {
            role: ConversationRole::User,
            text: text.into(),
        }
    }

    /// Creates an assistant message.
    pub(crate) fn assistant(text: impl Into<String>) -> Self {
        Self {
            role: ConversationRole::Assistant,
            text: text.into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum ConversationRole {
    User,
    Assistant,
}

/// Blocking client for ChatGPT requests through Codex OAuth.
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

    /// Sends a conversation and streams assistant text deltas.
    ///
    /// # Errors
    /// Returns an error for transport failures, backend errors, malformed SSE, or callback failures.
    pub(crate) fn stream_conversation(
        &self,
        messages: &[ConversationMessage],
        on_delta: impl FnMut(&str) -> Result<()>,
    ) -> Result<String> {
        anyhow::ensure!(!messages.is_empty(), "conversation cannot be empty");

        let body = json!({
            "model": MODEL,
            "instructions": "You are a concise assistant.",
            "input": responses_input(messages),
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
            .header("originator", CODEX_ORIGINATOR)
            .header("version", CODEX_VERSION)
            .header(header::ACCEPT, "text/event-stream")
            .json(&body)
            .send()
            .context("failed to send request to ChatGPT Codex backend")?;

        let status = response.status();
        if !status.is_success() {
            let response_text = response
                .text()
                .context("failed to read ChatGPT Codex error response")?;
            anyhow::bail!(
                "ChatGPT Codex backend returned {status}: {}",
                truncate_for_error(&response_text)
            );
        }

        read_assistant_text(BufReader::new(response), on_delta)
    }
}

fn responses_input(messages: &[ConversationMessage]) -> Vec<ResponsesMessage<'_>> {
    messages.iter().map(ResponsesMessage::from).collect()
}

#[derive(Debug, Serialize)]
struct ResponsesMessage<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    role: &'static str,
    content: [ResponsesContent<'a>; 1],
}

impl<'a> From<&'a ConversationMessage> for ResponsesMessage<'a> {
    fn from(message: &'a ConversationMessage) -> Self {
        Self {
            kind: "message",
            role: message.role.as_str(),
            content: [ResponsesContent {
                kind: message.role.content_type(),
                text: &message.text,
            }],
        }
    }
}

impl ConversationRole {
    fn as_str(&self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Assistant => "assistant",
        }
    }

    fn content_type(&self) -> &'static str {
        match self {
            Self::User => "input_text",
            Self::Assistant => "output_text",
        }
    }
}

#[derive(Debug, Serialize)]
struct ResponsesContent<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    text: &'a str,
}

#[cfg(test)]
fn extract_assistant_text(stream: &str) -> Result<String> {
    read_assistant_text(std::io::Cursor::new(stream.as_bytes()), |_| Ok(()))
}

fn read_assistant_text(
    mut reader: impl BufRead,
    mut on_delta: impl FnMut(&str) -> Result<()>,
) -> Result<String> {
    let mut state = AssistantText::default();
    let mut event_data = String::new();
    let mut has_event_data = false;
    let mut line = String::new();

    loop {
        line.clear();
        let bytes = reader
            .read_line(&mut line)
            .context("failed to read ChatGPT Codex response stream")?;
        if bytes == 0 {
            break;
        }

        let line = line.trim_end_matches('\n').trim_end_matches('\r');
        if line.is_empty() {
            if has_event_data {
                state.handle_data(&event_data, &mut on_delta)?;
                event_data.clear();
                has_event_data = false;
            }
            continue;
        }

        let Some(data) = line.strip_prefix("data:") else {
            continue;
        };
        let data = data.strip_prefix(' ').unwrap_or(data);
        if has_event_data {
            event_data.push('\n');
        }
        event_data.push_str(data);
        has_event_data = true;
    }

    if has_event_data {
        state.handle_data(&event_data, &mut on_delta)?;
    }

    state.finish(&mut on_delta)
}

#[derive(Default)]
struct AssistantText {
    text: String,
    fallback_text: String,
    completed: bool,
}

impl AssistantText {
    fn handle_data(
        &mut self,
        data: &str,
        on_delta: &mut impl FnMut(&str) -> Result<()>,
    ) -> Result<()> {
        if data == "[DONE]" {
            self.completed = true;
            return Ok(());
        }

        let event: StreamEvent = serde_json::from_str(data)
            .with_context(|| format!("failed to parse SSE data: {}", truncate_for_error(data)))?;

        match event.kind.as_str() {
            "response.output_text.delta" => {
                if let Some(delta) = event.delta {
                    on_delta(&delta)?;
                    self.text.push_str(&delta);
                }
            }
            "response.output_item.done" if self.text.is_empty() => {
                self.fallback_text
                    .push_str(&assistant_text_from_item(event.item.as_ref()));
            }
            "response.failed" => {
                let message = response_error_message(event.response.as_ref())
                    .unwrap_or_else(|| "response failed".to_string());
                anyhow::bail!("{message}");
            }
            "response.completed" => {
                self.completed = true;
            }
            _ => {}
        }

        Ok(())
    }

    fn finish(mut self, on_delta: &mut impl FnMut(&str) -> Result<()>) -> Result<String> {
        anyhow::ensure!(self.completed, "response stream ended before completion");
        if self.text.is_empty() {
            self.text = self.fallback_text;
            if !self.text.is_empty() {
                on_delta(&self.text)?;
            }
        }
        anyhow::ensure!(!self.text.trim().is_empty(), "assistant response was empty");
        Ok(self.text)
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
    use serde_json::json;

    #[test]
    fn serializes_conversation_history() {
        let messages = [
            ConversationMessage::user("hello"),
            ConversationMessage::assistant("hi"),
        ];
        let input = responses_input(&messages);

        assert_eq!(
            serde_json::to_value(input).unwrap(),
            json!([
                {
                    "type": "message",
                    "role": "user",
                    "content": [{"type": "input_text", "text": "hello"}],
                },
                {
                    "type": "message",
                    "role": "assistant",
                    "content": [{"type": "output_text", "text": "hi"}],
                },
            ])
        );
    }

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
    fn streams_text_deltas_to_callback() {
        let stream = r#"event: response.output_text.delta
data: {"type":"response.output_text.delta","delta":"hello"}

event: response.output_text.delta
data: {"type":"response.output_text.delta","delta":" world"}

event: response.completed
data: {"type":"response.completed","response":{"id":"resp_1"}}

"#;
        let mut deltas = Vec::new();

        let text = read_assistant_text(std::io::Cursor::new(stream.as_bytes()), |delta| {
            deltas.push(delta.to_string());
            Ok(())
        })
        .unwrap();

        assert_eq!(text, "hello world");
        assert_eq!(deltas, ["hello", " world"]);
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
    fn extracts_text_from_crlf_sse_blocks() {
        let stream = concat!(
            "event: response.output_text.delta\r\n",
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"ok\"}\r\n",
            "\r\n",
            "event: response.completed\r\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_1\"}}\r\n",
            "\r\n",
        );

        assert_eq!(extract_assistant_text(stream).unwrap(), "ok");
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
