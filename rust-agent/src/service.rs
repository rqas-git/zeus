//! Long-lived agent service for backend integrations.

use std::collections::HashMap;

use anyhow::Result;

use crate::agent_loop::AgentEvent;
use crate::agent_loop::AgentLoop;
use crate::agent_loop::ModelStreamer;
use crate::agent_loop::SessionConfig;
use crate::agent_loop::SessionId;
use crate::config::ContextWindowConfig;
use crate::config::ModelConfig;

/// Reuses a model client and keeps conversation state by session.
#[derive(Debug)]
pub(crate) struct AgentService<M> {
    model: M,
    context_window: ContextWindowConfig,
    model_config: ModelConfig,
    sessions: HashMap<SessionId, AgentLoop>,
}

impl<M> AgentService<M>
where
    M: ModelStreamer,
{
    /// Creates an empty service around a long-lived model client.
    pub(crate) fn new(
        model: M,
        context_window: ContextWindowConfig,
        model_config: ModelConfig,
    ) -> Self {
        Self {
            model,
            context_window,
            model_config,
            sessions: HashMap::new(),
        }
    }

    /// Returns the model selected for a session, or the default for a new session.
    pub(crate) fn session_model(&self, session_id: SessionId) -> &str {
        self.sessions
            .get(&session_id)
            .map_or_else(|| self.model_config.default_model(), AgentLoop::model)
    }

    /// Returns the backend allowlist for model changes.
    pub(crate) fn allowed_models(&self) -> &[String] {
        self.model_config.allowed_models()
    }

    /// Changes the selected model for future turns in a session.
    ///
    /// # Errors
    /// Returns an error if the model is not allowed or the session is currently running.
    pub(crate) fn set_session_model(&mut self, session_id: SessionId, model: &str) -> Result<&str> {
        let model = self.model_config.allowed_model(model)?.to_string();
        let agent = match self.sessions.entry(session_id) {
            std::collections::hash_map::Entry::Occupied(entry) => {
                let agent = entry.into_mut();
                agent.set_model(model)?;
                agent
            }
            std::collections::hash_map::Entry::Vacant(entry) => {
                entry.insert(AgentLoop::with_context_window(
                    session_id,
                    self.context_window,
                    SessionConfig::new(model),
                ))
            }
        };
        Ok(agent.model())
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
        let default_model = self.model_config.default_model();
        let agent = self.sessions.entry(session_id).or_insert_with(|| {
            AgentLoop::with_context_window(
                session_id,
                self.context_window,
                SessionConfig::new(default_model),
            )
        });
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

    use crate::agent_loop::ModelResponse;
    use crate::client::ConversationMessage;

    use super::*;

    #[tokio::test]
    async fn reuses_sessions_and_keeps_client_warm() {
        let turn = Cell::new(0);
        let model = FnStreamer::new(
            |history: &[ConversationMessage<'_>],
             selected_model: &str,
             _on_delta: &mut dyn FnMut(&str) -> Result<()>| {
                assert_eq!(selected_model, "test-default");
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
        let mut service =
            AgentService::new(model, ContextWindowConfig::default(), test_model_config());

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

    #[tokio::test]
    async fn changes_session_model_for_future_turns() {
        let turn = Cell::new(0);
        let model = FnStreamer::new(
            |_history: &[ConversationMessage<'_>],
             selected_model: &str,
             _on_delta: &mut dyn FnMut(&str) -> Result<()>| {
                match turn.get() {
                    0 => {
                        assert_eq!(selected_model, "test-default");
                        turn.set(1);
                    }
                    1 => {
                        assert_eq!(selected_model, "test-fast");
                        turn.set(2);
                    }
                    _ => unreachable!("unexpected turn"),
                }
                Ok(selected_model.to_string())
            },
        );
        let mut service =
            AgentService::new(model, ContextWindowConfig::default(), test_model_config());

        let first = service
            .submit_user_message(SessionId::new(1), "hello", |_| Ok(()))
            .await
            .unwrap();
        let selected = service
            .set_session_model(SessionId::new(1), "test-fast")
            .unwrap()
            .to_string();
        let second = service
            .submit_user_message(SessionId::new(1), "again", |_| Ok(()))
            .await
            .unwrap();

        assert_eq!(first, "test-default");
        assert_eq!(selected, "test-fast");
        assert_eq!(second, "test-fast");
        assert_eq!(turn.get(), 2);
    }

    #[test]
    fn rejects_unsupported_session_model() {
        let model = FnStreamer::new(
            |_history: &[ConversationMessage<'_>],
             _selected_model: &str,
             _on_delta: &mut dyn FnMut(&str) -> Result<()>| Ok("unused".to_string()),
        );
        let mut service =
            AgentService::new(model, ContextWindowConfig::default(), test_model_config());

        let error = service
            .set_session_model(SessionId::new(1), "unknown-model")
            .unwrap_err()
            .to_string();

        assert!(error.contains("unsupported model"));
        assert_eq!(service.session_count(), 0);
        assert_eq!(service.session_model(SessionId::new(1)), "test-default");
    }

    fn test_model_config() -> ModelConfig {
        ModelConfig::new("test-default", ["test-default", "test-fast"]).unwrap()
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
        ) -> Result<ModelResponse> {
            (self.stream.borrow_mut())(messages, model, on_delta).map(ModelResponse::new)
        }
    }
}
