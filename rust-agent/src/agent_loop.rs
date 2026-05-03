//! In-memory agent loop for ordered session turns.

use std::future::Future;
use std::ops::Range;

use anyhow::Result;
use futures_util::future::join_all;

use crate::client::ConversationMessage;
use crate::config::ContextWindowConfig;
use crate::tools::ToolExecution;
use crate::tools::ToolRegistry;
use crate::tools::ToolSpec;

const MAX_TOOL_ROUNDS: usize = 8;

/// Strong identifier for an agent session.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct SessionId(u64);

impl SessionId {
    /// Creates a session identifier from a stable numeric value.
    pub(crate) const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Returns the numeric session identifier.
    pub(crate) const fn get(self) -> u64 {
        self.0
    }
}

/// Strong identifier for a session message.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct MessageId(u64);

impl MessageId {
    const fn next(self) -> Self {
        Self(self.0 + 1)
    }
}

/// Role of a stored agent-loop message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MessageRole {
    User,
    Assistant,
}

impl MessageRole {
    /// Returns the wire label for this role.
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Assistant => "assistant",
        }
    }
}

/// Current execution state for a session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SessionStatus {
    Idle,
    Running,
    Failed,
}

impl SessionStatus {
    /// Returns the wire label for this status.
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Running => "running",
            Self::Failed => "failed",
        }
    }
}

/// Message stored in the current session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AgentMessage {
    id: MessageId,
    item: AgentItem,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum AgentItem {
    Message {
        role: MessageRole,
        text: String,
    },
    FunctionCall {
        item_id: Option<String>,
        call_id: String,
        name: String,
        arguments: String,
    },
    FunctionOutput {
        call_id: String,
        output: String,
        success: bool,
    },
}

impl AgentItem {
    fn is_tool_transcript_item(&self) -> bool {
        matches!(
            self,
            Self::FunctionCall { .. } | Self::FunctionOutput { .. }
        )
    }
}

impl AgentMessage {
    /// Returns the message role.
    #[cfg(test)]
    pub(crate) fn role(&self) -> MessageRole {
        match &self.item {
            AgentItem::Message { role, .. } => *role,
            AgentItem::FunctionCall { .. } | AgentItem::FunctionOutput { .. } => {
                panic!("tool transcript items do not have user or assistant roles")
            }
        }
    }

    /// Returns the message text.
    pub(crate) fn text(&self) -> &str {
        match &self.item {
            AgentItem::Message { text, .. } => text,
            AgentItem::FunctionCall { arguments, .. } => arguments,
            AgentItem::FunctionOutput { output, .. } => output,
        }
    }

    fn conversation_message(&self) -> ConversationMessage<'_> {
        match &self.item {
            AgentItem::Message { role, text } => match role {
                MessageRole::User => ConversationMessage::user(text),
                MessageRole::Assistant => ConversationMessage::assistant(text),
            },
            AgentItem::FunctionCall {
                item_id,
                call_id,
                name,
                arguments,
            } => ConversationMessage::function_call(item_id.as_deref(), call_id, name, arguments),
            AgentItem::FunctionOutput {
                call_id,
                output,
                success,
            } => ConversationMessage::function_output(call_id, output, *success),
        }
    }

    fn input_bytes(&self) -> usize {
        match &self.item {
            AgentItem::Message { text, .. } => text.len(),
            AgentItem::FunctionCall {
                item_id,
                call_id,
                name,
                arguments,
            } => {
                item_id.as_deref().map_or(0, str::len)
                    + call_id.len()
                    + name.len()
                    + arguments.len()
            }
            AgentItem::FunctionOutput {
                call_id, output, ..
            } => call_id.len() + output.len(),
        }
    }
}

/// Event emitted by the agent loop while processing a turn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AgentEvent<'a> {
    StatusChanged {
        session_id: SessionId,
        status: SessionStatus,
    },
    TextDelta {
        session_id: SessionId,
        delta: &'a str,
    },
    MessageCompleted {
        session_id: SessionId,
        message_id: MessageId,
        role: MessageRole,
        text: &'a str,
    },
    CacheHealth {
        session_id: SessionId,
        cache_health: &'a CacheHealth,
    },
    ToolCallStarted {
        session_id: SessionId,
        tool_call_id: &'a str,
        tool_name: &'a str,
    },
    ToolCallCompleted {
        session_id: SessionId,
        tool_call_id: &'a str,
        tool_name: &'a str,
        success: bool,
    },
    Error {
        session_id: SessionId,
        message: &'a str,
    },
}

/// Provider token usage reported for one completed model response.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct TokenUsage {
    pub(crate) input_tokens: Option<u64>,
    pub(crate) cached_input_tokens: Option<u64>,
    pub(crate) output_tokens: Option<u64>,
    pub(crate) total_tokens: Option<u64>,
}

impl TokenUsage {
    /// Creates token usage from provider-reported counters.
    pub(crate) const fn new(
        input_tokens: Option<u64>,
        cached_input_tokens: Option<u64>,
        output_tokens: Option<u64>,
        total_tokens: Option<u64>,
    ) -> Self {
        Self {
            input_tokens,
            cached_input_tokens,
            output_tokens,
            total_tokens,
        }
    }

    /// Returns the ratio of cached input tokens to all input tokens.
    pub(crate) fn cache_hit_ratio(self) -> Option<f64> {
        let input_tokens = self.input_tokens?;
        if input_tokens == 0 {
            return None;
        }
        Some(self.cached_input_tokens.unwrap_or(0) as f64 / input_tokens as f64)
    }
}

/// Whether the cacheable request shape matches the previous turn in this session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CacheStatus {
    FirstRequest,
    ReusedPrefix,
    CacheKeyChanged,
    StablePrefixChanged,
}

impl CacheStatus {
    /// Returns a stable telemetry label.
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::FirstRequest => "first_request",
            Self::ReusedPrefix => "reused_prefix",
            Self::CacheKeyChanged => "cache_key_changed",
            Self::StablePrefixChanged => "stable_prefix_changed",
        }
    }
}

/// Cache-related telemetry for one model request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CacheHealth {
    pub(crate) model: String,
    pub(crate) prompt_cache_key: String,
    pub(crate) stable_prefix_hash: u64,
    pub(crate) stable_prefix_bytes: usize,
    pub(crate) message_count: usize,
    pub(crate) input_bytes: usize,
    pub(crate) response_id: Option<String>,
    pub(crate) usage: Option<TokenUsage>,
    pub(crate) cache_status: CacheStatus,
}

/// Completed model response plus optional provider telemetry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ModelResponse {
    pub(crate) text: String,
    pub(crate) tool_calls: Vec<ModelToolCall>,
    pub(crate) cache_health: Option<CacheHealth>,
}

impl ModelResponse {
    /// Creates a model response without provider telemetry.
    #[cfg(test)]
    pub(crate) fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            tool_calls: Vec::new(),
            cache_health: None,
        }
    }

    /// Creates a model response containing tool calls.
    #[cfg(test)]
    pub(crate) fn with_tool_calls(
        text: impl Into<String>,
        tool_calls: impl IntoIterator<Item = ModelToolCall>,
    ) -> Self {
        Self {
            text: text.into(),
            tool_calls: tool_calls.into_iter().collect(),
            cache_health: None,
        }
    }

    /// Creates a model response with cache telemetry.
    pub(crate) fn with_cache_health(
        text: impl Into<String>,
        tool_calls: impl IntoIterator<Item = ModelToolCall>,
        cache_health: CacheHealth,
    ) -> Self {
        Self {
            text: text.into(),
            tool_calls: tool_calls.into_iter().collect(),
            cache_health: Some(cache_health),
        }
    }
}

/// Completed model tool call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ModelToolCall {
    pub(crate) item_id: Option<String>,
    pub(crate) call_id: String,
    pub(crate) name: String,
    pub(crate) arguments: String,
}

/// Streams a prompt window into an assistant response.
pub(crate) trait ModelStreamer {
    /// Sends the prompt window and streams assistant text deltas.
    fn stream_conversation<'a>(
        &'a self,
        messages: &'a [ConversationMessage<'a>],
        tools: &'a [ToolSpec],
        parallel_tool_calls: bool,
        session_id: SessionId,
        model: &'a str,
        on_delta: &'a mut (dyn FnMut(&str) -> Result<()> + Send),
    ) -> impl Future<Output = Result<ModelResponse>> + Send + 'a;
}

/// Per-session runtime settings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SessionConfig {
    model: String,
}

impl SessionConfig {
    /// Creates session settings with an initial model.
    pub(crate) fn new(model: impl Into<String>) -> Self {
        Self {
            model: model.into(),
        }
    }

    /// Returns the selected model for future turns.
    pub(crate) fn model(&self) -> &str {
        &self.model
    }

    fn set_model(&mut self, model: impl Into<String>) {
        self.model = model.into();
    }
}

/// In-memory storage for one session.
#[derive(Debug)]
pub(crate) struct InMemorySessionStore {
    session_id: SessionId,
    status: SessionStatus,
    config: SessionConfig,
    next_message_id: MessageId,
    messages: Vec<AgentMessage>,
    last_cache_observation: Option<CacheObservation>,
}

impl InMemorySessionStore {
    /// Creates empty session storage.
    pub(crate) fn new(session_id: SessionId, config: SessionConfig) -> Self {
        Self {
            session_id,
            status: SessionStatus::Idle,
            config,
            next_message_id: MessageId(1),
            messages: Vec::new(),
            last_cache_observation: None,
        }
    }

    /// Returns the session identifier.
    pub(crate) const fn session_id(&self) -> SessionId {
        self.session_id
    }

    /// Returns the current session status.
    pub(crate) const fn status(&self) -> SessionStatus {
        self.status
    }

    /// Returns the selected model for this session.
    pub(crate) fn model(&self) -> &str {
        self.config.model()
    }

    /// Returns stored messages in order.
    pub(crate) fn messages(&self) -> &[AgentMessage] {
        &self.messages
    }

    fn set_status(&mut self, status: SessionStatus) {
        self.status = status;
    }

    fn set_model(&mut self, model: impl Into<String>) {
        self.config.set_model(model);
    }

    fn append_message(&mut self, role: MessageRole, text: impl Into<String>) -> MessageId {
        self.append_item(AgentItem::Message {
            role,
            text: text.into(),
        })
    }

    fn append_tool_call(&mut self, tool_call: ModelToolCall) -> MessageId {
        self.append_item(AgentItem::FunctionCall {
            item_id: tool_call.item_id,
            call_id: tool_call.call_id,
            name: tool_call.name,
            arguments: tool_call.arguments,
        })
    }

    fn append_tool_output(&mut self, execution: ToolExecution) -> MessageId {
        self.append_item(AgentItem::FunctionOutput {
            call_id: execution.call_id,
            output: execution.output,
            success: execution.success,
        })
    }

    fn append_tool_transcript(
        &mut self,
        tool_calls: impl IntoIterator<Item = ModelToolCall>,
        executions: impl IntoIterator<Item = ToolExecution>,
    ) {
        for tool_call in tool_calls {
            self.append_tool_call(tool_call);
        }
        for execution in executions {
            self.append_tool_output(execution);
        }
    }

    fn append_item(&mut self, item: AgentItem) -> MessageId {
        let id = self.next_message_id;
        self.next_message_id = self.next_message_id.next();
        self.messages.push(AgentMessage { id, item });
        id
    }

    fn remove_last_message(&mut self, message_id: MessageId) {
        let Some(message) = self.messages.last() else {
            return;
        };
        if message.id == message_id {
            self.messages.pop();
            self.next_message_id = message_id;
        }
    }

    fn conversation_window(&self, config: ContextWindowConfig) -> Vec<ConversationMessage<'_>> {
        let ranges = self.retained_ranges(config.max_messages(), config.max_bytes());
        let retained_count = ranges.iter().map(|range| range.end - range.start).sum();
        let mut retained = Vec::with_capacity(retained_count);

        for range in ranges {
            for message in &self.messages[range] {
                retained.push(message.conversation_message());
            }
        }
        retained
    }

    fn prune_history(&mut self, config: ContextWindowConfig) {
        let ranges =
            self.retained_ranges(config.history_max_messages(), config.history_max_bytes());
        let first_retained = ranges
            .first()
            .map_or(self.messages.len(), |range| range.start);

        if first_retained > 0 {
            self.messages.drain(0..first_retained);
        }
    }

    fn retained_ranges(&self, max_messages: usize, max_bytes: usize) -> Vec<Range<usize>> {
        let mut ranges = Vec::new();
        let mut retained_messages = 0usize;
        let mut retained_bytes = 0usize;
        let mut end = self.messages.len();

        while end > 0 {
            let start = self.retention_unit_start_before(end);
            let unit_messages = end - start;
            let unit_bytes = self.messages[start..end]
                .iter()
                .map(AgentMessage::input_bytes)
                .sum::<usize>();
            let would_exceed_messages =
                retained_messages.saturating_add(unit_messages) > max_messages;
            let would_exceed_bytes = retained_bytes.saturating_add(unit_bytes) > max_bytes;
            if (would_exceed_messages || would_exceed_bytes) && !ranges.is_empty() {
                break;
            }

            ranges.push(start..end);
            retained_messages = retained_messages.saturating_add(unit_messages);
            retained_bytes = retained_bytes.saturating_add(unit_bytes);
            end = start;
        }

        ranges.reverse();
        ranges
    }

    fn retention_unit_start_before(&self, end: usize) -> usize {
        let mut start = end - 1;
        if self.messages[start].item.is_tool_transcript_item() {
            while start > 0 && self.messages[start - 1].item.is_tool_transcript_item() {
                start -= 1;
            }
        }
        start
    }

    fn cache_status(&self, cache_health: &CacheHealth) -> CacheStatus {
        let Some(previous) = &self.last_cache_observation else {
            return CacheStatus::FirstRequest;
        };
        if previous.prompt_cache_key != cache_health.prompt_cache_key {
            return CacheStatus::CacheKeyChanged;
        }
        if previous.stable_prefix_hash != cache_health.stable_prefix_hash {
            return CacheStatus::StablePrefixChanged;
        }
        CacheStatus::ReusedPrefix
    }

    fn record_cache_observation(&mut self, cache_health: &CacheHealth) {
        self.last_cache_observation = Some(CacheObservation {
            prompt_cache_key: cache_health.prompt_cache_key.clone(),
            stable_prefix_hash: cache_health.stable_prefix_hash,
        });
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct CacheObservation {
    prompt_cache_key: String,
    stable_prefix_hash: u64,
}

/// Runs ordered turns for a single in-memory session.
#[derive(Debug)]
pub(crate) struct AgentLoop {
    store: InMemorySessionStore,
    context_window: ContextWindowConfig,
    tools: ToolRegistry,
}

impl AgentLoop {
    /// Creates an agent loop for one session.
    #[cfg(test)]
    pub(crate) fn new(session_id: SessionId) -> Self {
        Self::with_context_window(
            session_id,
            ContextWindowConfig::default(),
            SessionConfig::new("test-model"),
        )
    }

    /// Creates an agent loop with explicit context-window bounds.
    #[cfg(test)]
    pub(crate) fn with_context_window(
        session_id: SessionId,
        context_window: ContextWindowConfig,
        config: SessionConfig,
    ) -> Self {
        Self::with_context_window_and_tools(
            session_id,
            context_window,
            config,
            ToolRegistry::default(),
        )
    }

    /// Creates an agent loop with explicit context bounds and tool registry.
    pub(crate) fn with_context_window_and_tools(
        session_id: SessionId,
        context_window: ContextWindowConfig,
        config: SessionConfig,
        tools: ToolRegistry,
    ) -> Self {
        Self {
            store: InMemorySessionStore::new(session_id, config),
            context_window,
            tools,
        }
    }

    /// Returns the session identifier.
    pub(crate) const fn session_id(&self) -> SessionId {
        self.store.session_id()
    }

    /// Returns the selected model for future turns.
    pub(crate) fn model(&self) -> &str {
        self.store.model()
    }

    /// Returns stored messages in order.
    #[cfg(test)]
    pub(crate) fn messages(&self) -> &[AgentMessage] {
        self.store.messages()
    }

    /// Changes the selected model for future turns.
    ///
    /// # Errors
    /// Returns an error if a turn is currently running.
    pub(crate) fn set_model(&mut self, model: impl Into<String>) -> Result<()> {
        anyhow::ensure!(
            self.store.status() != SessionStatus::Running,
            "cannot change model while session is running"
        );
        self.store.set_model(model);
        Ok(())
    }

    /// Appends a user message, streams the model response, and stores the assistant message.
    ///
    /// # Errors
    /// Returns an error if the session is already running, model streaming fails, or event publishing fails.
    pub(crate) async fn submit_user_message(
        &mut self,
        text: impl Into<String>,
        model: &impl ModelStreamer,
        mut emit: impl FnMut(AgentEvent<'_>) -> Result<()> + Send,
    ) -> Result<()> {
        anyhow::ensure!(
            self.store.status() != SessionStatus::Running,
            "session is already running"
        );

        let user_text = text.into();
        let user_id = self.store.append_message(MessageRole::User, user_text);
        if let Err(error) = emit(AgentEvent::MessageCompleted {
            session_id: self.session_id(),
            message_id: user_id,
            role: MessageRole::User,
            text: self
                .store
                .messages()
                .last()
                .map(AgentMessage::text)
                .unwrap_or_default(),
        }) {
            self.store.remove_last_message(user_id);
            return Err(error);
        }

        if let Err(error) = self.begin_running(&mut emit) {
            self.store.remove_last_message(user_id);
            return Err(error);
        }
        self.store.prune_history(self.context_window);
        let result = self.run_until_done(model, &mut emit).await;
        match result {
            Ok(()) => {
                self.set_status(SessionStatus::Idle, &mut emit)?;
                Ok(())
            }
            Err(error) => {
                self.store.prune_history(self.context_window);
                self.store.set_status(SessionStatus::Failed);
                let message = error.to_string();
                emit(AgentEvent::StatusChanged {
                    session_id: self.session_id(),
                    status: SessionStatus::Failed,
                })?;
                emit(AgentEvent::Error {
                    session_id: self.session_id(),
                    message: &message,
                })?;
                Err(error)
            }
        }
    }

    fn begin_running(
        &mut self,
        emit: &mut (impl FnMut(AgentEvent<'_>) -> Result<()> + Send),
    ) -> Result<()> {
        self.store.set_status(SessionStatus::Running);
        if let Err(error) = emit(AgentEvent::StatusChanged {
            session_id: self.session_id(),
            status: SessionStatus::Running,
        }) {
            self.store.set_status(SessionStatus::Idle);
            return Err(error);
        }
        Ok(())
    }

    fn set_status(
        &mut self,
        status: SessionStatus,
        emit: &mut (impl FnMut(AgentEvent<'_>) -> Result<()> + Send),
    ) -> Result<()> {
        self.store.set_status(status);
        emit(AgentEvent::StatusChanged {
            session_id: self.session_id(),
            status,
        })
    }

    async fn run_until_done(
        &mut self,
        model: &impl ModelStreamer,
        emit: &mut (impl FnMut(AgentEvent<'_>) -> Result<()> + Send),
    ) -> Result<()> {
        for _ in 0..MAX_TOOL_ROUNDS {
            let tool_calls = self.run_once(model, emit).await?;
            if tool_calls.is_empty() {
                self.store.prune_history(self.context_window);
                return Ok(());
            }

            let executions = self.execute_tool_calls(&tool_calls, emit).await?;
            self.store.append_tool_transcript(tool_calls, executions);
            self.store.prune_history(self.context_window);
        }

        anyhow::bail!("tool call limit exceeded")
    }

    async fn run_once(
        &mut self,
        model: &impl ModelStreamer,
        emit: &mut (impl FnMut(AgentEvent<'_>) -> Result<()> + Send),
    ) -> Result<Vec<ModelToolCall>> {
        let history = self.store.conversation_window(self.context_window);
        let session_id = self.session_id();
        let selected_model = self.store.model();
        let mut on_delta = |delta: &str| emit(AgentEvent::TextDelta { session_id, delta });
        let mut model_response = model
            .stream_conversation(
                &history,
                self.tools.specs(),
                true,
                session_id,
                selected_model,
                &mut on_delta,
            )
            .await?;
        if let Some(cache_health) = model_response.cache_health.as_mut() {
            cache_health.cache_status = self.store.cache_status(cache_health);
            emit(AgentEvent::CacheHealth {
                session_id,
                cache_health,
            })?;
            self.store.record_cache_observation(cache_health);
        }
        let assistant_text = model_response.text;
        if !assistant_text.is_empty() {
            let assistant_id = self
                .store
                .append_message(MessageRole::Assistant, assistant_text);
            let assistant_text = self
                .store
                .messages()
                .last()
                .map(AgentMessage::text)
                .unwrap_or_default();
            emit(AgentEvent::MessageCompleted {
                session_id,
                message_id: assistant_id,
                role: MessageRole::Assistant,
                text: assistant_text,
            })?;
        }
        Ok(model_response.tool_calls)
    }

    async fn execute_tool_calls(
        &self,
        tool_calls: &[ModelToolCall],
        emit: &mut (impl FnMut(AgentEvent<'_>) -> Result<()> + Send),
    ) -> Result<Vec<ToolExecution>> {
        let session_id = self.session_id();
        for tool_call in tool_calls {
            emit(AgentEvent::ToolCallStarted {
                session_id,
                tool_call_id: &tool_call.call_id,
                tool_name: &tool_call.name,
            })?;
        }

        let executions = if tool_calls
            .iter()
            .all(|call| self.tools.supports_parallel(&call.name))
        {
            join_all(
                tool_calls
                    .iter()
                    .cloned()
                    .map(|tool_call| self.tools.execute(tool_call)),
            )
            .await
        } else {
            let mut executions = Vec::new();
            for tool_call in tool_calls.iter().cloned() {
                executions.push(self.tools.execute(tool_call).await);
            }
            executions
        };

        for execution in &executions {
            emit(AgentEvent::ToolCallCompleted {
                session_id,
                tool_call_id: &execution.call_id,
                tool_name: &execution.tool_name,
                success: execution.success,
            })?;
        }
        Ok(executions)
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Mutex as StdMutex;
    use std::time::Instant;

    use serde_json::json;

    use super::*;
    use crate::bench_support::DurationSummary;
    use crate::tools::ToolPolicy;
    use crate::tools::ToolRegistry;

    #[tokio::test]
    async fn stores_one_turn_and_emits_ordered_events() {
        let mut agent = AgentLoop::new(SessionId::new(7));
        let mut events = Vec::new();
        let streamer = FnStreamer::new(
            |history: &[ConversationMessage<'_>],
             _tools: &[ToolSpec],
             _parallel_tool_calls: bool,
             _model: &str,
             on_delta: &mut (dyn FnMut(&str) -> Result<()> + Send)| {
                assert_eq!(history.len(), 1);
                on_delta("hi")?;
                Ok("hi".to_string())
            },
        );

        agent
            .submit_user_message("hello", &streamer, |event| {
                events.push(format_event(event));
                Ok(())
            })
            .await
            .unwrap();

        assert_eq!(agent.store.status(), SessionStatus::Idle);
        assert_eq!(agent.messages().len(), 2);
        assert_eq!(agent.messages()[0].role(), MessageRole::User);
        assert_eq!(agent.messages()[0].text(), "hello");
        assert_eq!(agent.messages()[1].role(), MessageRole::Assistant);
        assert_eq!(agent.messages()[1].text(), "hi");
        assert_eq!(
            events,
            [
                "message:user:hello",
                "status:Running",
                "delta:hi",
                "message:assistant:hi",
                "status:Idle",
            ]
        );
    }

    #[tokio::test]
    async fn sends_full_history_on_later_turns() {
        let mut agent = AgentLoop::new(SessionId::new(7));
        let turn = AtomicUsize::new(0);
        let streamer = FnStreamer::new(
            |history: &[ConversationMessage<'_>],
             _tools: &[ToolSpec],
             _parallel_tool_calls: bool,
             _model: &str,
             on_delta: &mut (dyn FnMut(&str) -> Result<()> + Send)| {
                match turn.load(Ordering::SeqCst) {
                    0 => {
                        assert_eq!(history.len(), 1);
                        turn.store(1, Ordering::SeqCst);
                        Ok("remembered".to_string())
                    }
                    1 => {
                        assert_eq!(history.len(), 3);
                        on_delta("rust-agent")?;
                        turn.store(2, Ordering::SeqCst);
                        Ok("rust-agent".to_string())
                    }
                    _ => unreachable!("unexpected turn"),
                }
            },
        );

        agent
            .submit_user_message("remember rust-agent", &streamer, |_| Ok(()))
            .await
            .unwrap();

        agent
            .submit_user_message("what did I ask you to remember?", &streamer, |_| Ok(()))
            .await
            .unwrap();

        assert_eq!(agent.messages().len(), 4);
        assert_eq!(agent.messages()[3].text(), "rust-agent");
    }

    #[tokio::test]
    async fn sends_bounded_recent_history() {
        let mut agent = AgentLoop::with_context_window(
            SessionId::new(7),
            ContextWindowConfig::new(3, 24),
            SessionConfig::new("test-model"),
        );
        let turn = AtomicUsize::new(0);
        let streamer = FnStreamer::new(
            |history: &[ConversationMessage<'_>],
             _tools: &[ToolSpec],
             _parallel_tool_calls: bool,
             _model: &str,
             _on_delta: &mut (dyn FnMut(&str) -> Result<()> + Send)| {
                match turn.load(Ordering::SeqCst) {
                    0 => {
                        turn.store(1, Ordering::SeqCst);
                        Ok("first answer".to_string())
                    }
                    1 => {
                        turn.store(2, Ordering::SeqCst);
                        Ok("second answer".to_string())
                    }
                    2 => {
                        assert_eq!(history.len(), 2);
                        assert_eq!(history[0], ConversationMessage::assistant("second answer"));
                        assert_eq!(history[1], ConversationMessage::user("third user"));
                        turn.store(3, Ordering::SeqCst);
                        Ok("third answer".to_string())
                    }
                    _ => unreachable!("unexpected turn"),
                }
            },
        );

        agent
            .submit_user_message("first user", &streamer, |_| Ok(()))
            .await
            .unwrap();
        agent
            .submit_user_message("second user", &streamer, |_| Ok(()))
            .await
            .unwrap();

        agent
            .submit_user_message("third user", &streamer, |_| Ok(()))
            .await
            .unwrap();

        assert_eq!(agent.messages().len(), 6);
        assert_eq!(agent.messages()[5].text(), "third answer");
    }

    #[test]
    fn keeps_tool_transcript_items_atomic_in_context_window() {
        let mut store =
            InMemorySessionStore::new(SessionId::new(7), SessionConfig::new("test-model"));
        store.append_message(MessageRole::User, "older user");
        store.append_tool_call(model_tool_call("call_1"));
        let large_output = "x".repeat(80);
        store.append_tool_output(tool_execution("call_1", large_output.clone()));

        let history = store.conversation_window(ContextWindowConfig::new(1, 1));

        assert_eq!(
            history,
            [
                ConversationMessage::function_call(None, "call_1", "read_file", "{}"),
                ConversationMessage::function_output("call_1", &large_output, true),
            ]
        );
    }

    #[test]
    fn drops_tool_transcript_unit_atomically_when_newer_message_fits() {
        let mut store =
            InMemorySessionStore::new(SessionId::new(7), SessionConfig::new("test-model"));
        store.append_message(MessageRole::User, "older user");
        store.append_tool_call(model_tool_call("call_1"));
        store.append_tool_output(tool_execution("call_1", "x".repeat(80)));
        store.append_message(MessageRole::Assistant, "done");

        let history = store.conversation_window(ContextWindowConfig::new(2, 8));

        assert_eq!(history, [ConversationMessage::assistant("done")]);
    }

    #[test]
    fn prunes_tool_transcript_items_atomically() {
        let mut store =
            InMemorySessionStore::new(SessionId::new(7), SessionConfig::new("test-model"));
        store.append_message(MessageRole::User, "older user");
        store.append_tool_call(model_tool_call("call_1"));
        store.append_tool_output(tool_execution("call_1", "x".repeat(80)));

        store.prune_history(ContextWindowConfig::with_history_limits(8, 1024, 1, 1));

        assert_eq!(store.messages().len(), 2);
        assert!(matches!(
            store.messages()[0].item,
            AgentItem::FunctionCall { .. }
        ));
        assert!(matches!(
            store.messages()[1].item,
            AgentItem::FunctionOutput { .. }
        ));
    }

    #[test]
    #[ignore = "release-mode context-window benchmark; run explicitly with --ignored --nocapture"]
    fn benchmark_context_window_large_tool_history() {
        const GROUPS: usize = 3_000;
        const SAMPLES: usize = 15;

        let config = ContextWindowConfig::with_history_limits(512, 512 * 1024, 512, 512 * 1024);
        let store = large_tool_history_store(GROUPS);
        let stored_messages = store.messages().len();
        let mut window_samples = Vec::with_capacity(SAMPLES);
        let mut retained_messages = 0usize;

        for _ in 0..SAMPLES {
            let started = Instant::now();
            let history = store.conversation_window(config);
            let elapsed = started.elapsed();

            retained_messages = history.len();
            std::hint::black_box(&history);
            assert!(!history.is_empty());
            assert_tool_outputs_have_calls(&history);
            window_samples.push(elapsed);
        }

        let mut prune_samples = Vec::with_capacity(SAMPLES);
        let mut pruned_messages = 0usize;
        for _ in 0..SAMPLES {
            let mut store = large_tool_history_store(GROUPS);
            let started = Instant::now();
            store.prune_history(config);
            let elapsed = started.elapsed();

            pruned_messages = store.messages().len();
            std::hint::black_box(store.messages());
            assert!(!store.messages().is_empty());
            assert_tool_items_are_balanced(store.messages());
            prune_samples.push(elapsed);
        }

        let window = DurationSummary::from_samples(&mut window_samples);
        let prune = DurationSummary::from_samples(&mut prune_samples);
        println!(
            "context_window_large_tool_history groups={GROUPS} stored_messages={stored_messages} retained_messages={retained_messages} pruned_messages={pruned_messages} samples={SAMPLES} window_min_ms={:.3} window_median_ms={:.3} window_max_ms={:.3} prune_min_ms={:.3} prune_median_ms={:.3} prune_max_ms={:.3}",
            window.min_ms(),
            window.median_ms(),
            window.max_ms(),
            prune.min_ms(),
            prune.median_ms(),
            prune.max_ms(),
        );
    }

    #[tokio::test]
    async fn prunes_retained_session_history() {
        let mut agent = AgentLoop::with_context_window(
            SessionId::new(7),
            ContextWindowConfig::with_history_limits(10, 1024, 3, 1024),
            SessionConfig::new("test-model"),
        );
        let turn = AtomicUsize::new(0);
        let streamer = FnStreamer::new(
            |history: &[ConversationMessage<'_>],
             _tools: &[ToolSpec],
             _parallel_tool_calls: bool,
             _model: &str,
             _on_delta: &mut (dyn FnMut(&str) -> Result<()> + Send)| {
                let answer = match turn.fetch_add(1, Ordering::SeqCst) {
                    0 => "first answer",
                    1 => "second answer",
                    2 => {
                        assert_eq!(
                            history,
                            [
                                ConversationMessage::user("second user"),
                                ConversationMessage::assistant("second answer"),
                                ConversationMessage::user("third user"),
                            ]
                        );
                        "third answer"
                    }
                    _ => unreachable!("unexpected turn"),
                };
                Ok(answer.to_string())
            },
        );

        agent
            .submit_user_message("first user", &streamer, |_| Ok(()))
            .await
            .unwrap();
        agent
            .submit_user_message("second user", &streamer, |_| Ok(()))
            .await
            .unwrap();
        agent
            .submit_user_message("third user", &streamer, |_| Ok(()))
            .await
            .unwrap();

        assert_eq!(agent.messages().len(), 3);
        assert_eq!(agent.messages()[0].text(), "second answer");
        assert_eq!(agent.messages()[1].text(), "third user");
        assert_eq!(agent.messages()[2].text(), "third answer");
    }

    #[tokio::test]
    async fn sends_selected_model_to_streamer() {
        let mut agent = AgentLoop::with_context_window(
            SessionId::new(7),
            ContextWindowConfig::default(),
            SessionConfig::new("first-model"),
        );
        let turn = AtomicUsize::new(0);
        let streamer = FnStreamer::new(
            |_history: &[ConversationMessage<'_>],
             _tools: &[ToolSpec],
             _parallel_tool_calls: bool,
             model: &str,
             _on_delta: &mut (dyn FnMut(&str) -> Result<()> + Send)| {
                match turn.load(Ordering::SeqCst) {
                    0 => {
                        assert_eq!(model, "first-model");
                        turn.store(1, Ordering::SeqCst);
                    }
                    1 => {
                        assert_eq!(model, "second-model");
                        turn.store(2, Ordering::SeqCst);
                    }
                    _ => unreachable!("unexpected turn"),
                }
                Ok(model.to_string())
            },
        );

        agent
            .submit_user_message("hello", &streamer, |_| Ok(()))
            .await
            .unwrap();
        agent.set_model("second-model").unwrap();
        agent
            .submit_user_message("again", &streamer, |_| Ok(()))
            .await
            .unwrap();

        assert_eq!(agent.messages()[1].text(), "first-model");
        assert_eq!(agent.messages()[3].text(), "second-model");
        assert_eq!(turn.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn emits_cache_health_status_for_model_responses() {
        let mut agent = AgentLoop::new(SessionId::new(7));
        let streamer = CacheStreamer {
            turn: AtomicUsize::new(0),
        };
        let mut events = Vec::new();

        agent
            .submit_user_message("first", &streamer, |event| {
                events.push(format_event(event));
                Ok(())
            })
            .await
            .unwrap();
        agent
            .submit_user_message("second", &streamer, |event| {
                events.push(format_event(event));
                Ok(())
            })
            .await
            .unwrap();

        assert_eq!(
            events,
            [
                "message:user:first",
                "status:Running",
                "cache:first_request",
                "message:assistant:first answer",
                "status:Idle",
                "message:user:second",
                "status:Running",
                "cache:reused_prefix",
                "message:assistant:second answer",
                "status:Idle",
            ]
        );
    }

    #[tokio::test]
    async fn executes_tool_calls_and_replays_outputs() {
        let mut agent = AgentLoop::new(SessionId::new(7));
        let streamer = ToolLoopStreamer {
            turn: AtomicUsize::new(0),
        };
        let mut events = Vec::new();

        agent
            .submit_user_message("read the manifest", &streamer, |event| {
                events.push(format_event(event));
                Ok(())
            })
            .await
            .unwrap();

        assert_eq!(agent.store.status(), SessionStatus::Idle);
        assert_eq!(agent.messages().last().unwrap().text(), "read ok");
        assert_eq!(
            events,
            [
                "message:user:read the manifest",
                "status:Running",
                "tool-start:read_file:call_read",
                "tool-end:read_file:call_read:true",
                "delta:read ok",
                "message:assistant:read ok",
                "status:Idle",
            ]
        );
    }

    #[tokio::test]
    async fn executes_workspace_patch_tool_calls() {
        let temp = std::env::temp_dir().join(format!(
            "rust-agent-loop-patch-{}-{}",
            std::process::id(),
            unique_nanos()
        ));
        let _ = fs::remove_dir_all(&temp);
        fs::create_dir_all(&temp).unwrap();
        fs::write(
            temp.join("lib.rs"),
            "pub fn value() -> &'static str {\n    \"old\"\n}\n",
        )
        .unwrap();
        let tools = ToolRegistry::for_root_with_policy(&temp, ToolPolicy::WorkspaceWrite);
        let mut agent = AgentLoop::with_context_window_and_tools(
            SessionId::new(7),
            ContextWindowConfig::default(),
            SessionConfig::new("test-model"),
            tools,
        );
        let streamer = PatchLoopStreamer {
            turn: AtomicUsize::new(0),
        };
        let mut events = Vec::new();

        agent
            .submit_user_message("patch lib", &streamer, |event| {
                events.push(format_event(event));
                Ok(())
            })
            .await
            .unwrap();

        assert_eq!(
            fs::read_to_string(temp.join("lib.rs")).unwrap(),
            "pub fn value() -> &'static str {\n    \"new\"\n}\n"
        );
        assert_eq!(
            events,
            [
                "message:user:patch lib",
                "status:Running",
                "tool-start:apply_patch:call_patch",
                "tool-end:apply_patch:call_patch:true",
                "delta:patched",
                "message:assistant:patched",
                "status:Idle",
            ]
        );

        fs::remove_dir_all(&temp).unwrap();
    }

    #[tokio::test]
    async fn tool_event_publish_failure_does_not_leave_orphaned_tool_calls() {
        let mut agent = AgentLoop::new(SessionId::new(7));
        let streamer = RetryAfterToolEventFailureStreamer {
            turn: AtomicUsize::new(0),
        };

        let error = agent
            .submit_user_message("read the manifest", &streamer, |event| {
                if matches!(event, AgentEvent::ToolCallCompleted { .. }) {
                    anyhow::bail!("sink failed");
                }
                Ok(())
            })
            .await
            .unwrap_err()
            .to_string();

        assert_eq!(error, "sink failed");
        assert_eq!(agent.store.status(), SessionStatus::Failed);
        assert!(agent
            .messages()
            .iter()
            .all(|message| matches!(message.item, AgentItem::Message { .. })));

        agent
            .submit_user_message("continue", &streamer, |_| Ok(()))
            .await
            .unwrap();

        assert_eq!(agent.store.status(), SessionStatus::Idle);
        assert_eq!(agent.messages().last().unwrap().text(), "ok");
        assert_eq!(streamer.turn.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn records_failed_status_and_error_event() {
        let mut agent = AgentLoop::new(SessionId::new(7));
        let mut events = Vec::new();
        let streamer = FnStreamer::new(
            |_history: &[ConversationMessage<'_>],
             _tools: &[ToolSpec],
             _parallel_tool_calls: bool,
             _model: &str,
             _on_delta: &mut (dyn FnMut(&str) -> Result<()> + Send)| {
                anyhow::bail!("backend failed")
            },
        );

        let error = agent
            .submit_user_message("hello", &streamer, |event| {
                events.push(format_event(event));
                Ok(())
            })
            .await
            .unwrap_err()
            .to_string();

        assert_eq!(error, "backend failed");
        assert_eq!(agent.store.status(), SessionStatus::Failed);
        assert_eq!(
            events,
            [
                "message:user:hello",
                "status:Running",
                "status:Failed",
                "error:backend failed",
            ]
        );
    }

    #[tokio::test]
    async fn user_message_publish_failure_rolls_back_submission() {
        let mut agent = AgentLoop::new(SessionId::new(7));
        let model_called = AtomicBool::new(false);
        let streamer = FnStreamer::new(
            |_history: &[ConversationMessage<'_>],
             _tools: &[ToolSpec],
             _parallel_tool_calls: bool,
             _model: &str,
             _on_delta: &mut (dyn FnMut(&str) -> Result<()> + Send)| {
                model_called.store(true, Ordering::SeqCst);
                Ok("unused".to_string())
            },
        );

        let error = agent
            .submit_user_message("hello", &streamer, |event| {
                if matches!(
                    event,
                    AgentEvent::MessageCompleted {
                        role: MessageRole::User,
                        ..
                    }
                ) {
                    anyhow::bail!("sink failed");
                }
                Ok(())
            })
            .await
            .unwrap_err()
            .to_string();

        assert_eq!(error, "sink failed");
        assert!(!model_called.load(Ordering::SeqCst));
        assert_eq!(agent.store.status(), SessionStatus::Idle);
        assert!(agent.messages().is_empty());

        let ok_streamer = FnStreamer::new(
            |history: &[ConversationMessage<'_>],
             _tools: &[ToolSpec],
             _parallel_tool_calls: bool,
             _model: &str,
             _on_delta: &mut (dyn FnMut(&str) -> Result<()> + Send)| {
                assert_eq!(history, &[ConversationMessage::user("retry")]);
                Ok("ok".to_string())
            },
        );
        agent
            .submit_user_message("retry", &ok_streamer, |_| Ok(()))
            .await
            .unwrap();

        assert_eq!(agent.messages().len(), 2);
        assert_eq!(agent.messages()[0].text(), "retry");
        assert_eq!(agent.messages()[1].text(), "ok");
    }

    #[tokio::test]
    async fn running_status_publish_failure_does_not_stick_session_running() {
        let mut agent = AgentLoop::new(SessionId::new(7));
        let model_called = AtomicBool::new(false);
        let streamer = FnStreamer::new(
            |_history: &[ConversationMessage<'_>],
             _tools: &[ToolSpec],
             _parallel_tool_calls: bool,
             _model: &str,
             _on_delta: &mut (dyn FnMut(&str) -> Result<()> + Send)| {
                model_called.store(true, Ordering::SeqCst);
                Ok("unused".to_string())
            },
        );

        let error = agent
            .submit_user_message("hello", &streamer, |event| {
                if matches!(
                    event,
                    AgentEvent::StatusChanged {
                        status: SessionStatus::Running,
                        ..
                    }
                ) {
                    anyhow::bail!("sink failed");
                }
                Ok(())
            })
            .await
            .unwrap_err()
            .to_string();

        assert_eq!(error, "sink failed");
        assert!(!model_called.load(Ordering::SeqCst));
        assert_eq!(agent.store.status(), SessionStatus::Idle);
        assert!(agent.messages().is_empty());

        let ok_streamer = FnStreamer::new(
            |history: &[ConversationMessage<'_>],
             _tools: &[ToolSpec],
             _parallel_tool_calls: bool,
             _model: &str,
             _on_delta: &mut (dyn FnMut(&str) -> Result<()> + Send)| {
                assert_eq!(history, &[ConversationMessage::user("again")]);
                Ok("ok".to_string())
            },
        );
        agent
            .submit_user_message("again", &ok_streamer, |_| Ok(()))
            .await
            .unwrap();

        assert_eq!(agent.store.status(), SessionStatus::Idle);
        assert_eq!(agent.messages().len(), 2);
        assert_eq!(agent.messages()[0].text(), "again");
    }

    fn model_tool_call(call_id: &str) -> ModelToolCall {
        ModelToolCall {
            item_id: None,
            call_id: call_id.to_string(),
            name: "read_file".to_string(),
            arguments: "{}".to_string(),
        }
    }

    fn tool_execution(call_id: &str, output: impl Into<String>) -> ToolExecution {
        ToolExecution {
            call_id: call_id.to_string(),
            tool_name: "read_file".to_string(),
            output: output.into(),
            success: true,
        }
    }

    fn large_tool_history_store(groups: usize) -> InMemorySessionStore {
        let mut store =
            InMemorySessionStore::new(SessionId::new(7), SessionConfig::new("test-model"));
        for index in 0..groups {
            let call_id = format!("call_{index}");
            store.append_message(
                MessageRole::User,
                format!("Read and summarize benchmark file {index}."),
            );
            store.append_tool_call(model_tool_call(&call_id));
            store.append_tool_output(tool_execution(
                &call_id,
                "benchmark output line with enough text to make byte budgeting meaningful\n"
                    .repeat(8),
            ));
            store.append_message(
                MessageRole::Assistant,
                format!("Benchmark file {index} was summarized."),
            );
        }
        store
    }

    fn assert_tool_outputs_have_calls(history: &[ConversationMessage<'_>]) {
        let mut call_ids = Vec::new();
        for message in history {
            match message {
                ConversationMessage::FunctionCall { call_id, .. } => call_ids.push(*call_id),
                ConversationMessage::FunctionOutput { call_id, .. } => {
                    assert!(
                        call_ids.iter().any(|seen| seen == call_id),
                        "tool output {call_id} did not have a retained function call"
                    );
                }
                ConversationMessage::Message { .. } => call_ids.clear(),
            }
        }
    }

    fn assert_tool_items_are_balanced(messages: &[AgentMessage]) {
        let mut call_ids = Vec::new();
        for message in messages {
            match &message.item {
                AgentItem::FunctionCall { call_id, .. } => call_ids.push(call_id.as_str()),
                AgentItem::FunctionOutput { call_id, .. } => {
                    assert!(
                        call_ids.iter().any(|seen| seen == call_id),
                        "tool output {call_id} did not have a retained function call"
                    );
                }
                AgentItem::Message { .. } => call_ids.clear(),
            }
        }
    }

    fn format_event(event: AgentEvent<'_>) -> String {
        match event {
            AgentEvent::StatusChanged { status, .. } => format!("status:{status:?}"),
            AgentEvent::TextDelta { delta, .. } => format!("delta:{delta}"),
            AgentEvent::MessageCompleted { role, text, .. } => {
                format!("message:{}:{text}", role_name(role))
            }
            AgentEvent::CacheHealth { cache_health, .. } => {
                format!("cache:{}", cache_health.cache_status.as_str())
            }
            AgentEvent::ToolCallStarted {
                tool_call_id,
                tool_name,
                ..
            } => format!("tool-start:{tool_name}:{tool_call_id}"),
            AgentEvent::ToolCallCompleted {
                tool_call_id,
                tool_name,
                success,
                ..
            } => format!("tool-end:{tool_name}:{tool_call_id}:{success}"),
            AgentEvent::Error { message, .. } => format!("error:{message}"),
        }
    }

    fn role_name(role: MessageRole) -> &'static str {
        match role {
            MessageRole::User => "user",
            MessageRole::Assistant => "assistant",
        }
    }

    fn unique_nanos() -> u128 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    }

    struct FnStreamer<F> {
        stream: StdMutex<F>,
    }

    impl<F> FnStreamer<F> {
        fn new(stream: F) -> Self {
            Self {
                stream: StdMutex::new(stream),
            }
        }
    }

    impl<F> ModelStreamer for FnStreamer<F>
    where
        F: for<'a> FnMut(
                &'a [ConversationMessage<'a>],
                &'a [ToolSpec],
                bool,
                &'a str,
                &'a mut (dyn FnMut(&str) -> Result<()> + Send),
            ) -> Result<String>
            + Send,
    {
        async fn stream_conversation<'a>(
            &'a self,
            messages: &'a [ConversationMessage<'a>],
            tools: &'a [ToolSpec],
            parallel_tool_calls: bool,
            _session_id: SessionId,
            model: &'a str,
            on_delta: &'a mut (dyn FnMut(&str) -> Result<()> + Send),
        ) -> Result<ModelResponse> {
            let mut stream = self.stream.lock().expect("test streamer lock poisoned");
            stream(messages, tools, parallel_tool_calls, model, on_delta).map(ModelResponse::new)
        }
    }

    struct CacheStreamer {
        turn: AtomicUsize,
    }

    impl ModelStreamer for CacheStreamer {
        async fn stream_conversation<'a>(
            &'a self,
            _messages: &'a [ConversationMessage<'a>],
            _tools: &'a [ToolSpec],
            _parallel_tool_calls: bool,
            _session_id: SessionId,
            model: &'a str,
            _on_delta: &'a mut (dyn FnMut(&str) -> Result<()> + Send),
        ) -> Result<ModelResponse> {
            let turn = self.turn.fetch_add(1, Ordering::SeqCst);
            let text = if turn == 0 {
                "first answer"
            } else {
                "second answer"
            };
            Ok(ModelResponse::with_cache_health(
                text,
                [],
                CacheHealth {
                    model: model.to_string(),
                    prompt_cache_key: format!("cache-key-{model}"),
                    stable_prefix_hash: 0x1234,
                    stable_prefix_bytes: 24,
                    message_count: 1,
                    input_bytes: 5,
                    response_id: Some(format!("resp_{turn}")),
                    usage: Some(TokenUsage::new(Some(100), Some(80), Some(10), Some(110))),
                    cache_status: CacheStatus::FirstRequest,
                },
            ))
        }
    }

    struct RetryAfterToolEventFailureStreamer {
        turn: AtomicUsize,
    }

    impl ModelStreamer for RetryAfterToolEventFailureStreamer {
        async fn stream_conversation<'a>(
            &'a self,
            messages: &'a [ConversationMessage<'a>],
            tools: &'a [ToolSpec],
            parallel_tool_calls: bool,
            _session_id: SessionId,
            _model: &'a str,
            _on_delta: &'a mut (dyn FnMut(&str) -> Result<()> + Send),
        ) -> Result<ModelResponse> {
            let turn = self.turn.fetch_add(1, Ordering::SeqCst);
            match turn {
                0 => {
                    assert!(!tools.is_empty());
                    assert!(parallel_tool_calls);
                    assert_eq!(messages, &[ConversationMessage::user("read the manifest")]);
                    Ok(ModelResponse::with_tool_calls(
                        "",
                        [ModelToolCall {
                            item_id: Some("fc_read".to_string()),
                            call_id: "call_read".to_string(),
                            name: "read_file".to_string(),
                            arguments: r#"{"path":"Cargo.toml"}"#.to_string(),
                        }],
                    ))
                }
                1 => {
                    assert_eq!(
                        messages,
                        &[
                            ConversationMessage::user("read the manifest"),
                            ConversationMessage::user("continue"),
                        ]
                    );
                    Ok(ModelResponse::new("ok"))
                }
                _ => unreachable!("unexpected turn"),
            }
        }
    }

    struct ToolLoopStreamer {
        turn: AtomicUsize,
    }

    impl ModelStreamer for ToolLoopStreamer {
        async fn stream_conversation<'a>(
            &'a self,
            messages: &'a [ConversationMessage<'a>],
            tools: &'a [ToolSpec],
            parallel_tool_calls: bool,
            _session_id: SessionId,
            _model: &'a str,
            on_delta: &'a mut (dyn FnMut(&str) -> Result<()> + Send),
        ) -> Result<ModelResponse> {
            let turn = self.turn.fetch_add(1, Ordering::SeqCst);
            match turn {
                0 => {
                    assert!(!tools.is_empty());
                    assert!(parallel_tool_calls);
                    assert_eq!(messages, &[ConversationMessage::user("read the manifest")]);
                    Ok(ModelResponse::with_tool_calls(
                        "",
                        [ModelToolCall {
                            item_id: Some("fc_read".to_string()),
                            call_id: "call_read".to_string(),
                            name: "read_file".to_string(),
                            arguments: r#"{"path":"Cargo.toml"}"#.to_string(),
                        }],
                    ))
                }
                1 => {
                    assert_eq!(messages.len(), 3);
                    assert!(matches!(
                        messages[1],
                        ConversationMessage::FunctionCall { .. }
                    ));
                    match messages[2] {
                        ConversationMessage::FunctionOutput {
                            call_id,
                            output,
                            success,
                        } => {
                            assert_eq!(call_id, "call_read");
                            assert!(output.contains("name = \"rust-agent\""));
                            assert!(success);
                        }
                        _ => panic!("expected function output in prompt"),
                    }
                    on_delta("read ok")?;
                    Ok(ModelResponse::new("read ok"))
                }
                _ => unreachable!("unexpected turn"),
            }
        }
    }

    struct PatchLoopStreamer {
        turn: AtomicUsize,
    }

    impl ModelStreamer for PatchLoopStreamer {
        async fn stream_conversation<'a>(
            &'a self,
            messages: &'a [ConversationMessage<'a>],
            tools: &'a [ToolSpec],
            parallel_tool_calls: bool,
            _session_id: SessionId,
            _model: &'a str,
            on_delta: &'a mut (dyn FnMut(&str) -> Result<()> + Send),
        ) -> Result<ModelResponse> {
            let turn = self.turn.fetch_add(1, Ordering::SeqCst);
            match turn {
                0 => {
                    assert!(tools.iter().any(|tool| tool.name() == "apply_patch"));
                    assert!(parallel_tool_calls);
                    assert_eq!(messages, &[ConversationMessage::user("patch lib")]);
                    let patch = concat!(
                        "*** Begin Patch\n",
                        "*** Update File: lib.rs\n",
                        "@@\n",
                        " pub fn value() -> &'static str {\n",
                        "-    \"old\"\n",
                        "+    \"new\"\n",
                        " }\n",
                        "*** End Patch\n",
                    );
                    Ok(ModelResponse::with_tool_calls(
                        "",
                        [ModelToolCall {
                            item_id: Some("fc_patch".to_string()),
                            call_id: "call_patch".to_string(),
                            name: "apply_patch".to_string(),
                            arguments: json!({ "patch": patch }).to_string(),
                        }],
                    ))
                }
                1 => {
                    assert_eq!(messages.len(), 3);
                    match messages[2] {
                        ConversationMessage::FunctionOutput {
                            call_id,
                            output,
                            success,
                        } => {
                            assert_eq!(call_id, "call_patch");
                            assert!(output.contains("updated lib.rs"));
                            assert!(success);
                        }
                        _ => panic!("expected function output in prompt"),
                    }
                    on_delta("patched")?;
                    Ok(ModelResponse::new("patched"))
                }
                _ => unreachable!("unexpected turn"),
            }
        }
    }
}
