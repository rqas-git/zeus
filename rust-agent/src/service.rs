//! Long-lived agent service for backend integrations.

use std::collections::HashMap;

use anyhow::Result;

use crate::agent_loop::AgentEvent;
use crate::agent_loop::AgentLoop;
use crate::agent_loop::ModelStreamer;
use crate::agent_loop::SessionId;
use crate::config::ContextWindowConfig;

/// Reuses a model client and keeps conversation state by session.
#[derive(Debug)]
pub(crate) struct AgentService<M> {
    model: M,
    context_window: ContextWindowConfig,
    sessions: HashMap<SessionId, AgentLoop>,
}

impl<M> AgentService<M>
where
    M: ModelStreamer,
{
    /// Creates an empty service around a long-lived model client.
    pub(crate) fn new(model: M, context_window: ContextWindowConfig) -> Self {
        Self {
            model,
            context_window,
            sessions: HashMap::new(),
        }
    }

    /// Submits a user message to a session, creating the session if needed.
    ///
    /// # Errors
    /// Returns an error when model streaming or event publishing fails.
    pub(crate) async fn submit_user_message(
        &mut self,
        session_id: SessionId,
        message: impl Into<String>,
        emit: impl FnMut(AgentEvent<'_>) -> Result<()>,
    ) -> Result<String> {
        let model = &self.model;
        let agent = self
            .sessions
            .entry(session_id)
            .or_insert_with(|| AgentLoop::with_context_window(session_id, self.context_window));
        agent.submit_user_message(message, model, emit).await
    }

    /// Returns the number of sessions held in memory.
    #[cfg(test)]
    pub(crate) fn session_count(&self) -> usize {
        self.sessions.len()
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::cell::RefCell;
    use std::future::Future;

    use crate::client::ConversationMessage;

    use super::*;

    #[tokio::test]
    async fn reuses_sessions_and_keeps_client_warm() {
        let turn = Cell::new(0);
        let model = FnStreamer::new(
            |history: &[ConversationMessage<'_>], _on_delta: &mut dyn FnMut(&str) -> Result<()>| {
                match turn.get() {
                    0 => {
                        assert_eq!(history.len(), 1);
                        turn.set(1);
                        Ok("one".to_string())
                    }
                    1 => {
                        assert_eq!(history.len(), 3);
                        turn.set(2);
                        Ok("two".to_string())
                    }
                    2 => {
                        assert_eq!(history.len(), 1);
                        turn.set(3);
                        Ok("other".to_string())
                    }
                    _ => unreachable!("unexpected turn"),
                }
            },
        );
        let mut service = AgentService::new(model, ContextWindowConfig::default());

        let first = service
            .submit_user_message(SessionId::new(1), "hello", |_| Ok(()))
            .await
            .unwrap();
        let second = service
            .submit_user_message(SessionId::new(1), "again", |_| Ok(()))
            .await
            .unwrap();
        let other = service
            .submit_user_message(SessionId::new(2), "fresh", |_| Ok(()))
            .await
            .unwrap();

        assert_eq!(first, "one");
        assert_eq!(second, "two");
        assert_eq!(other, "other");
        assert_eq!(service.session_count(), 2);
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
            &'a mut dyn FnMut(&str) -> Result<()>,
        ) -> Result<String>,
    {
        fn stream_conversation<'a>(
            &'a self,
            messages: &'a [ConversationMessage<'a>],
            _session_id: SessionId,
            on_delta: &'a mut dyn FnMut(&str) -> Result<()>,
        ) -> impl Future<Output = Result<String>> + 'a {
            async move { (self.stream.borrow_mut())(messages, on_delta) }
        }
    }
}
