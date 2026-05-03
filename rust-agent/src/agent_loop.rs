//! In-memory agent loop for ordered session turns.

use std::future::Future;

use anyhow::Result;

use crate::client::ConversationMessage;
use crate::config::ContextWindowConfig;

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

/// Current execution state for a session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SessionStatus {
    Idle,
    Running,
    Failed,
}

/// Message stored in the current session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AgentMessage {
    id: MessageId,
    role: MessageRole,
    text: String,
}

impl AgentMessage {
    /// Returns the message role.
    #[cfg(test)]
    pub(crate) const fn role(&self) -> MessageRole {
        self.role
    }

    /// Returns the message text.
    pub(crate) fn text(&self) -> &str {
        &self.text
    }

    fn conversation_message(&self) -> ConversationMessage<'_> {
        match self.role {
            MessageRole::User => ConversationMessage::user(&self.text),
            MessageRole::Assistant => ConversationMessage::assistant(&self.text),
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
    Error {
        session_id: SessionId,
        message: &'a str,
    },
}

/// Streams a prompt window into an assistant response.
pub(crate) trait ModelStreamer {
    /// Sends the prompt window and streams assistant text deltas.
    fn stream_conversation<'a>(
        &'a self,
        messages: &'a [ConversationMessage<'a>],
        session_id: SessionId,
        model: &'a str,
        on_delta: &'a mut dyn FnMut(&str) -> Result<()>,
    ) -> impl Future<Output = Result<String>> + 'a;
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
        let id = self.next_message_id;
        self.next_message_id = self.next_message_id.next();
        self.messages.push(AgentMessage {
            id,
            role,
            text: text.into(),
        });
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
        let mut retained = Vec::new();
        let mut retained_bytes = 0usize;

        for message in self.messages.iter().rev() {
            if retained.len() >= config.max_messages() {
                break;
            }

            let message_bytes = message.text().len();
            let would_exceed_budget =
                retained_bytes.saturating_add(message_bytes) > config.max_bytes();
            if would_exceed_budget && !retained.is_empty() {
                break;
            }

            retained.push(message.conversation_message());
            retained_bytes = retained_bytes.saturating_add(message_bytes);
        }

        retained.reverse();
        retained
    }
}

/// Runs ordered turns for a single in-memory session.
#[derive(Debug)]
pub(crate) struct AgentLoop {
    store: InMemorySessionStore,
    context_window: ContextWindowConfig,
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
    pub(crate) fn with_context_window(
        session_id: SessionId,
        context_window: ContextWindowConfig,
        config: SessionConfig,
    ) -> Self {
        Self {
            store: InMemorySessionStore::new(session_id, config),
            context_window,
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
        mut emit: impl FnMut(AgentEvent<'_>) -> Result<()>,
    ) -> Result<String> {
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
        let result = self.run_once(model, &mut emit).await;
        match result {
            Ok(response) => {
                self.set_status(SessionStatus::Idle, &mut emit)?;
                Ok(response)
            }
            Err(error) => {
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

    fn begin_running(&mut self, emit: &mut impl FnMut(AgentEvent<'_>) -> Result<()>) -> Result<()> {
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
        emit: &mut impl FnMut(AgentEvent<'_>) -> Result<()>,
    ) -> Result<()> {
        self.store.set_status(status);
        emit(AgentEvent::StatusChanged {
            session_id: self.session_id(),
            status,
        })
    }

    async fn run_once(
        &mut self,
        model: &impl ModelStreamer,
        emit: &mut impl FnMut(AgentEvent<'_>) -> Result<()>,
    ) -> Result<String> {
        let history = self.store.conversation_window(self.context_window);
        let session_id = self.session_id();
        let selected_model = self.store.model();
        let mut on_delta = |delta: &str| emit(AgentEvent::TextDelta { session_id, delta });
        let assistant_text = model
            .stream_conversation(&history, session_id, selected_model, &mut on_delta)
            .await?;
        let assistant_id = self
            .store
            .append_message(MessageRole::Assistant, assistant_text.clone());
        emit(AgentEvent::MessageCompleted {
            session_id,
            message_id: assistant_id,
            role: MessageRole::Assistant,
            text: &assistant_text,
        })?;
        Ok(assistant_text)
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::cell::RefCell;

    use super::*;

    #[tokio::test]
    async fn stores_one_turn_and_emits_ordered_events() {
        let mut agent = AgentLoop::new(SessionId::new(7));
        let mut events = Vec::new();
        let streamer = FnStreamer::new(
            |history: &[ConversationMessage<'_>],
             _model: &str,
             on_delta: &mut dyn FnMut(&str) -> Result<()>| {
                assert_eq!(history.len(), 1);
                on_delta("hi")?;
                Ok("hi".to_string())
            },
        );

        let response = agent
            .submit_user_message("hello", &streamer, |event| {
                events.push(format_event(event));
                Ok(())
            })
            .await
            .unwrap();

        assert_eq!(response, "hi");
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
        let turn = Cell::new(0);
        let streamer = FnStreamer::new(
            |history: &[ConversationMessage<'_>],
             _model: &str,
             on_delta: &mut dyn FnMut(&str) -> Result<()>| {
                match turn.get() {
                    0 => {
                        assert_eq!(history.len(), 1);
                        turn.set(1);
                        Ok("remembered".to_string())
                    }
                    1 => {
                        assert_eq!(history.len(), 3);
                        on_delta("rust-agent")?;
                        turn.set(2);
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

        let response = agent
            .submit_user_message("what did I ask you to remember?", &streamer, |_| Ok(()))
            .await
            .unwrap();

        assert_eq!(response, "rust-agent");
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
        let turn = Cell::new(0);
        let streamer = FnStreamer::new(
            |history: &[ConversationMessage<'_>],
             _model: &str,
             _on_delta: &mut dyn FnMut(&str) -> Result<()>| {
                match turn.get() {
                    0 => {
                        turn.set(1);
                        Ok("first answer".to_string())
                    }
                    1 => {
                        turn.set(2);
                        Ok("second answer".to_string())
                    }
                    2 => {
                        assert_eq!(history.len(), 2);
                        assert_eq!(history[0], ConversationMessage::assistant("second answer"));
                        assert_eq!(history[1], ConversationMessage::user("third user"));
                        turn.set(3);
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

        let response = agent
            .submit_user_message("third user", &streamer, |_| Ok(()))
            .await
            .unwrap();

        assert_eq!(response, "third answer");
        assert_eq!(agent.messages().len(), 6);
    }

    #[tokio::test]
    async fn sends_selected_model_to_streamer() {
        let mut agent = AgentLoop::with_context_window(
            SessionId::new(7),
            ContextWindowConfig::default(),
            SessionConfig::new("first-model"),
        );
        let turn = Cell::new(0);
        let streamer = FnStreamer::new(
            |_history: &[ConversationMessage<'_>],
             model: &str,
             _on_delta: &mut dyn FnMut(&str) -> Result<()>| {
                match turn.get() {
                    0 => {
                        assert_eq!(model, "first-model");
                        turn.set(1);
                    }
                    1 => {
                        assert_eq!(model, "second-model");
                        turn.set(2);
                    }
                    _ => unreachable!("unexpected turn"),
                }
                Ok(model.to_string())
            },
        );

        let first = agent
            .submit_user_message("hello", &streamer, |_| Ok(()))
            .await
            .unwrap();
        agent.set_model("second-model").unwrap();
        let second = agent
            .submit_user_message("again", &streamer, |_| Ok(()))
            .await
            .unwrap();

        assert_eq!(first, "first-model");
        assert_eq!(second, "second-model");
        assert_eq!(turn.get(), 2);
    }

    #[tokio::test]
    async fn records_failed_status_and_error_event() {
        let mut agent = AgentLoop::new(SessionId::new(7));
        let mut events = Vec::new();
        let streamer = FnStreamer::new(
            |_history: &[ConversationMessage<'_>],
             _model: &str,
             _on_delta: &mut dyn FnMut(&str) -> Result<()>| {
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
        let model_called = Cell::new(false);
        let streamer = FnStreamer::new(
            |_history: &[ConversationMessage<'_>],
             _model: &str,
             _on_delta: &mut dyn FnMut(&str) -> Result<()>| {
                model_called.set(true);
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
        assert!(!model_called.get());
        assert_eq!(agent.store.status(), SessionStatus::Idle);
        assert!(agent.messages().is_empty());

        let ok_streamer = FnStreamer::new(
            |history: &[ConversationMessage<'_>],
             _model: &str,
             _on_delta: &mut dyn FnMut(&str) -> Result<()>| {
                assert_eq!(history, &[ConversationMessage::user("retry")]);
                Ok("ok".to_string())
            },
        );
        let response = agent
            .submit_user_message("retry", &ok_streamer, |_| Ok(()))
            .await
            .unwrap();

        assert_eq!(response, "ok");
        assert_eq!(agent.messages().len(), 2);
        assert_eq!(agent.messages()[0].text(), "retry");
        assert_eq!(agent.messages()[1].text(), "ok");
    }

    #[tokio::test]
    async fn running_status_publish_failure_does_not_stick_session_running() {
        let mut agent = AgentLoop::new(SessionId::new(7));
        let model_called = Cell::new(false);
        let streamer = FnStreamer::new(
            |_history: &[ConversationMessage<'_>],
             _model: &str,
             _on_delta: &mut dyn FnMut(&str) -> Result<()>| {
                model_called.set(true);
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
        assert!(!model_called.get());
        assert_eq!(agent.store.status(), SessionStatus::Idle);
        assert!(agent.messages().is_empty());

        let ok_streamer = FnStreamer::new(
            |history: &[ConversationMessage<'_>],
             _model: &str,
             _on_delta: &mut dyn FnMut(&str) -> Result<()>| {
                assert_eq!(history, &[ConversationMessage::user("again")]);
                Ok("ok".to_string())
            },
        );
        let response = agent
            .submit_user_message("again", &ok_streamer, |_| Ok(()))
            .await
            .unwrap();

        assert_eq!(response, "ok");
        assert_eq!(agent.store.status(), SessionStatus::Idle);
        assert_eq!(agent.messages().len(), 2);
        assert_eq!(agent.messages()[0].text(), "again");
    }

    fn format_event(event: AgentEvent<'_>) -> String {
        match event {
            AgentEvent::StatusChanged { status, .. } => format!("status:{status:?}"),
            AgentEvent::TextDelta { delta, .. } => format!("delta:{delta}"),
            AgentEvent::MessageCompleted { role, text, .. } => {
                format!("message:{}:{text}", role_name(role))
            }
            AgentEvent::Error { message, .. } => format!("error:{message}"),
        }
    }

    fn role_name(role: MessageRole) -> &'static str {
        match role {
            MessageRole::User => "user",
            MessageRole::Assistant => "assistant",
        }
    }

    struct FnStreamer<F> {
        stream: RefCell<F>,
    }

    impl<F> FnStreamer<F> {
        fn new(stream: F) -> Self {
            Self {
                stream: RefCell::new(stream),
            }
        }
    }

    impl<F> ModelStreamer for FnStreamer<F>
    where
        F: for<'a> FnMut(
            &'a [ConversationMessage<'a>],
            &'a str,
            &'a mut dyn FnMut(&str) -> Result<()>,
        ) -> Result<String>,
    {
        async fn stream_conversation<'a>(
            &'a self,
            messages: &'a [ConversationMessage<'a>],
            _session_id: SessionId,
            model: &'a str,
            on_delta: &'a mut dyn FnMut(&str) -> Result<()>,
        ) -> Result<String> {
            (self.stream.borrow_mut())(messages, model, on_delta)
        }
    }
}
