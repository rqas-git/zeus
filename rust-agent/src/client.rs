//! Minimal ChatGPT Codex Responses client.

use anyhow::Context;
use anyhow::Result;
use eventsource_stream::Eventsource;
use futures_util::StreamExt;
use reqwest::header;
use reqwest::Client;
use serde::ser::SerializeSeq;
use serde::Deserialize;
use serde::Serialize;
use serde_json::value::RawValue;
use std::borrow::Cow;
#[cfg(test)]
use std::io::BufRead;

use crate::agent_loop::ModelStreamer;
use crate::agent_loop::SessionId;
use crate::auth::CodexAuth;
use crate::config::ClientConfig;

/// One message in the current in-memory conversation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ConversationMessage<'a> {
    role: ConversationRole,
    text: &'a str,
}

impl<'a> ConversationMessage<'a> {
    /// Creates a user message.
    pub(crate) fn user(text: &'a str) -> Self {
        Self {
            role: ConversationRole::User,
            text,
        }
    }

    /// Creates an assistant message.
    pub(crate) fn assistant(text: &'a str) -> Self {
        Self {
            role: ConversationRole::Assistant,
            text,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ConversationRole {
    User,
    Assistant,
}

/// Async client for ChatGPT requests through Codex OAuth.
pub(crate) struct ChatGptClient {
    auth: CodexAuth,
    config: ClientConfig,
    http: Client,
}

impl ChatGptClient {
    /// Creates a client with explicit backend configuration.
    ///
    /// # Errors
    /// Returns an error if the HTTP client cannot be constructed.
    pub(crate) fn new(auth: CodexAuth, config: ClientConfig) -> Result<Self> {
        let http = Client::builder()
            .timeout(config.request_timeout())
            .build()
            .context("failed to build HTTP client")?;

        Ok(Self { auth, config, http })
    }
}

impl ModelStreamer for ChatGptClient {
    /// Sends a conversation and streams assistant text deltas.
    async fn stream_conversation<'a>(
        &'a self,
        messages: &'a [ConversationMessage<'a>],
        session_id: SessionId,
        model: &'a str,
        on_delta: &'a mut dyn FnMut(&str) -> Result<()>,
    ) -> Result<String> {
        anyhow::ensure!(!messages.is_empty(), "conversation cannot be empty");

        let prompt_cache_key = self.config.prompt_cache_key(session_id.get(), model);
        let body = ResponsesRequest {
            model,
            instructions: self.config.instructions(),
            input: responses_input(messages),
            tools: EmptyArray,
            tool_choice: "auto",
            parallel_tool_calls: false,
            store: false,
            stream: true,
            prompt_cache_key: &prompt_cache_key,
        };

        let response = self
            .http
            .post(self.config.responses_url())
            .bearer_auth(self.auth.access_token())
            .header("ChatGPT-Account-ID", self.auth.account_id())
            .header("originator", self.config.originator())
            .header("version", self.config.version())
            .header(header::ACCEPT, "text/event-stream")
            .json(&body)
            .send()
            .await
            .context("failed to send request to ChatGPT Codex backend")?;

        let status = response.status();
        if !status.is_success() {
            let response_text = response
                .text()
                .await
                .context("failed to read ChatGPT Codex error response")?;
            anyhow::bail!(
                "ChatGPT Codex backend returned {status}: {}",
                truncate_for_error(&response_text)
            );
        }

        read_assistant_text_stream(response, on_delta).await
    }
}

async fn read_assistant_text_stream(
    response: reqwest::Response,
    mut on_delta: impl FnMut(&str) -> Result<()>,
) -> Result<String> {
    let mut state = AssistantText::default();
    let mut stream = response.bytes_stream().eventsource();

    while let Some(event) = stream.next().await {
        let event = event.map_err(|error| {
            anyhow::anyhow!("failed to read ChatGPT Codex response stream: {error}")
        })?;
        state.handle_data(&event.data, &mut on_delta)?;
    }

    state.finish(&mut on_delta)
}

#[derive(Debug, Serialize)]
struct ResponsesRequest<'a> {
    model: &'a str,
    instructions: &'a str,
    input: ResponsesInput<'a>,
    tools: EmptyArray,
    tool_choice: &'static str,
    parallel_tool_calls: bool,
    store: bool,
    stream: bool,
    prompt_cache_key: &'a str,
}

fn responses_input<'a>(messages: &'a [ConversationMessage<'a>]) -> ResponsesInput<'a> {
    ResponsesInput { messages }
}

#[derive(Debug)]
struct ResponsesInput<'a> {
    messages: &'a [ConversationMessage<'a>],
}

impl Serialize for ResponsesInput<'_> {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut seq = serializer.serialize_seq(Some(self.messages.len()))?;
        for message in self.messages {
            seq.serialize_element(&ResponsesMessage::from(message))?;
        }
        seq.end()
    }
}

#[derive(Clone, Copy, Debug)]
struct EmptyArray;

impl Serialize for EmptyArray {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_seq(Some(0))?.end()
    }
}

#[derive(Debug, Serialize)]
struct ResponsesMessage<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    role: &'static str,
    content: [ResponsesContent<'a>; 1],
}

impl<'a> From<&'a ConversationMessage<'a>> for ResponsesMessage<'a> {
    fn from(message: &'a ConversationMessage<'a>) -> Self {
        Self {
            kind: "message",
            role: message.role.as_str(),
            content: [ResponsesContent {
                kind: message.role.content_type(),
                text: message.text,
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

#[cfg(test)]
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

        match event.kind.as_ref() {
            "response.output_text.delta" => {
                if let Some(delta) = event.delta.as_deref() {
                    on_delta(delta)?;
                    self.text.push_str(delta);
                }
            }
            "response.output_item.done" if self.text.is_empty() => {
                self.fallback_text
                    .push_str(&assistant_text_from_item(event.item));
            }
            "response.failed" => {
                let message = response_error_message(event.response)
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

fn assistant_text_from_item(item: Option<&RawValue>) -> String {
    let Some(item) = item else {
        return String::new();
    };

    let Ok(item) = serde_json::from_str::<OutputItem>(item.get()) else {
        return String::new();
    };
    if item.role.as_deref() != Some("assistant") {
        return String::new();
    }

    item.content
        .into_iter()
        .filter(|content| content.kind.as_ref() == "output_text")
        .filter_map(|content| content.text)
        .map(Cow::into_owned)
        .collect::<String>()
}

#[derive(Debug, Deserialize)]
struct OutputItem<'a> {
    #[serde(borrow)]
    role: Option<Cow<'a, str>>,
    #[serde(default, borrow)]
    content: Vec<OutputContent<'a>>,
}

#[derive(Debug, Deserialize)]
struct OutputContent<'a> {
    #[serde(rename = "type", borrow)]
    kind: Cow<'a, str>,
    #[serde(borrow)]
    text: Option<Cow<'a, str>>,
}

fn response_error_message(response: Option<&RawValue>) -> Option<String> {
    let response = response?;
    let response = serde_json::from_str::<FailedResponse>(response.get()).ok()?;
    response.error?.message.map(Cow::into_owned)
}

#[derive(Debug, Deserialize)]
struct FailedResponse<'a> {
    #[serde(borrow)]
    error: Option<ResponseError<'a>>,
}

#[derive(Debug, Deserialize)]
struct ResponseError<'a> {
    #[serde(borrow)]
    message: Option<Cow<'a, str>>,
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
struct StreamEvent<'a> {
    #[serde(rename = "type", borrow)]
    kind: Cow<'a, str>,
    #[serde(borrow)]
    delta: Option<Cow<'a, str>>,
    #[serde(borrow)]
    response: Option<&'a RawValue>,
    #[serde(borrow)]
    item: Option<&'a RawValue>,
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
    fn extracts_escaped_text_deltas() {
        let stream = r#"event: response.output_text.delta
data: {"type":"response.output_text.delta","content_index":0,"delta":":\n\n","item_id":"msg_1","logprobs":[],"output_index":0,"sequence_number":10}

event: response.completed
data: {"type":"response.completed","response":{"id":"resp_1"}}

"#;

        assert_eq!(extract_assistant_text(stream).unwrap(), ":\n\n");
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
