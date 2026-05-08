//! Minimal ChatGPT Codex Responses client.

use anyhow::Context;
use anyhow::Result;
use futures_util::StreamExt;
use memchr::memchr;
use reqwest::header;
use reqwest::Client;
use reqwest::StatusCode;
use serde::ser::SerializeSeq;
use serde::ser::SerializeStruct;
use serde::Deserialize;
use serde::Serialize;
use serde_json::value::RawValue;
use std::borrow::Cow;
#[cfg(test)]
use std::io::Read;

use crate::agent_loop::CacheHealth;
use crate::agent_loop::CacheStatus;
use crate::agent_loop::ModelResponse;
use crate::agent_loop::ModelStreamer;
use crate::agent_loop::ModelToolCall;
use crate::agent_loop::SessionId;
use crate::agent_loop::TokenUsage;
use crate::auth::AuthCredentials;
use crate::auth::AuthManager;
use crate::config::ClientConfig;
use crate::tools::ToolSpec;

/// One message in the current in-memory conversation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ConversationMessage<'a> {
    Message {
        role: ConversationRole,
        text: Cow<'a, str>,
    },
    FunctionCall {
        item_id: Option<&'a str>,
        call_id: &'a str,
        name: &'a str,
        arguments: &'a str,
    },
    FunctionOutput {
        call_id: &'a str,
        output: &'a str,
        success: bool,
    },
}

impl<'a> ConversationMessage<'a> {
    /// Creates a user message.
    pub(crate) fn user(text: &'a str) -> Self {
        Self::Message {
            role: ConversationRole::User,
            text: Cow::Borrowed(text),
        }
    }

    /// Creates an owned user message.
    pub(crate) fn owned_user(text: impl Into<String>) -> Self {
        Self::Message {
            role: ConversationRole::User,
            text: Cow::Owned(text.into()),
        }
    }

    /// Creates an assistant message.
    pub(crate) fn assistant(text: &'a str) -> Self {
        Self::Message {
            role: ConversationRole::Assistant,
            text: Cow::Borrowed(text),
        }
    }

    /// Creates an assistant function-call item.
    pub(crate) fn function_call(
        item_id: Option<&'a str>,
        call_id: &'a str,
        name: &'a str,
        arguments: &'a str,
    ) -> Self {
        Self::FunctionCall {
            item_id,
            call_id,
            name,
            arguments,
        }
    }

    /// Creates a function-call output item.
    pub(crate) fn function_output(call_id: &'a str, output: &'a str, success: bool) -> Self {
        Self::FunctionOutput {
            call_id,
            output,
            success,
        }
    }

    fn input_bytes(&self) -> usize {
        match self {
            Self::Message { text, .. } => text.len(),
            Self::FunctionCall {
                item_id,
                call_id,
                name,
                arguments,
            } => item_id.map_or(0, str::len) + call_id.len() + name.len() + arguments.len(),
            Self::FunctionOutput {
                call_id, output, ..
            } => call_id.len() + output.len(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ConversationRole {
    User,
    Assistant,
}

/// Async client for ChatGPT requests through rust-agent auth.
pub(crate) struct ChatGptClient {
    auth: AuthManager,
    config: ClientConfig,
    http: Client,
}

impl ChatGptClient {
    /// Creates a client with explicit backend configuration.
    ///
    /// # Errors
    /// Returns an error if the HTTP client cannot be constructed.
    pub(crate) fn new(auth: AuthManager, config: ClientConfig) -> Result<Self> {
        let http = Client::builder()
            .timeout(config.request_timeout())
            .build()
            .context("failed to build HTTP client")?;

        Ok(Self { auth, config, http })
    }

    async fn send_responses_request<'a>(
        &'a self,
        body: &'a ResponsesRequest<'a>,
        credentials: &AuthCredentials,
    ) -> Result<reqwest::Response> {
        self.http
            .post(self.config.responses_url())
            .bearer_auth(credentials.access_token())
            .header("ChatGPT-Account-ID", credentials.account_id())
            .header("originator", self.config.originator())
            .header("version", self.config.version())
            .header(header::ACCEPT, "text/event-stream")
            .json(body)
            .send()
            .await
            .context("failed to send request to ChatGPT Codex backend")
    }
}

impl ModelStreamer for ChatGptClient {
    /// Sends a conversation and streams assistant text deltas.
    async fn stream_conversation<'a>(
        &'a self,
        messages: &'a [ConversationMessage<'a>],
        tools: &'a [ToolSpec],
        parallel_tool_calls: bool,
        session_id: SessionId,
        model: &'a str,
        on_delta: &'a mut (dyn FnMut(&str) -> Result<()> + Send),
    ) -> Result<ModelResponse> {
        self.stream_conversation_inner(
            messages,
            tools,
            parallel_tool_calls,
            session_id,
            model,
            None,
            self.config.instructions(),
            on_delta,
        )
        .await
    }

    async fn stream_conversation_with_reasoning<'a>(
        &'a self,
        messages: &'a [ConversationMessage<'a>],
        tools: &'a [ToolSpec],
        parallel_tool_calls: bool,
        session_id: SessionId,
        model: &'a str,
        reasoning_effort: Option<&'a str>,
        on_delta: &'a mut (dyn FnMut(&str) -> Result<()> + Send),
    ) -> Result<ModelResponse> {
        self.stream_conversation_inner(
            messages,
            tools,
            parallel_tool_calls,
            session_id,
            model,
            reasoning_effort,
            self.config.instructions(),
            on_delta,
        )
        .await
    }

    async fn compact_conversation<'a>(
        &'a self,
        prompt: &'a str,
        session_id: SessionId,
        model: &'a str,
        reasoning_effort: Option<&'a str>,
    ) -> Result<String> {
        let messages = [ConversationMessage::user(prompt)];
        let tools: [ToolSpec; 0] = [];
        let mut ignore_delta = |_delta: &str| Ok(());
        let response = self
            .stream_conversation_inner(
                &messages,
                &tools,
                false,
                session_id,
                model,
                reasoning_effort,
                crate::compaction::SUMMARIZATION_SYSTEM_PROMPT,
                &mut ignore_delta,
            )
            .await?;
        Ok(response.text)
    }
}

impl ChatGptClient {
    async fn stream_conversation_inner<'a>(
        &'a self,
        messages: &'a [ConversationMessage<'a>],
        tools: &'a [ToolSpec],
        parallel_tool_calls: bool,
        _session_id: SessionId,
        model: &'a str,
        reasoning_effort: Option<&'a str>,
        instructions: &'a str,
        on_delta: &'a mut (dyn FnMut(&str) -> Result<()> + Send),
    ) -> Result<ModelResponse> {
        anyhow::ensure!(!messages.is_empty(), "conversation cannot be empty");

        let stable_prefix = stable_prefix_stats(instructions, tools);
        let prompt_cache_key = self.config.prompt_cache_key(model);
        let input_bytes = conversation_input_bytes(messages);
        let body = ResponsesRequest {
            model,
            instructions,
            input: responses_input(messages),
            tools,
            tool_choice: "auto",
            parallel_tool_calls,
            reasoning: reasoning_effort.map(|effort| ResponsesReasoning { effort }),
            store: false,
            stream: true,
            prompt_cache_key: &prompt_cache_key,
        };
        let credentials = self.auth.credentials().await?;
        let mut response = self.send_responses_request(&body, &credentials).await?;

        if response.status() == StatusCode::UNAUTHORIZED {
            let credentials = self.auth.refresh().await?;
            response = self.send_responses_request(&body, &credentials).await?;
        }

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

        let completion = read_assistant_text_stream(response, on_delta).await?;
        let cache_health = CacheHealth {
            model: model.to_string(),
            prompt_cache_key,
            stable_prefix_hash: stable_prefix.hash,
            stable_prefix_bytes: stable_prefix.bytes,
            message_count: messages.len(),
            input_bytes,
            response_id: completion.response_id,
            usage: completion.usage,
            cache_status: CacheStatus::FirstRequest,
        };
        Ok(ModelResponse::with_cache_health(
            completion.text,
            completion.tool_calls,
            cache_health,
        ))
    }
}

async fn read_assistant_text_stream(
    response: reqwest::Response,
    mut on_delta: impl FnMut(&str) -> Result<()>,
) -> Result<StreamCompletion> {
    let mut state = AssistantText::default();
    let mut parser = SseDataParser::default();
    let mut stream = response.bytes_stream();

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.context("failed to read ChatGPT Codex response stream")?;
        parser.push_bytes(&chunk, &mut |data| state.handle_data(data, &mut on_delta))?;
    }
    parser.finish(&mut |data| state.handle_data(data, &mut on_delta))?;

    state.finish(&mut on_delta)
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct StablePrefixStats {
    hash: u64,
    bytes: usize,
}

fn stable_prefix_stats(instructions: &str, tools: &[ToolSpec]) -> StablePrefixStats {
    let mut hash = Fnv1a64::new();
    hash.update("instructions\0");
    hash.update(instructions);
    hash.update("\0tools\0");
    for tool in tools {
        hash.update(tool.name());
        hash.update("\0");
        hash.update(tool.description());
        hash.update("\0");
        hash.update(tool.parameters_cache_key());
        hash.update("\0");
    }
    StablePrefixStats {
        hash: hash.finish(),
        bytes: instructions.len()
            + tools
                .iter()
                .map(|tool| {
                    tool.name().len() + tool.description().len() + tool.parameters_cache_key().len()
                })
                .sum::<usize>(),
    }
}

fn conversation_input_bytes(messages: &[ConversationMessage<'_>]) -> usize {
    messages.iter().map(|message| message.input_bytes()).sum()
}

#[derive(Clone, Copy, Debug)]
struct Fnv1a64(u64);

impl Fnv1a64 {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;

    const fn new() -> Self {
        Self(Self::OFFSET)
    }

    fn update(&mut self, value: &str) {
        for byte in value.as_bytes() {
            self.0 ^= u64::from(*byte);
            self.0 = self.0.wrapping_mul(Self::PRIME);
        }
    }

    const fn finish(self) -> u64 {
        self.0
    }
}

#[derive(Debug, Serialize)]
struct ResponsesRequest<'a> {
    model: &'a str,
    instructions: &'a str,
    input: ResponsesInput<'a>,
    tools: &'a [ToolSpec],
    tool_choice: &'static str,
    parallel_tool_calls: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning: Option<ResponsesReasoning<'a>>,
    store: bool,
    stream: bool,
    prompt_cache_key: &'a str,
}

#[derive(Debug, Serialize)]
struct ResponsesReasoning<'a> {
    effort: &'a str,
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
            seq.serialize_element(&ResponsesMessage(message))?;
        }
        seq.end()
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

struct ResponsesMessage<'a>(&'a ConversationMessage<'a>);

impl Serialize for ResponsesMessage<'_> {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self.0 {
            ConversationMessage::Message { role, text } => {
                let mut state = serializer.serialize_struct("ResponsesMessage", 3)?;
                state.serialize_field("type", "message")?;
                state.serialize_field("role", role.as_str())?;
                state.serialize_field(
                    "content",
                    &[ResponsesContent {
                        kind: role.content_type(),
                        text: text.as_ref(),
                    }],
                )?;
                state.end()
            }
            ConversationMessage::FunctionCall {
                item_id,
                call_id,
                name,
                arguments,
            } => {
                let field_count = if item_id.is_some() { 5 } else { 4 };
                let mut state =
                    serializer.serialize_struct("ResponsesFunctionCall", field_count)?;
                state.serialize_field("type", "function_call")?;
                if let Some(item_id) = item_id {
                    state.serialize_field("id", item_id)?;
                }
                state.serialize_field("call_id", call_id)?;
                state.serialize_field("name", name)?;
                state.serialize_field("arguments", arguments)?;
                state.end()
            }
            ConversationMessage::FunctionOutput {
                call_id,
                output,
                success: _,
            } => {
                let mut state = serializer.serialize_struct("ResponsesFunctionOutput", 3)?;
                state.serialize_field("type", "function_call_output")?;
                state.serialize_field("call_id", call_id)?;
                state.serialize_field("output", output)?;
                state.end()
            }
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
    mut reader: impl Read,
    mut on_delta: impl FnMut(&str) -> Result<()>,
) -> Result<String> {
    read_assistant_completion(&mut reader, &mut on_delta).map(|completion| completion.text)
}

#[cfg(test)]
fn read_assistant_completion(
    mut reader: impl Read,
    mut on_delta: impl FnMut(&str) -> Result<()>,
) -> Result<StreamCompletion> {
    let mut state = AssistantText::default();
    let mut parser = SseDataParser::default();
    let mut chunk = [0; 7];

    loop {
        let bytes = reader
            .read(&mut chunk)
            .context("failed to read ChatGPT Codex response stream")?;
        if bytes == 0 {
            break;
        }
        parser.push_bytes(&chunk[..bytes], &mut |data| {
            state.handle_data(data, &mut on_delta)
        })?;
    }

    parser.finish(&mut |data| state.handle_data(data, &mut on_delta))?;

    state.finish(&mut on_delta)
}

struct SseDataParser {
    line: Vec<u8>,
    event_data: String,
    has_event_data: bool,
}

impl Default for SseDataParser {
    fn default() -> Self {
        Self {
            line: Vec::with_capacity(512),
            event_data: String::with_capacity(512),
            has_event_data: false,
        }
    }
}

impl SseDataParser {
    fn push_bytes(
        &mut self,
        mut bytes: &[u8],
        on_data: &mut impl FnMut(&str) -> Result<()>,
    ) -> Result<()> {
        while let Some(line_end) = memchr(b'\n', bytes) {
            self.line.extend_from_slice(&bytes[..line_end]);
            self.handle_line(on_data)?;
            bytes = &bytes[line_end + 1..];
        }
        self.line.extend_from_slice(bytes);
        Ok(())
    }

    fn finish(&mut self, on_data: &mut impl FnMut(&str) -> Result<()>) -> Result<()> {
        if !self.line.is_empty() {
            self.handle_line(on_data)?;
        }
        self.flush_event(on_data)
    }

    fn handle_line(&mut self, on_data: &mut impl FnMut(&str) -> Result<()>) -> Result<()> {
        if self.line.last() == Some(&b'\r') {
            self.line.pop();
        }

        if self.line.is_empty() {
            self.flush_event(on_data)?;
        } else if let Some(data) = self.line.strip_prefix(b"data:") {
            let data = data.strip_prefix(b" ").unwrap_or(data);
            let data = std::str::from_utf8(data)
                .context("failed to decode ChatGPT Codex response stream as UTF-8")?;
            if self.has_event_data {
                self.event_data.push('\n');
            }
            self.event_data.push_str(data);
            self.has_event_data = true;
        }

        self.line.clear();
        Ok(())
    }

    fn flush_event(&mut self, on_data: &mut impl FnMut(&str) -> Result<()>) -> Result<()> {
        if !self.has_event_data {
            return Ok(());
        }

        on_data(&self.event_data)?;
        self.event_data.clear();
        self.has_event_data = false;
        Ok(())
    }
}

#[derive(Default)]
struct AssistantText {
    text: String,
    fallback_text: String,
    tool_calls: Vec<ModelToolCall>,
    response_id: Option<String>,
    usage: Option<TokenUsage>,
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
                if let Some(tool_call) = tool_call_from_item(event.item) {
                    self.tool_calls.push(tool_call);
                } else {
                    self.fallback_text
                        .push_str(&assistant_text_from_item(event.item));
                }
            }
            "response.output_item.done" => {
                if let Some(tool_call) = tool_call_from_item(event.item) {
                    self.tool_calls.push(tool_call);
                }
            }
            "response.failed" => {
                let message = response_error_message(event.response)
                    .unwrap_or_else(|| "response failed".to_string());
                anyhow::bail!("{message}");
            }
            "response.completed" => {
                let metadata = response_metadata(event.response);
                self.response_id = metadata.response_id.or_else(|| self.response_id.take());
                self.usage = metadata.usage.or(self.usage);
                self.completed = true;
            }
            _ => {}
        }

        Ok(())
    }

    fn finish(mut self, on_delta: &mut impl FnMut(&str) -> Result<()>) -> Result<StreamCompletion> {
        anyhow::ensure!(self.completed, "response stream ended before completion");
        if self.text.is_empty() {
            self.text = self.fallback_text;
            if !self.text.is_empty() {
                on_delta(&self.text)?;
            }
        }
        anyhow::ensure!(
            !self.text.trim().is_empty() || !self.tool_calls.is_empty(),
            "assistant response was empty"
        );
        Ok(StreamCompletion {
            text: self.text,
            tool_calls: self.tool_calls,
            response_id: self.response_id,
            usage: self.usage,
        })
    }
}

#[derive(Debug, PartialEq, Eq)]
struct StreamCompletion {
    text: String,
    tool_calls: Vec<ModelToolCall>,
    response_id: Option<String>,
    usage: Option<TokenUsage>,
}

#[derive(Default)]
struct ResponseMetadata {
    response_id: Option<String>,
    usage: Option<TokenUsage>,
}

fn response_metadata(response: Option<&RawValue>) -> ResponseMetadata {
    let Some(response) = response else {
        return ResponseMetadata::default();
    };
    let Ok(response) = serde_json::from_str::<CompletedResponse>(response.get()) else {
        return ResponseMetadata::default();
    };
    ResponseMetadata {
        response_id: response.id.map(Cow::into_owned),
        usage: response.usage.map(ResponseUsage::into_token_usage),
    }
}

#[derive(Debug, Deserialize)]
struct CompletedResponse<'a> {
    #[serde(borrow)]
    id: Option<Cow<'a, str>>,
    usage: Option<ResponseUsage>,
}

#[derive(Clone, Copy, Debug, Deserialize)]
struct ResponseUsage {
    #[serde(default, alias = "prompt_tokens")]
    input_tokens: Option<u64>,
    #[serde(default, alias = "completion_tokens")]
    output_tokens: Option<u64>,
    #[serde(default)]
    total_tokens: Option<u64>,
    #[serde(default, alias = "cached_prompt_tokens")]
    cached_input_tokens: Option<u64>,
    #[serde(default, alias = "prompt_tokens_details")]
    input_tokens_details: Option<InputTokenDetails>,
    #[serde(default, alias = "completion_tokens_details")]
    output_tokens_details: Option<OutputTokenDetails>,
}

impl ResponseUsage {
    fn into_token_usage(self) -> TokenUsage {
        let cached_input_tokens = self.cached_input_tokens.or_else(|| {
            self.input_tokens_details
                .and_then(|details| details.cached_tokens)
        });
        let reasoning_output_tokens = self
            .output_tokens_details
            .and_then(|details| details.reasoning_tokens);
        TokenUsage::new(
            self.input_tokens,
            cached_input_tokens,
            self.output_tokens,
            reasoning_output_tokens,
            self.total_tokens,
        )
    }
}

#[derive(Clone, Copy, Debug, Deserialize)]
struct InputTokenDetails {
    #[serde(default, alias = "cached_input_tokens")]
    cached_tokens: Option<u64>,
}

#[derive(Clone, Copy, Debug, Deserialize)]
struct OutputTokenDetails {
    #[serde(default, alias = "reasoning_output_tokens")]
    reasoning_tokens: Option<u64>,
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
    #[serde(rename = "type", borrow)]
    kind: Option<Cow<'a, str>>,
    #[serde(borrow)]
    role: Option<Cow<'a, str>>,
    #[serde(default, borrow)]
    id: Option<Cow<'a, str>>,
    #[serde(default, borrow)]
    call_id: Option<Cow<'a, str>>,
    #[serde(default, borrow)]
    name: Option<Cow<'a, str>>,
    #[serde(default, borrow)]
    arguments: Option<Cow<'a, str>>,
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

fn tool_call_from_item(item: Option<&RawValue>) -> Option<ModelToolCall> {
    let item = item?;
    let item = serde_json::from_str::<OutputItem>(item.get()).ok()?;
    if item.kind.as_deref() != Some("function_call") {
        return None;
    }
    Some(ModelToolCall {
        item_id: item.id.map(Cow::into_owned),
        call_id: item.call_id.map(Cow::into_owned)?,
        name: item.name.map(Cow::into_owned)?,
        arguments: item.arguments.map(Cow::into_owned).unwrap_or_default(),
    })
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
    use base64::Engine;
    use serde_json::json;
    use std::path::Path;
    use std::path::PathBuf;
    use std::time::Duration;
    use std::time::Instant;
    use std::time::SystemTime;
    use std::time::UNIX_EPOCH;

    use crate::bench_support::DurationSummary;
    use crate::test_http::TestResponse;
    use crate::test_http::TestServer;
    use crate::tools::ToolRegistry;

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
    fn serializes_function_call_history() {
        let messages = [
            ConversationMessage::user("read it"),
            ConversationMessage::function_call(
                Some("fc_1"),
                "call_1",
                "read_file",
                r#"{"path":"Cargo.toml"}"#,
            ),
            ConversationMessage::function_output("call_1", "manifest", true),
        ];
        let input = responses_input(&messages);

        assert_eq!(
            serde_json::to_value(input).unwrap(),
            json!([
                {
                    "type": "message",
                    "role": "user",
                    "content": [{"type": "input_text", "text": "read it"}],
                },
                {
                    "type": "function_call",
                    "id": "fc_1",
                    "call_id": "call_1",
                    "name": "read_file",
                    "arguments": "{\"path\":\"Cargo.toml\"}",
                },
                {
                    "type": "function_call_output",
                    "call_id": "call_1",
                    "output": "manifest",
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
    fn extracts_tool_calls_from_done_items() {
        let stream = r#"event: response.output_item.done
data: {"type":"response.output_item.done","item":{"type":"function_call","id":"fc_1","call_id":"call_1","name":"read_file","arguments":"{\"path\":\"Cargo.toml\"}"}}

event: response.completed
data: {"type":"response.completed","response":{"id":"resp_1"}}

"#;

        let completion =
            read_assistant_completion(std::io::Cursor::new(stream.as_bytes()), |_| Ok(())).unwrap();

        assert_eq!(completion.text, "");
        assert_eq!(
            completion.tool_calls,
            [ModelToolCall {
                item_id: Some("fc_1".to_string()),
                call_id: "call_1".to_string(),
                name: "read_file".to_string(),
                arguments: r#"{"path":"Cargo.toml"}"#.to_string(),
            }]
        );
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

    #[test]
    fn extracts_response_cache_metadata() {
        let stream = r#"event: response.output_text.delta
data: {"type":"response.output_text.delta","delta":"hello"}

event: response.completed
data: {"type":"response.completed","response":{"id":"resp_1","usage":{"input_tokens":100,"input_tokens_details":{"cached_tokens":64},"output_tokens":7,"output_tokens_details":{"reasoning_tokens":3},"total_tokens":107}}}

"#;

        let completion =
            read_assistant_completion(std::io::Cursor::new(stream.as_bytes()), |_| Ok(())).unwrap();

        assert_eq!(completion.text, "hello");
        assert_eq!(completion.response_id.as_deref(), Some("resp_1"));
        assert_eq!(
            completion.usage,
            Some(TokenUsage::new(
                Some(100),
                Some(64),
                Some(7),
                Some(3),
                Some(107)
            ))
        );
    }

    #[test]
    fn stable_prefix_hash_changes_with_instructions() {
        let first = stable_prefix_stats("You are concise.", &[]);
        let second = stable_prefix_stats("You are verbose.", &[]);

        assert_eq!(first.bytes, "You are concise.".len());
        assert_ne!(first.hash, second.hash);
    }

    #[tokio::test]
    async fn retries_once_with_refreshed_credentials_after_unauthorized() {
        let auth_path = temp_auth_file("client-retry");
        let old_access = access_token(now_unix() + 600);
        let new_access = access_token(now_unix() + 900);
        write_auth_file(&auth_path, "account-old", &old_access, "refresh-old");
        let auth_server = TestServer::new(vec![TestResponse::json(
            200,
            serde_json::json!({
                "id_token": id_token("account-new"),
                "access_token": new_access,
                "refresh_token": "refresh-new"
            })
            .to_string(),
        )]);
        let model_server = TestServer::new(vec![
            TestResponse::json(401, r#"{"error":"expired"}"#),
            TestResponse::sse(
                200,
                "data: {\"type\":\"response.output_text.delta\",\"delta\":\"ok\"}\n\n\
                 data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_1\"}}\n\n",
            ),
        ]);
        let auth = AuthManager::for_test(auth_path.clone(), auth_server.url()).unwrap();
        let client = ChatGptClient::new(
            auth,
            ClientConfig::new(
                "instructions",
                format!("{}/responses", model_server.url()),
                "originator",
                "version",
                Duration::from_secs(5),
                "retry-test",
            ),
        )
        .unwrap();
        let messages = [ConversationMessage::user("hello")];
        let mut deltas = String::new();

        let response = client
            .stream_conversation(
                &messages,
                &[],
                true,
                SessionId::new(7),
                "gpt-test",
                &mut |delta| {
                    deltas.push_str(delta);
                    Ok(())
                },
            )
            .await
            .unwrap();

        assert_eq!(response.text, "ok");
        assert_eq!(deltas, "ok");
        let model_requests = model_server.requests();
        assert_eq!(model_requests.len(), 2);
        assert_eq!(model_requests[0].path, "/responses");
        assert_eq!(model_requests[1].path, "/responses");
        assert!(model_requests[0]
            .headers
            .contains(&format!("Bearer {old_access}")));
        assert!(model_requests[0]
            .headers
            .to_ascii_lowercase()
            .contains("chatgpt-account-id: account-old"));
        assert!(model_requests[1]
            .headers
            .contains(&format!("Bearer {new_access}")));
        assert!(model_requests[1]
            .headers
            .to_ascii_lowercase()
            .contains("chatgpt-account-id: account-new"));
        let auth_requests = auth_server.requests();
        assert_eq!(auth_requests[0].path, "/oauth/token");
        assert!(auth_requests[0]
            .body
            .contains(r#""refresh_token":"refresh-old""#));

        remove_parent(&auth_path);
    }

    #[tokio::test]
    async fn serializes_reasoning_effort_override() {
        let auth_path = temp_auth_file("client-reasoning");
        let access = access_token(now_unix() + 600);
        write_auth_file(&auth_path, "account-test", &access, "refresh-test");
        let model_server = TestServer::new(vec![TestResponse::sse(
            200,
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"ok\"}\n\n\
             data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_1\",\"usage\":{\"input_tokens\":100,\"input_tokens_details\":{\"cached_tokens\":64},\"output_tokens\":7,\"output_tokens_details\":{\"reasoning_tokens\":3},\"total_tokens\":107}}}\n\n",
        )]);
        let auth =
            AuthManager::for_test(auth_path.clone(), "http://127.0.0.1:1".to_string()).unwrap();
        let client = ChatGptClient::new(
            auth,
            ClientConfig::new(
                "instructions",
                format!("{}/responses", model_server.url()),
                "originator",
                "version",
                Duration::from_secs(5),
                "reasoning-test",
            ),
        )
        .unwrap();
        let messages = [ConversationMessage::user("hello")];

        let response = client
            .stream_conversation_with_reasoning(
                &messages,
                &[],
                true,
                SessionId::new(7),
                "gpt-test",
                Some("xhigh"),
                &mut |_| Ok(()),
            )
            .await
            .unwrap();

        let requests = model_server.requests();
        assert_eq!(requests.len(), 1);
        let body: serde_json::Value = serde_json::from_str(&requests[0].body).unwrap();
        assert_eq!(body["reasoning"]["effort"], "xhigh");
        assert_eq!(body["prompt_cache_key"], "reasoning-test-gpt-test");
        assert_eq!(
            response.cache_health.unwrap().usage,
            Some(TokenUsage::new(
                Some(100),
                Some(64),
                Some(7),
                Some(3),
                Some(107)
            ))
        );

        remove_parent(&auth_path);
    }

    #[test]
    #[ignore = "release-mode SSE parser benchmark; run explicitly with --ignored --nocapture"]
    fn benchmark_sse_parser_large_stream() {
        const EVENTS: usize = 20_000;
        const SAMPLES: usize = 15;
        const CHUNK_BYTES: usize = 8 * 1024;

        let stream = large_delta_stream(EVENTS);
        let expected_text_bytes = "hello world".len() * EVENTS;
        let mut samples = Vec::with_capacity(SAMPLES);

        for _ in 0..SAMPLES {
            let started = Instant::now();
            let completion = parse_assistant_completion_from_chunks(stream.as_bytes(), CHUNK_BYTES)
                .expect("benchmark stream should parse");
            let elapsed = started.elapsed();

            std::hint::black_box(&completion.text);
            assert_eq!(completion.text.len(), expected_text_bytes);
            assert_eq!(completion.response_id.as_deref(), Some("resp_bench"));
            samples.push(elapsed);
        }

        let summary = DurationSummary::from_samples(&mut samples);
        let throughput_mib_s = stream.len() as f64 / summary.median.as_secs_f64() / 1024.0 / 1024.0;

        println!(
            "sse_parser_large_stream events={EVENTS} bytes={} samples={SAMPLES} chunk_bytes={CHUNK_BYTES} min_ms={:.3} median_ms={:.3} max_ms={:.3} throughput_mib_s={:.1}",
            stream.len(),
            summary.min_ms(),
            summary.median_ms(),
            summary.max_ms(),
            throughput_mib_s,
        );
    }

    #[test]
    #[ignore = "release-mode Responses request serialization benchmark; run explicitly with --ignored --nocapture"]
    fn benchmark_responses_request_serialization_large_history() {
        const GROUPS: usize = 1_200;
        const SAMPLES: usize = 15;

        let owned = large_request_history(GROUPS);
        let messages = owned
            .iter()
            .map(BenchMessage::as_conversation_message)
            .collect::<Vec<_>>();
        let tools = ToolRegistry::default();
        let prompt_cache_key = "bench-serialization-7-gpt-bench";
        let request = ResponsesRequest {
            model: "gpt-bench",
            instructions: "You are a concise assistant for benchmark serialization.",
            input: responses_input(&messages),
            tools: tools.specs(),
            tool_choice: "auto",
            parallel_tool_calls: true,
            reasoning: None,
            store: false,
            stream: true,
            prompt_cache_key,
        };
        let mut samples = Vec::with_capacity(SAMPLES);
        let mut serialized_bytes = 0usize;

        for _ in 0..SAMPLES {
            let started = Instant::now();
            let serialized =
                serde_json::to_vec(&request).expect("benchmark request should serialize");
            let elapsed = started.elapsed();

            serialized_bytes = serialized.len();
            std::hint::black_box(&serialized);
            assert!(serialized_bytes > 1_000_000);
            samples.push(elapsed);
        }

        let summary = DurationSummary::from_samples(&mut samples);
        let throughput_mib_s =
            serialized_bytes as f64 / summary.median.as_secs_f64() / 1024.0 / 1024.0;

        println!(
            "responses_request_serialization_large_history groups={GROUPS} messages={} bytes={serialized_bytes} samples={SAMPLES} min_ms={:.3} median_ms={:.3} max_ms={:.3} throughput_mib_s={:.1}",
            messages.len(),
            summary.min_ms(),
            summary.median_ms(),
            summary.max_ms(),
            throughput_mib_s,
        );
    }

    fn parse_assistant_completion_from_chunks(
        bytes: &[u8],
        chunk_bytes: usize,
    ) -> Result<StreamCompletion> {
        let mut state = AssistantText::default();
        let mut parser = SseDataParser::default();

        for chunk in bytes.chunks(chunk_bytes) {
            parser.push_bytes(chunk, &mut |data| state.handle_data(data, &mut |_| Ok(())))?;
        }

        parser.finish(&mut |data| state.handle_data(data, &mut |_| Ok(())))?;
        state.finish(&mut |_| Ok(()))
    }

    fn large_delta_stream(events: usize) -> String {
        let mut stream = String::with_capacity(events * 72 + 90);
        for _ in 0..events {
            stream.push_str(
                "event: response.output_text.delta\n\
                 data: {\"type\":\"response.output_text.delta\",\"delta\":\"hello world\"}\n\n",
            );
        }
        stream.push_str(
            "event: response.completed\n\
             data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_bench\"}}\n\n",
        );
        stream
    }

    fn now_unix() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
    }

    fn write_auth_file(path: &Path, account_id: &str, access_token: &str, refresh_token: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let auth_file = serde_json::json!({
            "tokens": {
                "id_token": id_token(account_id),
                "access_token": access_token,
                "refresh_token": refresh_token,
                "account_id": account_id
            },
            "last_refresh_unix": now_unix()
        });
        std::fs::write(path, serde_json::to_vec(&auth_file).unwrap()).unwrap();
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

    fn jwt(payload: &serde_json::Value) -> String {
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
                "rust-agent-client-{name}-{}-{unique}",
                std::process::id()
            ))
            .join("auth.json")
    }

    fn remove_parent(path: &Path) {
        if let Some(parent) = path.parent() {
            let _ = std::fs::remove_dir_all(parent);
        }
    }

    enum BenchMessage {
        User(String),
        Assistant(String),
        FunctionCall {
            item_id: String,
            call_id: String,
            arguments: String,
        },
        FunctionOutput {
            call_id: String,
            output: String,
        },
    }

    impl BenchMessage {
        fn as_conversation_message(&self) -> ConversationMessage<'_> {
            match self {
                Self::User(text) => ConversationMessage::user(text),
                Self::Assistant(text) => ConversationMessage::assistant(text),
                Self::FunctionCall {
                    item_id,
                    call_id,
                    arguments,
                } => ConversationMessage::function_call(
                    Some(item_id.as_str()),
                    call_id,
                    "read_file",
                    arguments,
                ),
                Self::FunctionOutput { call_id, output } => {
                    ConversationMessage::function_output(call_id, output, true)
                }
            }
        }
    }

    fn large_request_history(groups: usize) -> Vec<BenchMessage> {
        let mut messages = Vec::with_capacity(groups * 4);
        for index in 0..groups {
            messages.push(BenchMessage::User(format!(
                "Inspect file group {index} and summarize the relevant implementation details."
            )));
            messages.push(BenchMessage::FunctionCall {
                item_id: format!("fc_{index}"),
                call_id: format!("call_{index}"),
                arguments: format!(r#"{{"path":"src/generated/bench_{index}.rs"}}"#),
            });
            messages.push(BenchMessage::FunctionOutput {
                call_id: format!("call_{index}"),
                output: "fn bench_target() { println!(\"benchmark payload\"); }\n".repeat(12),
            });
            messages.push(BenchMessage::Assistant(format!(
                "Group {index} contains a small benchmark target function."
            )));
        }
        messages
    }
}
