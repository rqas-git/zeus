//! In-memory agent loop for ordered session turns.

use anyhow::Result;

use crate::client::ConversationMessage;

/// Strong identifier for an agent session.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct SessionId(u64);

impl SessionId {
    /// Creates a session identifier from a stable numeric value.
    pub(crate) const fn new(value: u64) -> Self {
        Self(value)
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

    fn conversation_message(&self) -> ConversationMessage {
        match self.role {
            MessageRole::User => ConversationMessage::user(self.text.clone()),
            MessageRole::Assistant => ConversationMessage::assistant(self.text.clone()),
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

/// In-memory storage for one session.
#[derive(Debug)]
pub(crate) struct InMemorySessionStore {
    session_id: SessionId,
    status: SessionStatus,
    next_message_id: MessageId,
    messages: Vec<AgentMessage>,
}

impl InMemorySessionStore {
    /// Creates empty session storage.
    pub(crate) fn new(session_id: SessionId) -> Self {
        Self {
            session_id,
            status: SessionStatus::Idle,
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

    /// Returns stored messages in order.
    pub(crate) fn messages(&self) -> &[AgentMessage] {
        &self.messages
    }

    fn set_status(&mut self, status: SessionStatus) {
        self.status = status;
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

    fn conversation_history(&self) -> Vec<ConversationMessage> {
        self.messages
            .iter()
            .map(AgentMessage::conversation_message)
            .collect()
    }
}

/// Runs ordered turns for a single in-memory session.
#[derive(Debug)]
pub(crate) struct AgentLoop {
    store: InMemorySessionStore,
}

impl AgentLoop {
    /// Creates an agent loop for one session.
    pub(crate) fn new(session_id: SessionId) -> Self {
        Self {
            store: InMemorySessionStore::new(session_id),
        }
    }

    /// Returns the session identifier.
    pub(crate) const fn session_id(&self) -> SessionId {
        self.store.session_id()
    }

    /// Returns stored messages in order.
    #[cfg(test)]
    pub(crate) fn messages(&self) -> &[AgentMessage] {
        self.store.messages()
    }

    /// Appends a user message, streams the model response, and stores the assistant message.
    ///
    /// # Errors
    /// Returns an error if the session is already running, model streaming fails, or event publishing fails.
    pub(crate) fn submit_user_message(
        &mut self,
        text: impl Into<String>,
        stream_model: impl FnOnce(
            &[ConversationMessage],
            &mut dyn FnMut(&str) -> Result<()>,
        ) -> Result<String>,
        mut emit: impl FnMut(AgentEvent<'_>) -> Result<()>,
    ) -> Result<String> {
        anyhow::ensure!(
            self.store.status() != SessionStatus::Running,
            "session is already running"
        );

        let user_text = text.into();
        let user_id = self.store.append_message(MessageRole::User, user_text);
        emit(AgentEvent::MessageCompleted {
            session_id: self.session_id(),
            message_id: user_id,
            role: MessageRole::User,
            text: self
                .store
                .messages()
                .last()
                .map(AgentMessage::text)
                .unwrap_or_default(),
        })?;

        self.set_status(SessionStatus::Running, &mut emit)?;
        let result = self.run_once(stream_model, &mut emit);
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

    fn run_once(
        &mut self,
        stream_model: impl FnOnce(
            &[ConversationMessage],
            &mut dyn FnMut(&str) -> Result<()>,
        ) -> Result<String>,
        emit: &mut impl FnMut(AgentEvent<'_>) -> Result<()>,
    ) -> Result<String> {
        let history = self.store.conversation_history();
        let session_id = self.session_id();
        let mut on_delta = |delta: &str| emit(AgentEvent::TextDelta { session_id, delta });
        let assistant_text = stream_model(&history, &mut on_delta)?;
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
    use super::*;

    #[test]
    fn stores_one_turn_and_emits_ordered_events() {
        let mut agent = AgentLoop::new(SessionId::new(7));
        let mut events = Vec::new();

        let response = agent
            .submit_user_message(
                "hello",
                |history, on_delta| {
                    assert_eq!(history.len(), 1);
                    on_delta("hi")?;
                    Ok("hi".to_string())
                },
                |event| {
                    events.push(format_event(event));
                    Ok(())
                },
            )
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

    #[test]
    fn sends_full_history_on_later_turns() {
        let mut agent = AgentLoop::new(SessionId::new(7));
        agent
            .submit_user_message(
                "remember rust-agent",
                |history, _| {
                    assert_eq!(history.len(), 1);
                    Ok("remembered".to_string())
                },
                |_| Ok(()),
            )
            .unwrap();

        let response = agent
            .submit_user_message(
                "what did I ask you to remember?",
                |history, on_delta| {
                    assert_eq!(history.len(), 3);
                    on_delta("rust-agent")?;
                    Ok("rust-agent".to_string())
                },
                |_| Ok(()),
            )
            .unwrap();

        assert_eq!(response, "rust-agent");
        assert_eq!(agent.messages().len(), 4);
        assert_eq!(agent.messages()[3].text(), "rust-agent");
    }

    #[test]
    fn records_failed_status_and_error_event() {
        let mut agent = AgentLoop::new(SessionId::new(7));
        let mut events = Vec::new();

        let error = agent
            .submit_user_message(
                "hello",
                |_history, _| anyhow::bail!("backend failed"),
                |event| {
                    events.push(format_event(event));
                    Ok(())
                },
            )
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
}
