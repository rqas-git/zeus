//! Long-lived agent service for backend integrations.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;

use anyhow::Context;
use anyhow::Result;
use tokio::sync::Mutex as AsyncMutex;

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
    sessions: Mutex<HashMap<SessionId, SharedSession>>,
}

type SharedSession = Arc<AsyncMutex<AgentLoop>>;

impl<M> AgentService<M>
where
    M: ModelStreamer + Sync,
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
            sessions: Mutex::new(HashMap::new()),
        }
    }

    /// Returns the model selected for a session, or the default for a new session.
    pub(crate) async fn session_model(&self, session_id: SessionId) -> Result<String> {
        let Some(session) = self.session(session_id)? else {
            return Ok(self.model_config.default_model().to_string());
        };
        let agent = session.lock().await;
        Ok(agent.model().to_string())
    }

    /// Returns the backend allowlist for model changes.
    pub(crate) fn allowed_models(&self) -> &[String] {
        self.model_config.allowed_models()
    }

    /// Changes the selected model for future turns in a session.
    ///
    /// # Errors
    /// Returns an error if the model is not allowed or the session is currently busy.
    pub(crate) fn set_session_model(&self, session_id: SessionId, model: &str) -> Result<String> {
        let model = self.model_config.allowed_model(model)?.to_string();
        let session = {
            let mut sessions = self.lock_sessions()?;
            match sessions.entry(session_id) {
                std::collections::hash_map::Entry::Occupied(entry) => Arc::clone(entry.get()),
                std::collections::hash_map::Entry::Vacant(entry) => {
                    entry.insert(Self::new_session(
                        session_id,
                        self.context_window,
                        SessionConfig::new(model.clone()),
                    ));
                    return Ok(model);
                }
            }
        };

        let Ok(mut agent) = session.try_lock() else {
            anyhow::bail!("cannot change model while session is running");
        };
        agent.set_model(model)?;
        Ok(agent.model().to_string())
    }

    /// Submits a user message to a session, creating the session if needed.
    ///
    /// # Errors
    /// Returns an error when model streaming or event publishing fails.
    pub(crate) async fn submit_user_message(
        &self,
        session_id: SessionId,
        message: impl Into<String>,
        emit: impl FnMut(AgentEvent<'_>) -> Result<()>,
    ) -> Result<()> {
        let session = self.session_or_insert_default(session_id)?;
        let mut agent = session.lock().await;
        agent.submit_user_message(message, &self.model, emit).await
    }

    fn session(&self, session_id: SessionId) -> Result<Option<SharedSession>> {
        Ok(self.lock_sessions()?.get(&session_id).map(Arc::clone))
    }

    fn session_or_insert_default(&self, session_id: SessionId) -> Result<SharedSession> {
        let mut sessions = self.lock_sessions()?;
        Ok(Arc::clone(sessions.entry(session_id).or_insert_with(
            || {
                Self::new_session(
                    session_id,
                    self.context_window,
                    SessionConfig::new(self.model_config.default_model()),
                )
            },
        )))
    }

    fn new_session(
        session_id: SessionId,
        context_window: ContextWindowConfig,
        config: SessionConfig,
    ) -> SharedSession {
        Arc::new(AsyncMutex::new(AgentLoop::with_context_window(
            session_id,
            context_window,
            config,
        )))
    }

    fn lock_sessions(
        &self,
    ) -> Result<std::sync::MutexGuard<'_, HashMap<SessionId, SharedSession>>> {
        self.sessions
            .lock()
            .map_err(|_| anyhow::anyhow!("session map lock was poisoned"))
            .context("failed to lock session map")
    }

    /// Returns the number of sessions held in memory.
    #[cfg(test)]
    pub(crate) fn session_count(&self) -> Result<usize> {
        Ok(self.lock_sessions()?.len())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicUsize;
    use std::sync::atomic::Ordering;
    use std::sync::Mutex as StdMutex;
    use std::time::Duration;

    use tokio::sync::Barrier;
    use tokio::sync::Notify;

    use crate::agent_loop::ModelResponse;
    use crate::client::ConversationMessage;
    use crate::tools::ToolSpec;

    use super::*;

    #[tokio::test]
    async fn reuses_sessions_and_keeps_client_warm() {
        let turn = AtomicUsize::new(0);
        let model = FnStreamer::new(
            |history: &[ConversationMessage<'_>],
             _tools: &[ToolSpec],
             _parallel_tool_calls: bool,
             selected_model: &str,
             _on_delta: &mut dyn FnMut(&str) -> Result<()>| {
                assert_eq!(selected_model, "test-default");
                match turn.load(Ordering::SeqCst) {
                    0 => {
                        assert_eq!(history.len(), 1);
                        turn.store(1, Ordering::SeqCst);
                        Ok("one".to_string())
                    }
                    1 => {
                        assert_eq!(history.len(), 3);
                        turn.store(2, Ordering::SeqCst);
                        Ok("two".to_string())
                    }
                    2 => {
                        assert_eq!(history.len(), 1);
                        turn.store(3, Ordering::SeqCst);
                        Ok("other".to_string())
                    }
                    _ => unreachable!("unexpected turn"),
                }
            },
        );
        let service = AgentService::new(model, ContextWindowConfig::default(), test_model_config());

        service
            .submit_user_message(SessionId::new(1), "hello", |_| Ok(()))
            .await
            .unwrap();
        service
            .submit_user_message(SessionId::new(1), "again", |_| Ok(()))
            .await
            .unwrap();
        service
            .submit_user_message(SessionId::new(2), "fresh", |_| Ok(()))
            .await
            .unwrap();

        assert_eq!(service.session_count().unwrap(), 2);
    }

    #[tokio::test]
    async fn changes_session_model_for_future_turns() {
        let turn = AtomicUsize::new(0);
        let model = FnStreamer::new(
            |_history: &[ConversationMessage<'_>],
             _tools: &[ToolSpec],
             _parallel_tool_calls: bool,
             selected_model: &str,
             _on_delta: &mut dyn FnMut(&str) -> Result<()>| {
                match turn.load(Ordering::SeqCst) {
                    0 => {
                        assert_eq!(selected_model, "test-default");
                        turn.store(1, Ordering::SeqCst);
                    }
                    1 => {
                        assert_eq!(selected_model, "test-fast");
                        turn.store(2, Ordering::SeqCst);
                    }
                    _ => unreachable!("unexpected turn"),
                }
                Ok(selected_model.to_string())
            },
        );
        let service = AgentService::new(model, ContextWindowConfig::default(), test_model_config());

        service
            .submit_user_message(SessionId::new(1), "hello", |_| Ok(()))
            .await
            .unwrap();
        let selected = service
            .set_session_model(SessionId::new(1), "test-fast")
            .unwrap()
            .to_string();
        service
            .submit_user_message(SessionId::new(1), "again", |_| Ok(()))
            .await
            .unwrap();

        assert_eq!(selected, "test-fast");
        assert_eq!(turn.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn rejects_unsupported_session_model() {
        let model = FnStreamer::new(
            |_history: &[ConversationMessage<'_>],
             _tools: &[ToolSpec],
             _parallel_tool_calls: bool,
             _selected_model: &str,
             _on_delta: &mut dyn FnMut(&str) -> Result<()>| Ok("unused".to_string()),
        );
        let service = AgentService::new(model, ContextWindowConfig::default(), test_model_config());

        let error = service
            .set_session_model(SessionId::new(1), "unknown-model")
            .unwrap_err()
            .to_string();

        assert!(error.contains("unsupported model"));
        assert_eq!(service.session_count().unwrap(), 0);
        assert_eq!(
            service.session_model(SessionId::new(1)).await.unwrap(),
            "test-default"
        );
    }

    #[tokio::test]
    async fn runs_different_sessions_concurrently() {
        let barrier = Arc::new(Barrier::new(2));
        let active = Arc::new(AtomicUsize::new(0));
        let max_active = Arc::new(AtomicUsize::new(0));
        let service = AgentService::new(
            ConcurrentStreamer {
                barrier: Arc::clone(&barrier),
                active: Arc::clone(&active),
                max_active: Arc::clone(&max_active),
            },
            ContextWindowConfig::default(),
            test_model_config(),
        );

        let result = tokio::time::timeout(Duration::from_secs(1), async {
            tokio::join!(
                service.submit_user_message(SessionId::new(1), "one", |_| Ok(())),
                service.submit_user_message(SessionId::new(2), "two", |_| Ok(())),
            )
        })
        .await
        .expect("different sessions should not block each other");

        result.0.unwrap();
        result.1.unwrap();
        assert_eq!(max_active.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn serializes_submissions_for_the_same_session() {
        let first_started = Arc::new(Notify::new());
        let release_first = Arc::new(Notify::new());
        let started_calls = Arc::new(AtomicUsize::new(0));
        let active = Arc::new(AtomicUsize::new(0));
        let max_active = Arc::new(AtomicUsize::new(0));
        let service = AgentService::new(
            OrderedStreamer {
                first_started: Arc::clone(&first_started),
                release_first: Arc::clone(&release_first),
                started_calls: Arc::clone(&started_calls),
                active: Arc::clone(&active),
                max_active: Arc::clone(&max_active),
            },
            ContextWindowConfig::default(),
            test_model_config(),
        );

        let releaser = async {
            first_started.notified().await;
            tokio::task::yield_now().await;
            tokio::task::yield_now().await;
            assert_eq!(started_calls.load(Ordering::SeqCst), 1);
            let error = service
                .set_session_model(SessionId::new(1), "test-fast")
                .unwrap_err()
                .to_string();
            assert!(error.contains("session is running"));
            release_first.notify_waiters();
        };

        let result = tokio::time::timeout(Duration::from_secs(1), async {
            tokio::join!(
                service.submit_user_message(SessionId::new(1), "one", |_| Ok(())),
                service.submit_user_message(SessionId::new(1), "two", |_| Ok(())),
                releaser,
            )
        })
        .await
        .expect("same-session submissions should complete in order");

        result.0.unwrap();
        result.1.unwrap();
        assert_eq!(started_calls.load(Ordering::SeqCst), 2);
        assert_eq!(max_active.load(Ordering::SeqCst), 1);
    }

    fn test_model_config() -> ModelConfig {
        ModelConfig::new("test-default", ["test-default", "test-fast"]).unwrap()
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
            &'a mut dyn FnMut(&str) -> Result<()>,
        ) -> Result<String>,
    {
        async fn stream_conversation<'a>(
            &'a self,
            messages: &'a [ConversationMessage<'a>],
            tools: &'a [ToolSpec],
            parallel_tool_calls: bool,
            _session_id: SessionId,
            model: &'a str,
            on_delta: &'a mut dyn FnMut(&str) -> Result<()>,
        ) -> Result<ModelResponse> {
            let mut stream = self.stream.lock().expect("test streamer lock poisoned");
            stream(messages, tools, parallel_tool_calls, model, on_delta).map(ModelResponse::new)
        }
    }

    struct ConcurrentStreamer {
        barrier: Arc<Barrier>,
        active: Arc<AtomicUsize>,
        max_active: Arc<AtomicUsize>,
    }

    impl ModelStreamer for ConcurrentStreamer {
        async fn stream_conversation<'a>(
            &'a self,
            _messages: &'a [ConversationMessage<'a>],
            _tools: &'a [ToolSpec],
            _parallel_tool_calls: bool,
            _session_id: SessionId,
            _model: &'a str,
            on_delta: &'a mut dyn FnMut(&str) -> Result<()>,
        ) -> Result<ModelResponse> {
            let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            update_max(&self.max_active, active);
            self.barrier.wait().await;
            on_delta("ok")?;
            self.active.fetch_sub(1, Ordering::SeqCst);
            Ok(ModelResponse::new("ok"))
        }
    }

    struct OrderedStreamer {
        first_started: Arc<Notify>,
        release_first: Arc<Notify>,
        started_calls: Arc<AtomicUsize>,
        active: Arc<AtomicUsize>,
        max_active: Arc<AtomicUsize>,
    }

    impl ModelStreamer for OrderedStreamer {
        async fn stream_conversation<'a>(
            &'a self,
            _messages: &'a [ConversationMessage<'a>],
            _tools: &'a [ToolSpec],
            _parallel_tool_calls: bool,
            _session_id: SessionId,
            _model: &'a str,
            on_delta: &'a mut dyn FnMut(&str) -> Result<()>,
        ) -> Result<ModelResponse> {
            let call = self.started_calls.fetch_add(1, Ordering::SeqCst) + 1;
            let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            update_max(&self.max_active, active);

            if call == 1 {
                self.first_started.notify_one();
                self.release_first.notified().await;
            }

            on_delta("ok")?;
            self.active.fetch_sub(1, Ordering::SeqCst);
            Ok(ModelResponse::new("ok"))
        }
    }

    fn update_max(max_active: &AtomicUsize, active: usize) {
        let mut previous = max_active.load(Ordering::SeqCst);
        while active > previous {
            match max_active.compare_exchange(previous, active, Ordering::SeqCst, Ordering::SeqCst)
            {
                Ok(_) => return,
                Err(current) => previous = current,
            }
        }
    }
}
