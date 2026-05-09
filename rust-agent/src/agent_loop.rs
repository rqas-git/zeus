//! In-memory agent loop for ordered session turns.

use std::fmt;
use std::future::Future;
use std::ops::Range;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::sync::OnceLock;

use anyhow::Context;
use anyhow::Result;
use futures_util::future::join_all;
use tokio::sync::Notify;

use crate::client::ConversationMessage;
use crate::compaction::is_context_overflow_error;
use crate::compaction::prepare_compaction;
use crate::compaction::summary_prompt;
use crate::compaction::turn_prefix_prompt;
use crate::compaction::with_file_operations;
use crate::compaction::CompactionDetails;
use crate::compaction::CompactionPreparation;
use crate::compaction::CompactionResult;
use crate::config::CompactionConfig;
use crate::config::ContextWindowConfig;
use crate::storage::SessionDatabase;
use crate::storage::StoredSession;
use crate::tools::ToolExecution;
use crate::tools::ToolPolicy;
use crate::tools::ToolRegistry;
use crate::tools::ToolSpec;
use crate::tools::EXEC_COMMAND_TOOL_NAME;

/// Shared cancellation signal for one running turn.
#[derive(Clone, Debug)]
pub(crate) struct TurnCancellation {
    inner: Arc<TurnCancellationState>,
}

#[derive(Debug)]
struct TurnCancellationState {
    cancelled: AtomicBool,
    notify: Notify,
}

impl TurnCancellation {
    /// Creates a cancellation signal for a turn.
    pub(crate) fn new() -> Self {
        Self {
            inner: Arc::new(TurnCancellationState {
                cancelled: AtomicBool::new(false),
                notify: Notify::new(),
            }),
        }
    }

    /// Requests cancellation for the turn.
    pub(crate) fn cancel(&self) {
        self.inner.cancelled.store(true, Ordering::SeqCst);
        self.inner.notify.notify_waiters();
    }

    /// Returns `true` when cancellation has been requested.
    pub(crate) fn is_cancelled(&self) -> bool {
        self.inner.cancelled.load(Ordering::SeqCst)
    }

    /// Waits until cancellation is requested.
    pub(crate) async fn cancelled(&self) {
        loop {
            let notified = self.inner.notify.notified();
            if self.is_cancelled() {
                return;
            }
            notified.await;
        }
    }

    fn ensure_not_cancelled(&self) -> Result<()> {
        if self.is_cancelled() {
            return Err(TurnCancelled.into());
        }
        Ok(())
    }
}

impl Default for TurnCancellation {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug)]
struct TurnCancelled;

impl fmt::Display for TurnCancelled {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("turn cancelled")
    }
}

impl std::error::Error for TurnCancelled {}

/// Returns `true` when an error represents turn cancellation.
pub(crate) fn is_turn_cancelled(error: &anyhow::Error) -> bool {
    error.downcast_ref::<TurnCancelled>().is_some()
}

/// Creates the canonical turn cancellation error.
pub(crate) fn turn_cancelled_error() -> anyhow::Error {
    TurnCancelled.into()
}

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
    /// Creates a message identifier from a stable numeric value.
    pub(crate) const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Returns the numeric message identifier.
    pub(crate) const fn get(self) -> u64 {
        self.0
    }

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

    /// Creates a role from its stable storage label.
    pub(crate) fn from_str(value: &str) -> Option<Self> {
        match value {
            "user" => Some(Self::User),
            "assistant" => Some(Self::Assistant),
            _ => None,
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

    /// Creates a status from its stable storage label.
    pub(crate) fn from_str(value: &str) -> Option<Self> {
        match value {
            "idle" => Some(Self::Idle),
            "running" => Some(Self::Running),
            "failed" => Some(Self::Failed),
            _ => None,
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
pub(crate) enum AgentItem {
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
    Compaction {
        summary: String,
        first_kept_message_id: MessageId,
        tokens_before: u64,
        details: CompactionDetails,
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
    /// Creates a stored agent message from durable parts.
    pub(crate) fn from_parts(id: MessageId, item: AgentItem) -> Self {
        Self { id, item }
    }

    /// Returns the stable message identifier.
    pub(crate) const fn id(&self) -> MessageId {
        self.id
    }

    /// Returns the stored item payload.
    pub(crate) const fn item(&self) -> &AgentItem {
        &self.item
    }

    /// Returns the message role.
    #[cfg(test)]
    pub(crate) fn role(&self) -> MessageRole {
        match &self.item {
            AgentItem::Message { role, .. } => *role,
            AgentItem::FunctionCall { .. } | AgentItem::FunctionOutput { .. } => {
                panic!("tool transcript items do not have user or assistant roles")
            }
            AgentItem::Compaction { .. } => {
                panic!("compaction entries do not have user or assistant roles")
            }
        }
    }

    /// Returns the message text.
    pub(crate) fn text(&self) -> &str {
        match &self.item {
            AgentItem::Message { text, .. } => text,
            AgentItem::FunctionCall { arguments, .. } => arguments,
            AgentItem::FunctionOutput { output, .. } => output,
            AgentItem::Compaction { summary, .. } => summary,
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
            AgentItem::Compaction { summary, .. } => {
                ConversationMessage::owned_user(crate::compaction::compaction_context_text(summary))
            }
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
            AgentItem::Compaction { summary, .. } => {
                crate::compaction::compaction_context_text(summary).len()
            }
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
    TurnTokenUsage {
        session_id: SessionId,
        usage: TokenUsage,
    },
    CompactionStarted {
        session_id: SessionId,
        reason: CompactionReason,
    },
    CompactionCompleted {
        session_id: SessionId,
        reason: CompactionReason,
        result: &'a CompactionResult,
    },
    ToolCallStarted {
        session_id: SessionId,
        tool_call_id: &'a str,
        tool_name: &'a str,
        args: &'a str,
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

/// Reason a compaction run started.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CompactionReason {
    Manual,
    Threshold,
    Overflow,
}

impl CompactionReason {
    /// Returns the stable event label for the reason.
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Manual => "manual",
            Self::Threshold => "threshold",
            Self::Overflow => "overflow",
        }
    }
}

/// Provider token usage reported for one completed model response.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct TokenUsage {
    pub(crate) input_tokens: Option<u64>,
    pub(crate) cached_input_tokens: Option<u64>,
    pub(crate) output_tokens: Option<u64>,
    pub(crate) reasoning_output_tokens: Option<u64>,
    pub(crate) total_tokens: Option<u64>,
}

impl TokenUsage {
    /// Creates token usage from provider-reported counters.
    pub(crate) const fn new(
        input_tokens: Option<u64>,
        cached_input_tokens: Option<u64>,
        output_tokens: Option<u64>,
        reasoning_output_tokens: Option<u64>,
        total_tokens: Option<u64>,
    ) -> Self {
        Self {
            input_tokens,
            cached_input_tokens,
            output_tokens,
            reasoning_output_tokens,
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

    /// Returns whether any provider counter was reported.
    pub(crate) fn is_reported(self) -> bool {
        self.input_tokens.is_some()
            || self.cached_input_tokens.is_some()
            || self.output_tokens.is_some()
            || self.reasoning_output_tokens.is_some()
            || self.total_tokens.is_some()
    }

    /// Adds reported counters from another usage sample.
    pub(crate) fn add_reported(&mut self, other: Self) {
        add_optional_token_count(&mut self.input_tokens, other.input_tokens);
        add_optional_token_count(&mut self.cached_input_tokens, other.cached_input_tokens);
        add_optional_token_count(&mut self.output_tokens, other.output_tokens);
        add_optional_token_count(
            &mut self.reasoning_output_tokens,
            other.reasoning_output_tokens,
        );
        add_optional_token_count(&mut self.total_tokens, other.total_tokens);
    }
}

fn add_optional_token_count(total: &mut Option<u64>, value: Option<u64>) {
    if let Some(value) = value {
        *total = Some(total.unwrap_or(0).saturating_add(value));
    }
}

/// Whether the cacheable request shape matches the previous turn in this session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CacheStatus {
    FirstRequest,
    ReusedPrefix,
    CacheKeyChanged,
    StablePrefixChanged,
    InputPrefixChanged,
}

impl CacheStatus {
    /// Returns a stable telemetry label.
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::FirstRequest => "first_request",
            Self::ReusedPrefix => "reused_prefix",
            Self::CacheKeyChanged => "cache_key_changed",
            Self::StablePrefixChanged => "stable_prefix_changed",
            Self::InputPrefixChanged => "input_prefix_changed",
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
    pub(crate) request_input_hash: u64,
    pub(crate) request_input_prefix_hashes: Vec<u64>,
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

/// Result of a user-initiated terminal command recorded in the session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TerminalCommandResult {
    pub(crate) output: String,
    pub(crate) success: bool,
}

/// Codex routing state scoped to one model-driven turn.
#[derive(Debug, Default)]
pub(crate) struct ModelTurnState {
    codex_turn_state: OnceLock<String>,
}

impl ModelTurnState {
    /// Creates empty turn state for one user turn.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Returns the Codex sticky-routing token captured for this turn.
    pub(crate) fn codex_turn_state(&self) -> Option<&str> {
        self.codex_turn_state.get().map(String::as_str)
    }

    /// Records the first Codex sticky-routing token observed in this turn.
    pub(crate) fn set_codex_turn_state(&self, value: &str) {
        if !value.is_empty() {
            let _ = self.codex_turn_state.set(value.to_string());
        }
    }
}

/// Streams a prompt window into an assistant response.
pub(crate) trait ModelStreamer: Sync {
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

    /// Sends the prompt window with an optional reasoning effort override.
    fn stream_conversation_with_reasoning<'a>(
        &'a self,
        messages: &'a [ConversationMessage<'a>],
        tools: &'a [ToolSpec],
        parallel_tool_calls: bool,
        session_id: SessionId,
        model: &'a str,
        reasoning_effort: Option<&'a str>,
        on_delta: &'a mut (dyn FnMut(&str) -> Result<()> + Send),
    ) -> impl Future<Output = Result<ModelResponse>> + Send + 'a {
        let _ = reasoning_effort;
        async move {
            self.stream_conversation(
                messages,
                tools,
                parallel_tool_calls,
                session_id,
                model,
                on_delta,
            )
            .await
        }
    }

    /// Sends the prompt window with routing state scoped to the current turn.
    fn stream_conversation_with_turn_state<'a>(
        &'a self,
        messages: &'a [ConversationMessage<'a>],
        tools: &'a [ToolSpec],
        parallel_tool_calls: bool,
        session_id: SessionId,
        model: &'a str,
        reasoning_effort: Option<&'a str>,
        turn_state: &'a ModelTurnState,
        on_delta: &'a mut (dyn FnMut(&str) -> Result<()> + Send),
    ) -> impl Future<Output = Result<ModelResponse>> + Send + 'a {
        let _ = turn_state;
        async move {
            self.stream_conversation_with_reasoning(
                messages,
                tools,
                parallel_tool_calls,
                session_id,
                model,
                reasoning_effort,
                on_delta,
            )
            .await
        }
    }

    /// Generates a semantic compaction summary without user-visible deltas.
    fn compact_conversation<'a>(
        &'a self,
        prompt: &'a str,
        session_id: SessionId,
        model: &'a str,
        reasoning_effort: Option<&'a str>,
    ) -> impl Future<Output = Result<String>> + Send + 'a {
        async move {
            let messages = [ConversationMessage::user(prompt)];
            let tools: [ToolSpec; 0] = [];
            let mut ignore_delta = |_delta: &str| Ok(());
            let response = self
                .stream_conversation_with_reasoning(
                    &messages,
                    &tools,
                    false,
                    session_id,
                    model,
                    reasoning_effort,
                    &mut ignore_delta,
                )
                .await?;
            Ok(response.text)
        }
    }
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
            next_message_id: MessageId::new(1),
            messages: Vec::new(),
            last_cache_observation: None,
        }
    }

    fn from_stored(session_id: SessionId, mut stored: StoredSession) -> Self {
        if stored.status == SessionStatus::Running {
            stored.status = SessionStatus::Idle;
        }
        let next_message_id = stored
            .messages
            .iter()
            .map(AgentMessage::id)
            .max_by_key(|id| id.get())
            .map_or(MessageId::new(1), MessageId::next);
        Self {
            session_id,
            status: stored.status,
            config: stored.config,
            next_message_id,
            messages: stored.messages,
            last_cache_observation: stored.last_cache_observation,
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

    fn append_compaction(&mut self, result: CompactionResult) -> MessageId {
        self.append_item(AgentItem::Compaction {
            summary: result.summary,
            first_kept_message_id: result.first_kept_message_id,
            tokens_before: result.tokens_before,
            details: result.details,
        })
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

    #[cfg(test)]
    fn conversation_window(&self, config: ContextWindowConfig) -> Vec<ConversationMessage<'_>> {
        self.conversation_window_with_compaction(config, CompactionConfig::disabled())
    }

    fn conversation_window_with_compaction(
        &self,
        config: ContextWindowConfig,
        compaction: CompactionConfig,
    ) -> Vec<ConversationMessage<'_>> {
        if compaction.enabled() || self.latest_compaction_index().is_some() {
            return self.compacted_context();
        }

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

    fn compacted_context(&self) -> Vec<ConversationMessage<'_>> {
        let Some(compaction_index) = self.latest_compaction_index() else {
            return self
                .messages
                .iter()
                .filter(|message| !matches!(message.item(), AgentItem::Compaction { .. }))
                .map(AgentMessage::conversation_message)
                .collect();
        };
        let AgentItem::Compaction {
            summary,
            first_kept_message_id,
            ..
        } = self.messages[compaction_index].item()
        else {
            unreachable!("latest compaction index must point at a compaction")
        };
        let first_kept = self
            .messages
            .iter()
            .take(compaction_index)
            .position(|message| message.id() == *first_kept_message_id)
            .unwrap_or(compaction_index);
        let mut context = Vec::new();
        context.push(ConversationMessage::owned_user(
            crate::compaction::compaction_context_text(summary),
        ));
        context.extend(
            self.messages[first_kept..compaction_index]
                .iter()
                .filter(|message| !matches!(message.item(), AgentItem::Compaction { .. }))
                .map(AgentMessage::conversation_message),
        );
        context.extend(
            self.messages[compaction_index + 1..]
                .iter()
                .filter(|message| !matches!(message.item(), AgentItem::Compaction { .. }))
                .map(AgentMessage::conversation_message),
        );
        context
    }

    #[cfg(test)]
    fn prune_history(&mut self, config: ContextWindowConfig) {
        self.prune_history_with_compaction(config, CompactionConfig::disabled());
    }

    fn prune_history_with_compaction(
        &mut self,
        config: ContextWindowConfig,
        compaction: CompactionConfig,
    ) {
        if compaction.enabled() || self.latest_compaction_index().is_some() {
            self.prune_before_latest_compaction_boundary();
            return;
        }

        let ranges =
            self.retained_ranges(config.history_max_messages(), config.history_max_bytes());
        let first_retained = ranges
            .first()
            .map_or(self.messages.len(), |range| range.start);

        if first_retained > 0 {
            self.messages.drain(0..first_retained);
        }
    }

    fn prune_before_latest_compaction_boundary(&mut self) {
        let Some(compaction_index) = self.latest_compaction_index() else {
            return;
        };
        let AgentItem::Compaction {
            first_kept_message_id,
            ..
        } = self.messages[compaction_index].item()
        else {
            unreachable!("latest compaction index must point at a compaction")
        };
        let first_retained = self
            .messages
            .iter()
            .take(compaction_index)
            .position(|message| message.id() == *first_kept_message_id)
            .unwrap_or(compaction_index);
        if first_retained > 0 {
            self.messages.drain(0..first_retained);
        }
    }

    fn latest_compaction_index(&self) -> Option<usize> {
        self.messages
            .iter()
            .rposition(|message| matches!(message.item(), AgentItem::Compaction { .. }))
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
        if let (Some(previous_hash), Some(previous_count)) = (
            previous.request_input_hash,
            previous.request_input_message_count,
        ) {
            match cache_health.request_input_prefix_hashes.get(previous_count) {
                Some(current_prefix_hash) if *current_prefix_hash == previous_hash => {}
                _ => return CacheStatus::InputPrefixChanged,
            }
        }
        CacheStatus::ReusedPrefix
    }

    fn record_cache_observation(&mut self, cache_health: &CacheHealth) {
        self.last_cache_observation = Some(CacheObservation {
            prompt_cache_key: cache_health.prompt_cache_key.clone(),
            stable_prefix_hash: cache_health.stable_prefix_hash,
            request_input_hash: Some(cache_health.request_input_hash),
            request_input_message_count: Some(cache_health.message_count),
        });
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CacheObservation {
    pub(crate) prompt_cache_key: String,
    pub(crate) stable_prefix_hash: u64,
    pub(crate) request_input_hash: Option<u64>,
    pub(crate) request_input_message_count: Option<usize>,
}

/// Runs ordered turns for a single in-memory session.
#[derive(Debug)]
pub(crate) struct AgentLoop {
    store: InMemorySessionStore,
    context_window: ContextWindowConfig,
    compaction: CompactionConfig,
    tools: ToolRegistry,
    database: Option<SessionDatabase>,
}

/// Chainable construction for an agent loop session.
#[derive(Debug)]
pub(crate) struct AgentLoopBuilder {
    session_id: SessionId,
    config: SessionConfig,
    context_window: ContextWindowConfig,
    compaction: CompactionConfig,
    tools: ToolRegistry,
    database: Option<SessionDatabase>,
}

impl AgentLoopBuilder {
    /// Sets context-window bounds.
    pub(crate) const fn context_window(mut self, context_window: ContextWindowConfig) -> Self {
        self.context_window = context_window;
        self
    }

    /// Sets semantic compaction behavior.
    pub(crate) const fn compaction(mut self, compaction: CompactionConfig) -> Self {
        self.compaction = compaction;
        self
    }

    /// Sets the tool registry used by model turns.
    pub(crate) fn tools(mut self, tools: ToolRegistry) -> Self {
        self.tools = tools;
        self
    }

    /// Enables durable SQLite-backed storage.
    pub(crate) fn database(mut self, database: SessionDatabase) -> Self {
        self.database = Some(database);
        self
    }

    /// Builds the configured agent loop.
    ///
    /// # Errors
    /// Returns an error when durable storage cannot load or initialize the session.
    pub(crate) fn build(self) -> Result<AgentLoop> {
        let mut builder = self;
        match builder.database.take() {
            Some(database) => AgentLoop::from_database(builder, database),
            None => Ok(AgentLoop {
                store: InMemorySessionStore::new(builder.session_id, builder.config),
                context_window: builder.context_window,
                compaction: builder.compaction,
                tools: builder.tools,
                database: None,
            }),
        }
    }
}

impl AgentLoop {
    /// Starts an agent-loop builder for one session.
    pub(crate) fn builder(session_id: SessionId, config: SessionConfig) -> AgentLoopBuilder {
        AgentLoopBuilder {
            session_id,
            config,
            context_window: ContextWindowConfig::default(),
            compaction: CompactionConfig::disabled(),
            tools: ToolRegistry::default(),
            database: None,
        }
    }

    /// Creates an agent loop for one session.
    #[cfg(test)]
    pub(crate) fn new(session_id: SessionId) -> Self {
        Self::builder(session_id, SessionConfig::new("test-model"))
            .build()
            .expect("in-memory agent loop construction should not fail")
    }

    /// Creates an agent loop with explicit context-window bounds.
    #[cfg(test)]
    pub(crate) fn with_context_window(
        session_id: SessionId,
        context_window: ContextWindowConfig,
        config: SessionConfig,
    ) -> Self {
        Self::builder(session_id, config)
            .context_window(context_window)
            .build()
            .expect("in-memory agent loop construction should not fail")
    }

    /// Creates an agent loop with explicit context and compaction bounds.
    #[cfg(test)]
    pub(crate) fn with_context_window_and_compaction(
        session_id: SessionId,
        context_window: ContextWindowConfig,
        compaction: CompactionConfig,
        config: SessionConfig,
    ) -> Self {
        Self::builder(session_id, config)
            .context_window(context_window)
            .compaction(compaction)
            .build()
            .expect("in-memory agent loop construction should not fail")
    }

    /// Creates an agent loop with explicit context bounds and tool registry.
    #[cfg(test)]
    pub(crate) fn with_context_window_and_tools(
        session_id: SessionId,
        context_window: ContextWindowConfig,
        config: SessionConfig,
        tools: ToolRegistry,
    ) -> Self {
        Self::builder(session_id, config)
            .context_window(context_window)
            .tools(tools)
            .build()
            .expect("in-memory agent loop construction should not fail")
    }

    /// Creates an agent loop backed by durable SQLite session storage.
    ///
    /// # Errors
    /// Returns an error when the session cannot be loaded or initialized.
    #[cfg(test)]
    pub(crate) fn with_context_window_tools_and_database(
        session_id: SessionId,
        context_window: ContextWindowConfig,
        config: SessionConfig,
        tools: ToolRegistry,
        database: SessionDatabase,
    ) -> Result<Self> {
        Self::builder(session_id, config)
            .context_window(context_window)
            .tools(tools)
            .database(database)
            .build()
    }

    fn from_database(builder: AgentLoopBuilder, database: SessionDatabase) -> Result<Self> {
        let AgentLoopBuilder {
            session_id,
            config,
            context_window,
            compaction,
            tools,
            ..
        } = builder;
        let mut store = match database.load_session(session_id)? {
            Some(stored) => InMemorySessionStore::from_stored(session_id, stored),
            None => {
                database.ensure_session(session_id, config.model())?;
                InMemorySessionStore::new(session_id, config)
            }
        };
        store.prune_history_with_compaction(context_window, compaction);
        if store.status() == SessionStatus::Idle {
            database.set_session_status(session_id, SessionStatus::Idle)?;
        }
        Ok(Self {
            store,
            context_window,
            compaction,
            tools,
            database: Some(database),
        })
    }

    /// Returns the session identifier.
    pub(crate) const fn session_id(&self) -> SessionId {
        self.store.session_id()
    }

    /// Returns the selected model for future turns.
    pub(crate) fn model(&self) -> &str {
        self.store.model()
    }

    /// Returns the selected tool permission policy for future turns.
    pub(crate) fn tool_policy(&self) -> ToolPolicy {
        self.tools.policy()
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
        let model = model.into();
        if let Some(database) = &self.database {
            database.set_session_model(self.session_id(), &model)?;
        }
        self.store.set_model(model);
        Ok(())
    }

    /// Changes the selected tool permission policy for future turns.
    ///
    /// # Errors
    /// Returns an error if a turn is currently running.
    pub(crate) fn set_tool_policy(&mut self, policy: ToolPolicy) -> Result<()> {
        anyhow::ensure!(
            self.store.status() != SessionStatus::Running,
            "cannot change tool policy while session is running"
        );
        self.tools = self.tools.with_policy(policy);
        Ok(())
    }

    /// Appends a user message, streams the model response, and stores the assistant message.
    ///
    /// # Errors
    /// Returns an error if the session is already running, model streaming fails, or event publishing fails.
    #[cfg(test)]
    pub(crate) async fn submit_user_message(
        &mut self,
        text: impl Into<String>,
        model: &impl ModelStreamer,
        mut emit: impl FnMut(AgentEvent<'_>) -> Result<()> + Send,
    ) -> Result<()> {
        self.submit_user_message_with_cancellation(
            text,
            model,
            TurnCancellation::new(),
            None,
            &mut emit,
        )
        .await
    }

    /// Appends a user message and runs the turn with a cancellation signal.
    ///
    /// # Errors
    /// Returns an error if the session is already running, cancellation is requested, model streaming fails, or event publishing fails.
    pub(crate) async fn submit_user_message_with_cancellation(
        &mut self,
        text: impl Into<String>,
        model: &impl ModelStreamer,
        cancellation: TurnCancellation,
        reasoning_effort: Option<&str>,
        mut emit: impl FnMut(AgentEvent<'_>) -> Result<()> + Send,
    ) -> Result<()> {
        anyhow::ensure!(
            self.store.status() != SessionStatus::Running,
            "session is already running"
        );
        cancellation.ensure_not_cancelled()?;

        let user_text = text.into();
        let user_id = self.store.append_message(MessageRole::User, user_text);
        self.persist_message(user_id)?;
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
            return rollback_persisted_message(
                self.database.as_ref(),
                self.session_id(),
                user_id,
                error,
            );
        }

        if let Err(error) = self.begin_running(&mut emit) {
            self.store.remove_last_message(user_id);
            return rollback_persisted_message(
                self.database.as_ref(),
                self.session_id(),
                user_id,
                error,
            );
        }
        self.store
            .prune_history_with_compaction(self.context_window, self.compaction);
        let result = async {
            self.maybe_auto_compact(model, &cancellation, reasoning_effort, &mut emit)
                .await?;
            self.run_until_done(model, &cancellation, reasoning_effort, &mut emit)
                .await
        }
        .await;
        match result {
            Ok(()) => {
                self.set_status(SessionStatus::Idle, &mut emit)?;
                Ok(())
            }
            Err(error) if is_turn_cancelled(&error) => {
                self.store
                    .prune_history_with_compaction(self.context_window, self.compaction);
                self.set_status(SessionStatus::Idle, &mut emit)?;
                Err(error)
            }
            Err(error) => {
                self.store
                    .prune_history_with_compaction(self.context_window, self.compaction);
                self.set_status(SessionStatus::Failed, &mut emit)?;
                let message = error.to_string();
                emit(AgentEvent::Error {
                    session_id: self.session_id(),
                    message: &message,
                })?;
                Err(error)
            }
        }
    }

    /// Runs a user-initiated terminal command through the backend tool layer and stores it.
    ///
    /// # Errors
    /// Returns an error if the session is already running, cancellation is requested before the
    /// command starts, or event/storage updates fail.
    pub(crate) async fn run_terminal_command_with_cancellation(
        &mut self,
        command: impl Into<String>,
        cancellation: TurnCancellation,
        mut emit: impl FnMut(AgentEvent<'_>) -> Result<()> + Send,
    ) -> Result<TerminalCommandResult> {
        anyhow::ensure!(
            self.store.status() != SessionStatus::Running,
            "session is already running"
        );
        cancellation.ensure_not_cancelled()?;

        let command = command.into();
        let command = command.trim();
        anyhow::ensure!(!command.is_empty(), "command cannot be empty");
        anyhow::ensure!(
            self.tools.policy() == ToolPolicy::WorkspaceExec,
            "terminal commands require workspace-exec tool policy"
        );
        let user_id = self
            .store
            .append_message(MessageRole::User, format!("$ {command}"));
        self.persist_message(user_id)?;
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
            return rollback_persisted_message(
                self.database.as_ref(),
                self.session_id(),
                user_id,
                error,
            );
        }

        if let Err(error) = self.begin_running(&mut emit) {
            self.store.remove_last_message(user_id);
            return rollback_persisted_message(
                self.database.as_ref(),
                self.session_id(),
                user_id,
                error,
            );
        }

        let result = self
            .execute_terminal_command(command, user_id, &cancellation, &mut emit)
            .await;
        self.store
            .prune_history_with_compaction(self.context_window, self.compaction);
        match result {
            Ok(result) => {
                self.set_status(SessionStatus::Idle, &mut emit)?;
                Ok(result)
            }
            Err(error) if is_turn_cancelled(&error) => {
                self.set_status(SessionStatus::Idle, &mut emit)?;
                Err(error)
            }
            Err(error) => {
                self.set_status(SessionStatus::Failed, &mut emit)?;
                let message = error.to_string();
                emit(AgentEvent::Error {
                    session_id: self.session_id(),
                    message: &message,
                })?;
                Err(error)
            }
        }
    }

    fn persist_message(&self, message_id: MessageId) -> Result<()> {
        let Some(database) = &self.database else {
            return Ok(());
        };
        let message = self
            .store
            .messages()
            .iter()
            .find(|message| message.id() == message_id)
            .context("message missing from session store")?;
        database.insert_message(self.session_id(), message)
    }

    fn persist_messages_from(&self, start: usize) -> Result<()> {
        let Some(database) = &self.database else {
            return Ok(());
        };
        database.insert_messages(self.session_id(), &self.store.messages()[start..])
    }

    fn persist_status(&self, status: SessionStatus) -> Result<()> {
        if let Some(database) = &self.database {
            database.set_session_status(self.session_id(), status)?;
        }
        Ok(())
    }

    fn record_cache_observation(&mut self, cache_health: &CacheHealth) -> Result<()> {
        self.store.record_cache_observation(cache_health);
        if let (Some(database), Some(observation)) =
            (&self.database, &self.store.last_cache_observation)
        {
            database.record_cache_observation(self.session_id(), observation)?;
        }
        Ok(())
    }

    fn begin_running(
        &mut self,
        emit: &mut (impl FnMut(AgentEvent<'_>) -> Result<()> + Send),
    ) -> Result<()> {
        self.store.set_status(SessionStatus::Running);
        self.persist_status(SessionStatus::Running)?;
        if let Err(error) = emit(AgentEvent::StatusChanged {
            session_id: self.session_id(),
            status: SessionStatus::Running,
        }) {
            self.store.set_status(SessionStatus::Idle);
            self.persist_status(SessionStatus::Idle)?;
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
        self.persist_status(status)?;
        emit(AgentEvent::StatusChanged {
            session_id: self.session_id(),
            status,
        })
    }

    async fn run_until_done(
        &mut self,
        model: &impl ModelStreamer,
        cancellation: &TurnCancellation,
        reasoning_effort: Option<&str>,
        emit: &mut (impl FnMut(AgentEvent<'_>) -> Result<()> + Send),
    ) -> Result<()> {
        let mut recovered_overflow = false;
        let turn_state = ModelTurnState::new();
        let mut turn_usage = TokenUsage::default();
        loop {
            cancellation.ensure_not_cancelled()?;
            let tool_calls = match self
                .run_once(
                    model,
                    cancellation,
                    reasoning_effort,
                    &turn_state,
                    &mut turn_usage,
                    emit,
                )
                .await
            {
                Ok(tool_calls) => tool_calls,
                Err(error)
                    if self.compaction.enabled()
                        && !recovered_overflow
                        && is_context_overflow_error(&error.to_string()) =>
                {
                    recovered_overflow = true;
                    self.run_compaction(
                        model,
                        cancellation,
                        reasoning_effort,
                        None,
                        CompactionReason::Overflow,
                        emit,
                    )
                    .await
                    .with_context(|| format!("context overflow recovery failed: {error}"))?;
                    continue;
                }
                Err(error) => return Err(error),
            };
            if tool_calls.is_empty() {
                self.maybe_auto_compact(model, cancellation, reasoning_effort, emit)
                    .await?;
                self.store
                    .prune_history_with_compaction(self.context_window, self.compaction);
                if turn_usage.is_reported() {
                    emit(AgentEvent::TurnTokenUsage {
                        session_id: self.session_id(),
                        usage: turn_usage,
                    })?;
                }
                return Ok(());
            }

            self.execute_tool_calls(tool_calls, cancellation, emit)
                .await?;
            self.store
                .prune_history_with_compaction(self.context_window, self.compaction);
        }
    }

    async fn run_once(
        &mut self,
        model: &impl ModelStreamer,
        cancellation: &TurnCancellation,
        reasoning_effort: Option<&str>,
        turn_state: &ModelTurnState,
        turn_usage: &mut TokenUsage,
        emit: &mut (impl FnMut(AgentEvent<'_>) -> Result<()> + Send),
    ) -> Result<Vec<ModelToolCall>> {
        let history = self
            .store
            .conversation_window_with_compaction(self.context_window, self.compaction);
        let session_id = self.session_id();
        let selected_model = self.store.model();
        let mut on_delta = |delta: &str| emit(AgentEvent::TextDelta { session_id, delta });
        let mut model_response = tokio::select! {
            biased;
            _ = cancellation.cancelled() => return Err(TurnCancelled.into()),
            response = model.stream_conversation_with_turn_state(
                &history,
                self.tools.specs(),
                true,
                session_id,
                selected_model,
                reasoning_effort,
                turn_state,
                &mut on_delta,
            ) => response?,
        };
        if let Some(cache_health) = model_response.cache_health.as_mut() {
            if let Some(usage) = cache_health.usage {
                turn_usage.add_reported(usage);
            }
            cache_health.cache_status = self.store.cache_status(cache_health);
            emit(AgentEvent::CacheHealth {
                session_id,
                cache_health,
            })?;
            self.record_cache_observation(cache_health)?;
        }
        let assistant_text = model_response.text;
        if !assistant_text.is_empty() {
            let assistant_id = self
                .store
                .append_message(MessageRole::Assistant, assistant_text);
            self.persist_message(assistant_id)?;
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

    /// Manually compacts the session context.
    ///
    /// # Errors
    /// Returns an error if the session is running, cancellation is requested, or summarization fails.
    pub(crate) async fn compact_with_cancellation(
        &mut self,
        model: &impl ModelStreamer,
        cancellation: TurnCancellation,
        reasoning_effort: Option<&str>,
        custom_instructions: Option<&str>,
        mut emit: impl FnMut(AgentEvent<'_>) -> Result<()> + Send,
    ) -> Result<CompactionResult> {
        anyhow::ensure!(
            self.store.status() != SessionStatus::Running,
            "session is already running"
        );
        cancellation.ensure_not_cancelled()?;
        self.begin_running(&mut emit)?;
        let result = self
            .run_compaction(
                model,
                &cancellation,
                reasoning_effort,
                custom_instructions,
                CompactionReason::Manual,
                &mut emit,
            )
            .await;
        match result {
            Ok(result) => {
                self.set_status(SessionStatus::Idle, &mut emit)?;
                Ok(result)
            }
            Err(error) if is_turn_cancelled(&error) => {
                self.set_status(SessionStatus::Idle, &mut emit)?;
                Err(error)
            }
            Err(error) => {
                self.set_status(SessionStatus::Failed, &mut emit)?;
                let message = error.to_string();
                emit(AgentEvent::Error {
                    session_id: self.session_id(),
                    message: &message,
                })?;
                Err(error)
            }
        }
    }

    async fn maybe_auto_compact(
        &mut self,
        model: &impl ModelStreamer,
        cancellation: &TurnCancellation,
        reasoning_effort: Option<&str>,
        emit: &mut (impl FnMut(AgentEvent<'_>) -> Result<()> + Send),
    ) -> Result<Option<CompactionResult>> {
        let context_tokens = crate::compaction::estimate_session_tokens(self.store.messages());
        if !self.compaction.should_compact(context_tokens) {
            return Ok(None);
        }
        self.run_compaction(
            model,
            cancellation,
            reasoning_effort,
            None,
            CompactionReason::Threshold,
            emit,
        )
        .await
        .map(Some)
    }

    async fn run_compaction(
        &mut self,
        model: &impl ModelStreamer,
        cancellation: &TurnCancellation,
        reasoning_effort: Option<&str>,
        custom_instructions: Option<&str>,
        reason: CompactionReason,
        emit: &mut (impl FnMut(AgentEvent<'_>) -> Result<()> + Send),
    ) -> Result<CompactionResult> {
        cancellation.ensure_not_cancelled()?;
        let preparation = prepare_compaction(self.store.messages(), self.compaction)
            .context("nothing to compact")?;
        emit(AgentEvent::CompactionStarted {
            session_id: self.session_id(),
            reason,
        })?;

        let mut summary = self
            .generate_compaction_summary(
                model,
                &preparation,
                cancellation,
                reasoning_effort,
                custom_instructions,
            )
            .await?;
        summary = with_file_operations(summary, &preparation.details);
        let result = CompactionResult {
            summary,
            first_kept_message_id: preparation.first_kept_message_id,
            tokens_before: preparation.tokens_before,
            details: preparation.details,
        };
        let stored_result = result.clone();
        let compaction_id = self.store.append_compaction(stored_result);
        self.persist_message(compaction_id)?;
        self.store
            .prune_history_with_compaction(self.context_window, self.compaction);
        emit(AgentEvent::CompactionCompleted {
            session_id: self.session_id(),
            reason,
            result: &result,
        })?;
        Ok(result)
    }

    async fn generate_compaction_summary(
        &self,
        model: &impl ModelStreamer,
        preparation: &CompactionPreparation,
        cancellation: &TurnCancellation,
        reasoning_effort: Option<&str>,
        custom_instructions: Option<&str>,
    ) -> Result<String> {
        let session_id = self.session_id();
        let selected_model = self.store.model();
        if preparation.is_split_turn && !preparation.turn_prefix_messages.is_empty() {
            let history_prompt = summary_prompt(preparation, custom_instructions);
            let prefix_prompt = turn_prefix_prompt(preparation);
            let (history_summary, prefix_summary) = tokio::try_join!(
                async {
                    if preparation.messages_to_summarize.is_empty() {
                        Ok("No prior history.".to_string())
                    } else {
                        cancellation.ensure_not_cancelled()?;
                        model
                            .compact_conversation(
                                &history_prompt,
                                session_id,
                                selected_model,
                                reasoning_effort,
                            )
                            .await
                    }
                },
                async {
                    cancellation.ensure_not_cancelled()?;
                    model
                        .compact_conversation(
                            &prefix_prompt,
                            session_id,
                            selected_model,
                            reasoning_effort,
                        )
                        .await
                }
            )?;
            return Ok(format!(
                "{history_summary}\n\n---\n\n**Turn Context (split turn):**\n\n{prefix_summary}"
            ));
        }

        let prompt = summary_prompt(preparation, custom_instructions);
        model
            .compact_conversation(&prompt, session_id, selected_model, reasoning_effort)
            .await
    }

    async fn execute_tool_calls(
        &mut self,
        tool_calls: Vec<ModelToolCall>,
        cancellation: &TurnCancellation,
        emit: &mut (impl FnMut(AgentEvent<'_>) -> Result<()> + Send),
    ) -> Result<()> {
        let session_id = self.session_id();
        cancellation.ensure_not_cancelled()?;
        for tool_call in &tool_calls {
            emit(AgentEvent::ToolCallStarted {
                session_id,
                tool_call_id: &tool_call.call_id,
                tool_name: &tool_call.name,
                args: &tool_call.arguments,
            })?;
        }

        let executions = if tool_calls
            .iter()
            .all(|call| self.tools.supports_parallel(&call.name))
        {
            join_all(tool_calls.iter().map(|tool_call| {
                self.tools
                    .execute_ref_with_cancellation(tool_call, cancellation)
            }))
            .await
        } else {
            let mut executions = Vec::new();
            for tool_call in &tool_calls {
                cancellation.ensure_not_cancelled()?;
                executions.push(
                    self.tools
                        .execute_ref_with_cancellation(tool_call, cancellation)
                        .await,
                );
            }
            executions
        };
        cancellation.ensure_not_cancelled()?;

        let completions = executions
            .iter()
            .map(|execution| {
                (
                    execution.call_id.clone(),
                    execution.tool_name.clone(),
                    execution.success,
                )
            })
            .collect::<Vec<_>>();
        let first_new_message = self.store.messages().len();
        self.store.append_tool_transcript(tool_calls, executions);
        self.persist_messages_from(first_new_message)?;
        for (tool_call_id, tool_name, success) in completions {
            emit(AgentEvent::ToolCallCompleted {
                session_id,
                tool_call_id: &tool_call_id,
                tool_name: &tool_name,
                success,
            })?;
        }
        Ok(())
    }

    async fn execute_terminal_command(
        &mut self,
        command: &str,
        user_id: MessageId,
        cancellation: &TurnCancellation,
        emit: &mut (impl FnMut(AgentEvent<'_>) -> Result<()> + Send),
    ) -> Result<TerminalCommandResult> {
        let session_id = self.session_id();
        let arguments = serde_json::json!({ "command": command }).to_string();
        let tool_call = ModelToolCall {
            item_id: None,
            call_id: format!("terminal_{}", user_id.get()),
            name: EXEC_COMMAND_TOOL_NAME.to_string(),
            arguments,
        };
        emit(AgentEvent::ToolCallStarted {
            session_id,
            tool_call_id: &tool_call.call_id,
            tool_name: &tool_call.name,
            args: &tool_call.arguments,
        })?;

        let execution = self
            .tools
            .execute_ref_with_cancellation(&tool_call, cancellation)
            .await;
        let result = TerminalCommandResult {
            output: execution.output.clone(),
            success: execution.success,
        };
        let tool_call_id = execution.call_id.clone();
        let tool_name = execution.tool_name.clone();
        let success = execution.success;
        let first_new_message = self.store.messages().len();
        self.store.append_tool_transcript([tool_call], [execution]);
        self.persist_messages_from(first_new_message)?;
        emit(AgentEvent::ToolCallCompleted {
            session_id,
            tool_call_id: &tool_call_id,
            tool_name: &tool_name,
            success,
        })?;
        Ok(result)
    }
}

fn rollback_persisted_message<T>(
    database: Option<&SessionDatabase>,
    session_id: SessionId,
    message_id: MessageId,
    error: anyhow::Error,
) -> Result<T> {
    if let Some(database) = database {
        database
            .delete_message(session_id, message_id)
            .context("failed to roll back persisted message")?;
    }
    Err(error)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::atomic::AtomicUsize;
    use std::sync::Mutex as StdMutex;
    use std::time::Instant;

    use serde_json::json;

    use super::*;
    use crate::bench_support::DurationSummary;

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
    async fn sqlite_database_restores_session_history() {
        let database = SessionDatabase::in_memory().unwrap();
        let session_id = SessionId::new(7);
        let mut agent = AgentLoop::with_context_window_tools_and_database(
            session_id,
            ContextWindowConfig::default(),
            SessionConfig::new("test-model"),
            ToolRegistry::default(),
            database.clone(),
        )
        .unwrap();
        let first_streamer = FnStreamer::new(
            |history: &[ConversationMessage<'_>],
             _tools: &[ToolSpec],
             _parallel_tool_calls: bool,
             _model: &str,
             _on_delta: &mut (dyn FnMut(&str) -> Result<()> + Send)| {
                assert_eq!(history, &[ConversationMessage::user("hello")]);
                Ok("one".to_string())
            },
        );
        agent
            .submit_user_message("hello", &first_streamer, |_| Ok(()))
            .await
            .unwrap();

        let mut restored = AgentLoop::with_context_window_tools_and_database(
            session_id,
            ContextWindowConfig::default(),
            SessionConfig::new("test-model"),
            ToolRegistry::default(),
            database.clone(),
        )
        .unwrap();
        let second_streamer = FnStreamer::new(
            |history: &[ConversationMessage<'_>],
             _tools: &[ToolSpec],
             _parallel_tool_calls: bool,
             _model: &str,
             _on_delta: &mut (dyn FnMut(&str) -> Result<()> + Send)| {
                assert_eq!(
                    history,
                    &[
                        ConversationMessage::user("hello"),
                        ConversationMessage::assistant("one"),
                        ConversationMessage::user("again"),
                    ]
                );
                Ok("two".to_string())
            },
        );

        restored
            .submit_user_message("again", &second_streamer, |_| Ok(()))
            .await
            .unwrap();

        let stored = database.load_session(session_id).unwrap().unwrap();
        assert_eq!(stored.messages.len(), 4);
        assert_eq!(stored.messages[3].text(), "two");
    }

    #[tokio::test]
    async fn sqlite_database_rolls_back_user_message_when_publish_fails() {
        let database = SessionDatabase::in_memory().unwrap();
        let session_id = SessionId::new(7);
        let mut agent = AgentLoop::with_context_window_tools_and_database(
            session_id,
            ContextWindowConfig::default(),
            SessionConfig::new("test-model"),
            ToolRegistry::default(),
            database.clone(),
        )
        .unwrap();
        let streamer = FnStreamer::new(
            |_history: &[ConversationMessage<'_>],
             _tools: &[ToolSpec],
             _parallel_tool_calls: bool,
             _model: &str,
             _on_delta: &mut (dyn FnMut(&str) -> Result<()> + Send)| {
                Ok("ok".to_string())
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
                    anyhow::bail!("publish failed");
                }
                Ok(())
            })
            .await
            .unwrap_err();

        assert!(error.to_string().contains("publish failed"));
        let stored = database.load_session(session_id).unwrap().unwrap();
        assert!(stored.messages.is_empty());
        assert_eq!(stored.status, SessionStatus::Idle);
    }

    #[tokio::test]
    async fn sqlite_database_restores_cache_observation() {
        let database = SessionDatabase::in_memory().unwrap();
        let session_id = SessionId::new(7);
        let streamer = CacheStreamer {
            turn: AtomicUsize::new(0),
        };
        let mut first = AgentLoop::with_context_window_tools_and_database(
            session_id,
            ContextWindowConfig::default(),
            SessionConfig::new("test-model"),
            ToolRegistry::default(),
            database.clone(),
        )
        .unwrap();
        let mut events = Vec::new();

        first
            .submit_user_message("first", &streamer, |event| {
                events.push(format_event(event));
                Ok(())
            })
            .await
            .unwrap();

        let mut restored = AgentLoop::with_context_window_tools_and_database(
            session_id,
            ContextWindowConfig::default(),
            SessionConfig::new("test-model"),
            ToolRegistry::default(),
            database,
        )
        .unwrap();
        restored
            .submit_user_message("second", &streamer, |event| {
                events.push(format_event(event));
                Ok(())
            })
            .await
            .unwrap();

        assert!(events.contains(&"cache:first_request".to_string()));
        assert!(events.contains(&"cache:reused_prefix".to_string()));
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
    async fn manual_compaction_stores_summary_and_keeps_recent_tail() {
        let mut agent = AgentLoop::with_context_window_and_compaction(
            SessionId::new(7),
            ContextWindowConfig::default(),
            CompactionConfig::for_test(100, 10, 8),
            SessionConfig::new("test-model"),
        );
        agent
            .store
            .append_message(MessageRole::User, "old user text");
        agent
            .store
            .append_message(MessageRole::Assistant, "old answer text");
        agent
            .store
            .append_message(MessageRole::User, "recent request");
        agent
            .store
            .append_message(MessageRole::Assistant, "recent answer");
        let calls = AtomicUsize::new(0);
        let streamer = FnStreamer::new(
            |history: &[ConversationMessage<'_>],
             tools: &[ToolSpec],
             parallel_tool_calls: bool,
             _model: &str,
             _on_delta: &mut (dyn FnMut(&str) -> Result<()> + Send)| {
                calls.fetch_add(1, Ordering::SeqCst);
                assert!(tools.is_empty());
                assert!(!parallel_tool_calls);
                assert_eq!(history.len(), 1);
                let ConversationMessage::Message { text, .. } = &history[0] else {
                    panic!("compaction prompt must be a user message");
                };
                assert!(text.contains("<conversation>"));
                assert!(text.contains("old user text"));
                assert!(text.contains("Additional focus: paths"));
                Ok("checkpoint summary".to_string())
            },
        );
        let mut events = Vec::new();

        let result = agent
            .compact_with_cancellation(
                &streamer,
                TurnCancellation::new(),
                None,
                Some("paths"),
                |event| {
                    events.push(format_event(event));
                    Ok(())
                },
            )
            .await
            .unwrap();

        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(result.summary, "checkpoint summary");
        assert_eq!(result.first_kept_message_id, MessageId::new(3));
        assert!(matches!(
            agent.messages().last().unwrap().item(),
            AgentItem::Compaction { summary, .. } if summary == "checkpoint summary"
        ));
        let context = agent
            .store
            .conversation_window_with_compaction(agent.context_window, agent.compaction);
        assert_eq!(context.len(), 3);
        assert!(matches!(
            &context[0],
            ConversationMessage::Message { text, .. }
                if text.contains("checkpoint summary")
        ));
        assert_eq!(context[1], ConversationMessage::user("recent request"));
        assert_eq!(context[2], ConversationMessage::assistant("recent answer"));
        assert_eq!(
            events,
            [
                "status:Running",
                "compaction-start:manual",
                "compaction-end:manual:3",
                "status:Idle",
            ]
        );
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
    #[ignore = "release-mode large tool round benchmark; run explicitly with --ignored --nocapture"]
    async fn benchmark_tool_round_large_outputs() {
        const TOOL_CALLS: usize = 24;
        const FILE_BYTES: usize = 64 * 1024;
        const SAMPLES: usize = 10;

        let temp = std::env::temp_dir().join(format!(
            "rust-agent-loop-large-tools-{}-{}",
            std::process::id(),
            unique_nanos()
        ));
        let _ = fs::remove_dir_all(&temp);
        fs::create_dir_all(&temp).unwrap();
        fs::write(temp.join("large.txt"), "x".repeat(FILE_BYTES)).unwrap();

        let tools = ToolRegistry::for_root_with_policy(&temp, ToolPolicy::ReadOnly);
        let mut samples = Vec::with_capacity(SAMPLES);
        let mut stored_messages = 0usize;
        let mut stored_bytes = 0usize;

        for _ in 0..SAMPLES {
            let mut agent = AgentLoop::with_context_window_and_tools(
                SessionId::new(7),
                ContextWindowConfig::with_history_limits(
                    200,
                    4 * 1024 * 1024,
                    200,
                    4 * 1024 * 1024,
                ),
                SessionConfig::new("test-model"),
                tools.clone(),
            );
            let streamer = LargeReadToolStreamer {
                turn: AtomicUsize::new(0),
                calls: TOOL_CALLS,
            };

            let started = Instant::now();
            agent
                .submit_user_message("read large files", &streamer, |_| Ok(()))
                .await
                .unwrap();
            let elapsed = started.elapsed();

            stored_messages = agent.messages().len();
            stored_bytes = agent.messages().iter().map(AgentMessage::input_bytes).sum();
            std::hint::black_box(agent.messages());
            samples.push(elapsed);
        }

        fs::remove_dir_all(&temp).unwrap();

        let summary = DurationSummary::from_samples(&mut samples);
        let tool_output_bytes = TOOL_CALLS * FILE_BYTES;
        println!(
            "tool_round_large_outputs calls={TOOL_CALLS} file_bytes={FILE_BYTES} tool_output_bytes={tool_output_bytes} stored_messages={stored_messages} stored_bytes={stored_bytes} samples={SAMPLES} min_ms={:.3} median_ms={:.3} max_ms={:.3}",
            summary.min_ms(),
            summary.median_ms(),
            summary.max_ms(),
        );
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
                "turn-usage:100:80:10",
                "status:Idle",
                "message:user:second",
                "status:Running",
                "cache:reused_prefix",
                "message:assistant:second answer",
                "turn-usage:100:80:10",
                "status:Idle",
            ]
        );
    }

    #[test]
    fn cache_status_detects_changed_input_prefix() {
        let mut store =
            InMemorySessionStore::new(SessionId::new(7), SessionConfig::new("test-model"));
        store.last_cache_observation = Some(CacheObservation {
            prompt_cache_key: "cache-key".to_string(),
            stable_prefix_hash: 0x1234,
            request_input_hash: Some(0xaaaa),
            request_input_message_count: Some(1),
        });
        let cache_health = CacheHealth {
            model: "test-model".to_string(),
            prompt_cache_key: "cache-key".to_string(),
            stable_prefix_hash: 0x1234,
            stable_prefix_bytes: 16,
            request_input_hash: 0xbbbb,
            request_input_prefix_hashes: vec![0, 0xbbbb],
            message_count: 1,
            input_bytes: 5,
            response_id: None,
            usage: None,
            cache_status: CacheStatus::FirstRequest,
        };

        assert_eq!(
            store.cache_status(&cache_health),
            CacheStatus::InputPrefixChanged
        );
    }

    #[tokio::test]
    async fn aggregates_turn_usage_across_tool_followups() {
        let mut agent = AgentLoop::new(SessionId::new(7));
        let streamer = UsageToolStreamer {
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

        let turn_usage_events = events
            .iter()
            .filter(|event| event.starts_with("turn-usage:"))
            .cloned()
            .collect::<Vec<_>>();
        assert_eq!(turn_usage_events, ["turn-usage:300:170:15"]);
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
    async fn continues_past_previous_tool_round_limit() {
        let mut agent = AgentLoop::new(SessionId::new(7));
        let streamer = ManyToolRoundsStreamer {
            turn: AtomicUsize::new(0),
            rounds: 9,
        };
        let mut events = Vec::new();

        agent
            .submit_user_message("keep reading", &streamer, |event| {
                events.push(format_event(event));
                Ok(())
            })
            .await
            .unwrap();

        assert_eq!(agent.store.status(), SessionStatus::Idle);
        assert_eq!(agent.messages().last().unwrap().text(), "done");
        assert_eq!(streamer.turn.load(Ordering::SeqCst), 10);
        assert_eq!(
            events
                .iter()
                .filter(|event| event.starts_with("tool-start:read_file:"))
                .count(),
            9
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
    async fn terminal_command_is_recorded_in_context() {
        let _shell_guard = crate::tools::SHELL_TEST_LOCK.lock().await;
        let temp = std::env::temp_dir().join(format!(
            "rust-agent-loop-terminal-{}-{}",
            std::process::id(),
            unique_nanos()
        ));
        let _ = fs::remove_dir_all(&temp);
        fs::create_dir_all(&temp).unwrap();
        let tools = ToolRegistry::for_root_with_policy(&temp, ToolPolicy::WorkspaceExec);
        let mut agent = AgentLoop::with_context_window_and_tools(
            SessionId::new(7),
            ContextWindowConfig::default(),
            SessionConfig::new("test-model"),
            tools,
        );
        let mut events = Vec::new();

        let result = agent
            .run_terminal_command_with_cancellation(
                "printf terminal-ok",
                TurnCancellation::new(),
                |event| {
                    events.push(format_event(event));
                    Ok(())
                },
            )
            .await
            .unwrap();

        assert!(result.success, "{}", result.output);
        assert!(result.output.contains("terminal-ok"));
        assert_eq!(agent.tool_policy(), ToolPolicy::WorkspaceExec);
        assert_eq!(
            events,
            [
                "message:user:$ printf terminal-ok",
                "status:Running",
                "tool-start:exec_command:terminal_1",
                "tool-end:exec_command:terminal_1:true",
                "status:Idle",
            ]
        );

        let streamer = FnStreamer::new(
            |history: &[ConversationMessage<'_>],
             tools: &[ToolSpec],
             _parallel_tool_calls: bool,
             _model: &str,
             _on_delta: &mut (dyn FnMut(&str) -> Result<()> + Send)| {
                assert!(tools.iter().any(|tool| tool.name() == "exec_command"));
                assert_eq!(history.len(), 4);
                assert_eq!(
                    history[0],
                    ConversationMessage::user("$ printf terminal-ok")
                );
                match history[1] {
                    ConversationMessage::FunctionCall {
                        call_id,
                        name,
                        arguments,
                        ..
                    } => {
                        assert_eq!(call_id, "terminal_1");
                        assert_eq!(name, "exec_command");
                        assert!(arguments.contains("printf terminal-ok"));
                    }
                    _ => panic!("expected terminal function call in prompt"),
                }
                match history[2] {
                    ConversationMessage::FunctionOutput {
                        call_id,
                        output,
                        success,
                    } => {
                        assert_eq!(call_id, "terminal_1");
                        assert!(output.contains("terminal-ok"));
                        assert!(success);
                    }
                    _ => panic!("expected terminal function output in prompt"),
                }
                assert_eq!(history[3], ConversationMessage::user("what happened?"));
                Ok("saw terminal".to_string())
            },
        );
        agent
            .submit_user_message("what happened?", &streamer, |_| Ok(()))
            .await
            .unwrap();

        fs::remove_dir_all(&temp).unwrap();
    }

    #[tokio::test]
    async fn terminal_command_requires_workspace_exec_policy() {
        let temp = std::env::temp_dir().join(format!(
            "rust-agent-loop-terminal-denied-{}-{}",
            std::process::id(),
            unique_nanos()
        ));
        let _ = fs::remove_dir_all(&temp);
        fs::create_dir_all(&temp).unwrap();
        let tools = ToolRegistry::for_root_with_policy(&temp, ToolPolicy::ReadOnly);
        let mut agent = AgentLoop::with_context_window_and_tools(
            SessionId::new(7),
            ContextWindowConfig::default(),
            SessionConfig::new("test-model"),
            tools,
        );
        let mut events = Vec::new();

        let error = agent
            .run_terminal_command_with_cancellation(
                "printf denied",
                TurnCancellation::new(),
                |event| {
                    events.push(format_event(event));
                    Ok(())
                },
            )
            .await
            .unwrap_err()
            .to_string();

        assert_eq!(
            error,
            "terminal commands require workspace-exec tool policy"
        );
        assert!(events.is_empty());
        assert!(agent.messages().is_empty());

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
        assert!(matches!(
            agent.messages()[1].item,
            AgentItem::FunctionCall { .. }
        ));
        assert!(matches!(
            agent.messages()[2].item,
            AgentItem::FunctionOutput { .. }
        ));

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
    async fn auto_compaction_failure_does_not_stick_session_running() {
        let mut agent = AgentLoop::with_context_window_and_compaction(
            SessionId::new(7),
            ContextWindowConfig::default(),
            CompactionConfig::for_test(1, 0, 1),
            SessionConfig::new("test-model"),
        );
        agent
            .store
            .append_message(MessageRole::User, "old user text");
        agent
            .store
            .append_message(MessageRole::Assistant, "old answer text");
        let mut events = Vec::new();
        let failing_compactor = FnStreamer::new(
            |_history: &[ConversationMessage<'_>],
             tools: &[ToolSpec],
             _parallel_tool_calls: bool,
             _model: &str,
             _on_delta: &mut (dyn FnMut(&str) -> Result<()> + Send)| {
                assert!(tools.is_empty());
                anyhow::bail!("summary failed")
            },
        );

        let error = agent
            .submit_user_message("new request", &failing_compactor, |event| {
                events.push(format_event(event));
                Ok(())
            })
            .await
            .unwrap_err()
            .to_string();

        assert_eq!(error, "summary failed");
        assert_eq!(agent.store.status(), SessionStatus::Failed);
        assert_eq!(
            events,
            [
                "message:user:new request",
                "status:Running",
                "compaction-start:threshold",
                "status:Failed",
                "error:summary failed",
            ]
        );

        agent.compaction = CompactionConfig::disabled();
        let ok_streamer = FnStreamer::new(
            |_history: &[ConversationMessage<'_>],
             _tools: &[ToolSpec],
             _parallel_tool_calls: bool,
             _model: &str,
             _on_delta: &mut (dyn FnMut(&str) -> Result<()> + Send)| {
                Ok("ok".to_string())
            },
        );
        agent
            .submit_user_message("retry", &ok_streamer, |_| Ok(()))
            .await
            .unwrap();

        assert_eq!(agent.store.status(), SessionStatus::Idle);
        assert_eq!(agent.messages().last().unwrap().text(), "ok");
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
                AgentItem::Message { .. } | AgentItem::Compaction { .. } => call_ids.clear(),
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
            AgentEvent::TurnTokenUsage { usage, .. } => {
                format!(
                    "turn-usage:{}:{}:{}",
                    usage.input_tokens.unwrap_or(0),
                    usage.cached_input_tokens.unwrap_or(0),
                    usage.output_tokens.unwrap_or(0)
                )
            }
            AgentEvent::CompactionStarted { reason, .. } => {
                format!("compaction-start:{}", reason.as_str())
            }
            AgentEvent::CompactionCompleted { reason, result, .. } => format!(
                "compaction-end:{}:{}",
                reason.as_str(),
                result.first_kept_message_id.get()
            ),
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

    fn test_cache_health(
        model: &str,
        turn: usize,
        request_input_hash: u64,
        request_input_prefix_hashes: Vec<u64>,
        message_count: usize,
        usage: TokenUsage,
    ) -> CacheHealth {
        CacheHealth {
            model: model.to_string(),
            prompt_cache_key: format!("cache-key-{model}"),
            stable_prefix_hash: 0x1234,
            stable_prefix_bytes: 24,
            request_input_hash,
            request_input_prefix_hashes,
            message_count,
            input_bytes: 5,
            response_id: Some(format!("resp_{turn}")),
            usage: Some(usage),
            cache_status: CacheStatus::FirstRequest,
        }
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
                test_cache_health(
                    model,
                    turn,
                    0x5678,
                    vec![0, 0x5678],
                    1,
                    TokenUsage::new(Some(100), Some(80), Some(10), Some(2), Some(110)),
                ),
            ))
        }
    }

    struct UsageToolStreamer {
        turn: AtomicUsize,
    }

    impl ModelStreamer for UsageToolStreamer {
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
            match turn {
                0 => Ok(ModelResponse::with_cache_health(
                    "",
                    [ModelToolCall {
                        item_id: Some("fc_read".to_string()),
                        call_id: "call_read".to_string(),
                        name: "read_file".to_string(),
                        arguments: r#"{"path":"Cargo.toml"}"#.to_string(),
                    }],
                    test_cache_health(
                        model,
                        turn,
                        0x10,
                        vec![0, 0x10],
                        1,
                        TokenUsage::new(Some(100), Some(50), Some(5), Some(1), Some(105)),
                    ),
                )),
                1 => Ok(ModelResponse::with_cache_health(
                    "done",
                    [],
                    test_cache_health(
                        model,
                        turn,
                        0x20,
                        vec![0, 0x10, 0x18, 0x20],
                        3,
                        TokenUsage::new(Some(200), Some(120), Some(10), Some(2), Some(210)),
                    ),
                )),
                _ => unreachable!("unexpected turn"),
            }
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
                            ConversationMessage::function_call(
                                Some("fc_read"),
                                "call_read",
                                "read_file",
                                r#"{"path":"Cargo.toml"}"#
                            ),
                            ConversationMessage::function_output(
                                "call_read",
                                include_str!("../Cargo.toml"),
                                true
                            ),
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

    struct ManyToolRoundsStreamer {
        turn: AtomicUsize,
        rounds: usize,
    }

    struct LargeReadToolStreamer {
        turn: AtomicUsize,
        calls: usize,
    }

    impl ModelStreamer for ManyToolRoundsStreamer {
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
            assert!(!tools.is_empty());
            assert!(parallel_tool_calls);
            assert_eq!(messages.len(), 1 + turn * 2);

            if turn < self.rounds {
                return Ok(ModelResponse::with_tool_calls(
                    "",
                    [ModelToolCall {
                        item_id: Some(format!("fc_read_{turn}")),
                        call_id: format!("call_read_{turn}"),
                        name: "read_file".to_string(),
                        arguments: r#"{"path":"Cargo.toml"}"#.to_string(),
                    }],
                ));
            }

            assert_eq!(turn, self.rounds);
            on_delta("done")?;
            Ok(ModelResponse::new("done"))
        }
    }

    impl ModelStreamer for LargeReadToolStreamer {
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
                    assert_eq!(messages, &[ConversationMessage::user("read large files")]);
                    let tool_calls = (0..self.calls).map(|index| ModelToolCall {
                        item_id: Some(format!("fc_read_{index}")),
                        call_id: format!("call_read_{index}"),
                        name: "read_file".to_string(),
                        arguments: r#"{"path":"large.txt"}"#.to_string(),
                    });
                    Ok(ModelResponse::with_tool_calls("", tool_calls))
                }
                1 => {
                    assert_eq!(messages.len(), 1 + self.calls * 2);
                    Ok(ModelResponse::new("done"))
                }
                _ => unreachable!("unexpected turn"),
            }
        }
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
