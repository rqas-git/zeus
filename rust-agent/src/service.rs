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
use crate::agent_loop::TurnCancellation;
use crate::config::ContextWindowConfig;
use crate::config::ModelConfig;
use crate::storage::SessionDatabase;
use crate::tools::ToolRegistry;

/// Reuses a model client and keeps conversation state by session.
#[derive(Debug)]
pub(crate) struct AgentService<M> {
    model: M,
    context_window: ContextWindowConfig,
    model_config: ModelConfig,
    tools: ToolRegistry,
    database: Option<SessionDatabase>,
    max_sessions: usize,
    sessions: Mutex<HashMap<SessionId, SharedSession>>,
}

#[derive(Debug)]
struct SessionHandle {
    agent: AsyncMutex<AgentLoop>,
    active_turn: Mutex<Option<TurnCancellation>>,
}

type SharedSession = Arc<SessionHandle>;

impl SessionHandle {
    fn new(agent: AgentLoop) -> Self {
        Self {
            agent: AsyncMutex::new(agent),
            active_turn: Mutex::new(None),
        }
    }

    fn begin_turn(&self) -> Result<TurnCancellation> {
        let cancellation = TurnCancellation::new();
        let mut active_turn = self
            .active_turn
            .lock()
            .map_err(|_| anyhow::anyhow!("active turn lock was poisoned"))?;
        *active_turn = Some(cancellation.clone());
        Ok(cancellation)
    }

    fn clear_turn(&self) -> Result<()> {
        let mut active_turn = self
            .active_turn
            .lock()
            .map_err(|_| anyhow::anyhow!("active turn lock was poisoned"))?;
        *active_turn = None;
        Ok(())
    }

    fn cancel_turn(&self) -> Result<bool> {
        let active_turn = self
            .active_turn
            .lock()
            .map_err(|_| anyhow::anyhow!("active turn lock was poisoned"))?;
        let Some(cancellation) = active_turn.as_ref() else {
            return Ok(false);
        };
        cancellation.cancel();
        Ok(true)
    }
}

impl<M> AgentService<M>
where
    M: ModelStreamer + Sync,
{
    /// Creates an empty service around a long-lived model client.
    #[cfg(test)]
    pub(crate) fn new(
        model: M,
        context_window: ContextWindowConfig,
        model_config: ModelConfig,
    ) -> Self {
        Self::with_tools(model, context_window, model_config, ToolRegistry::default())
    }

    /// Creates an empty service with an explicit shared tool registry.
    pub(crate) fn with_tools(
        model: M,
        context_window: ContextWindowConfig,
        model_config: ModelConfig,
        tools: ToolRegistry,
    ) -> Self {
        Self {
            model,
            context_window,
            model_config,
            tools,
            database: None,
            max_sessions: usize::MAX,
            sessions: Mutex::new(HashMap::new()),
        }
    }

    /// Enables SQLite-backed session storage.
    pub(crate) fn with_database(mut self, database: SessionDatabase) -> Self {
        self.database = Some(database);
        self
    }

    /// Sets the maximum number of retained sessions.
    pub(crate) fn with_session_limit(mut self, max_sessions: usize) -> Self {
        self.max_sessions = max_sessions.max(1);
        self
    }

    /// Creates an idle session with the default model.
    ///
    /// # Errors
    /// Returns an error when the session map is full or cannot be locked.
    pub(crate) fn create_session(&self, session_id: SessionId) -> Result<()> {
        let mut sessions = self.lock_sessions()?;
        if sessions.contains_key(&session_id) {
            return Ok(());
        }
        self.ensure_can_insert_session(&sessions)?;
        sessions.insert(
            session_id,
            Self::new_session(
                session_id,
                self.context_window,
                SessionConfig::new(self.model_config.default_model()),
                self.tools.clone(),
                self.database.clone(),
            )?,
        );
        Ok(())
    }

    /// Deletes a session from memory and durable storage if present.
    pub(crate) fn delete_session(&self, session_id: SessionId) -> Result<bool> {
        let removed_from_memory = self.lock_sessions()?.remove(&session_id).is_some();
        let removed_from_storage = match &self.database {
            Some(database) => database.delete_session(session_id)?,
            None => false,
        };
        Ok(removed_from_memory || removed_from_storage)
    }

    /// Requests cancellation of the currently running turn for a session.
    pub(crate) fn cancel_session_turn(&self, session_id: SessionId) -> Result<bool> {
        let Some(session) = self.session(session_id)? else {
            return Ok(false);
        };
        session.cancel_turn()
    }

    /// Returns the model selected for a session, or the default for a new session.
    pub(crate) async fn session_model(&self, session_id: SessionId) -> Result<String> {
        if let Some(session) = self.session(session_id)? {
            let agent = session.agent.lock().await;
            return Ok(agent.model().to_string());
        }
        if let Some(database) = &self.database {
            if let Some(stored) = database.load_session(session_id)? {
                return Ok(stored.config.model().to_string());
            }
        }
        Ok(self.model_config.default_model().to_string())
    }

    /// Returns the backend allowlist for model changes.
    pub(crate) fn allowed_models(&self) -> &[String] {
        self.model_config.allowed_models()
    }

    /// Returns the configured default model for new sessions.
    pub(crate) fn default_model(&self) -> &str {
        self.model_config.default_model()
    }

    /// Changes the selected model for future turns in a session.
    ///
    /// # Errors
    /// Returns an error if the model is not allowed or the session is currently busy.
    pub(crate) fn set_session_model(&self, session_id: SessionId, model: &str) -> Result<String> {
        let model = self.model_config.allowed_model(model)?.to_string();
        let session = {
            let mut sessions = self.lock_sessions()?;
            if let Some(session) = sessions.get(&session_id) {
                Arc::clone(session)
            } else {
                self.ensure_can_insert_session(&sessions)?;
                let session = Self::new_session(
                    session_id,
                    self.context_window,
                    SessionConfig::new(model.clone()),
                    self.tools.clone(),
                    self.database.clone(),
                )?;
                {
                    let Ok(mut agent) = session.agent.try_lock() else {
                        anyhow::bail!("cannot change model while session is running");
                    };
                    agent.set_model(model.clone())?;
                }
                sessions.insert(session_id, session);
                return Ok(model);
            }
        };

        let Ok(mut agent) = session.agent.try_lock() else {
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
        emit: impl FnMut(AgentEvent<'_>) -> Result<()> + Send,
    ) -> Result<()> {
        let session = self.session_or_insert_default(session_id)?;
        let mut agent = session.agent.lock().await;
        let cancellation = session.begin_turn()?;
        let result = agent
            .submit_user_message_with_cancellation(message, &self.model, cancellation, emit)
            .await;
        session.clear_turn()?;
        result
    }

    fn session(&self, session_id: SessionId) -> Result<Option<SharedSession>> {
        Ok(self.lock_sessions()?.get(&session_id).map(Arc::clone))
    }

    fn session_or_insert_default(&self, session_id: SessionId) -> Result<SharedSession> {
        let mut sessions = self.lock_sessions()?;
        if let Some(session) = sessions.get(&session_id) {
            return Ok(Arc::clone(session));
        }
        self.ensure_can_insert_session(&sessions)?;
        let session = Self::new_session(
            session_id,
            self.context_window,
            SessionConfig::new(self.model_config.default_model()),
            self.tools.clone(),
            self.database.clone(),
        )?;
        sessions.insert(session_id, Arc::clone(&session));
        Ok(session)
    }

    fn new_session(
        session_id: SessionId,
        context_window: ContextWindowConfig,
        config: SessionConfig,
        tools: ToolRegistry,
        database: Option<SessionDatabase>,
    ) -> Result<SharedSession> {
        let agent = match database {
            Some(database) => AgentLoop::with_context_window_tools_and_database(
                session_id,
                context_window,
                config,
                tools,
                database,
            )?,
            None => {
                AgentLoop::with_context_window_and_tools(session_id, context_window, config, tools)
            }
        };
        Ok(Arc::new(SessionHandle::new(agent)))
    }

    fn lock_sessions(
        &self,
    ) -> Result<std::sync::MutexGuard<'_, HashMap<SessionId, SharedSession>>> {
        self.sessions
            .lock()
            .map_err(|_| anyhow::anyhow!("session map lock was poisoned"))
            .context("failed to lock session map")
    }

    fn ensure_can_insert_session(
        &self,
        sessions: &HashMap<SessionId, SharedSession>,
    ) -> Result<()> {
        anyhow::ensure!(sessions.len() < self.max_sessions, "session limit exceeded");
        Ok(())
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

    use crate::agent_loop::is_turn_cancelled;
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
             _on_delta: &mut (dyn FnMut(&str) -> Result<()> + Send)| {
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
             _on_delta: &mut (dyn FnMut(&str) -> Result<()> + Send)| {
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
             _on_delta: &mut (dyn FnMut(&str) -> Result<()> + Send)| {
                Ok("unused".to_string())
            },
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

    #[test]
    fn enforces_configured_session_limit() {
        let model = FnStreamer::new(
            |_history: &[ConversationMessage<'_>],
             _tools: &[ToolSpec],
             _parallel_tool_calls: bool,
             _selected_model: &str,
             _on_delta: &mut (dyn FnMut(&str) -> Result<()> + Send)| {
                Ok("unused".to_string())
            },
        );
        let service = AgentService::new(model, ContextWindowConfig::default(), test_model_config())
            .with_session_limit(1);

        service.create_session(SessionId::new(1)).unwrap();
        let error = service
            .create_session(SessionId::new(2))
            .unwrap_err()
            .to_string();

        assert!(error.contains("session limit exceeded"));
        assert_eq!(service.session_count().unwrap(), 1);
    }

    #[test]
    fn delete_session_removes_state_and_frees_limit() {
        let model = FnStreamer::new(
            |_history: &[ConversationMessage<'_>],
             _tools: &[ToolSpec],
             _parallel_tool_calls: bool,
             _selected_model: &str,
             _on_delta: &mut (dyn FnMut(&str) -> Result<()> + Send)| {
                Ok("unused".to_string())
            },
        );
        let service = AgentService::new(model, ContextWindowConfig::default(), test_model_config())
            .with_session_limit(1);

        service.create_session(SessionId::new(1)).unwrap();
        assert_eq!(service.session_count().unwrap(), 1);

        assert!(service.delete_session(SessionId::new(1)).unwrap());
        assert_eq!(service.session_count().unwrap(), 0);
        assert!(!service.delete_session(SessionId::new(1)).unwrap());

        service.create_session(SessionId::new(2)).unwrap();
        assert_eq!(service.session_count().unwrap(), 1);
    }

    #[tokio::test]
    async fn sqlite_database_survives_service_recreation_and_delete() {
        let database = SessionDatabase::in_memory().unwrap();
        let first_service = AgentService::new(
            FnStreamer::new(
                |history: &[ConversationMessage<'_>],
                 _tools: &[ToolSpec],
                 _parallel_tool_calls: bool,
                 _selected_model: &str,
                 _on_delta: &mut (dyn FnMut(&str) -> Result<()> + Send)| {
                    assert_eq!(history, &[ConversationMessage::user("hello")]);
                    Ok("one".to_string())
                },
            ),
            ContextWindowConfig::default(),
            test_model_config(),
        )
        .with_database(database.clone());

        first_service
            .submit_user_message(SessionId::new(1), "hello", |_| Ok(()))
            .await
            .unwrap();
        drop(first_service);

        let second_service = AgentService::new(
            FnStreamer::new(
                |history: &[ConversationMessage<'_>],
                 _tools: &[ToolSpec],
                 _parallel_tool_calls: bool,
                 _selected_model: &str,
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
            ),
            ContextWindowConfig::default(),
            test_model_config(),
        )
        .with_database(database.clone());

        second_service
            .submit_user_message(SessionId::new(1), "again", |_| Ok(()))
            .await
            .unwrap();
        assert_eq!(
            database
                .load_session(SessionId::new(1))
                .unwrap()
                .unwrap()
                .messages
                .len(),
            4
        );

        assert!(second_service.delete_session(SessionId::new(1)).unwrap());
        assert!(database.load_session(SessionId::new(1)).unwrap().is_none());
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

    #[tokio::test]
    async fn cancels_running_turn_and_allows_next_turn() {
        let first_started = Arc::new(Notify::new());
        let release_first = Arc::new(Notify::new());
        let started_calls = Arc::new(AtomicUsize::new(0));
        let service = Arc::new(AgentService::new(
            CancellableStreamer {
                first_started: Arc::clone(&first_started),
                release_first: Arc::clone(&release_first),
                started_calls: Arc::clone(&started_calls),
            },
            ContextWindowConfig::default(),
            test_model_config(),
        ));

        let running = tokio::spawn({
            let service = Arc::clone(&service);
            async move {
                service
                    .submit_user_message(SessionId::new(1), "one", |_| Ok(()))
                    .await
            }
        });

        first_started.notified().await;
        assert!(service.cancel_session_turn(SessionId::new(1)).unwrap());

        let error = tokio::time::timeout(Duration::from_secs(1), running)
            .await
            .expect("cancelled turn should finish promptly")
            .expect("turn task should not panic")
            .unwrap_err();
        assert!(is_turn_cancelled(&error), "{error}");
        assert!(!service.cancel_session_turn(SessionId::new(1)).unwrap());

        service
            .submit_user_message(SessionId::new(1), "two", |_| Ok(()))
            .await
            .unwrap();
        assert_eq!(started_calls.load(Ordering::SeqCst), 2);
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
            on_delta: &'a mut (dyn FnMut(&str) -> Result<()> + Send),
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

    struct CancellableStreamer {
        first_started: Arc<Notify>,
        release_first: Arc<Notify>,
        started_calls: Arc<AtomicUsize>,
    }

    impl ModelStreamer for CancellableStreamer {
        async fn stream_conversation<'a>(
            &'a self,
            _messages: &'a [ConversationMessage<'a>],
            _tools: &'a [ToolSpec],
            _parallel_tool_calls: bool,
            _session_id: SessionId,
            _model: &'a str,
            on_delta: &'a mut (dyn FnMut(&str) -> Result<()> + Send),
        ) -> Result<ModelResponse> {
            let call = self.started_calls.fetch_add(1, Ordering::SeqCst) + 1;
            if call == 1 {
                self.first_started.notify_one();
                self.release_first.notified().await;
            }

            on_delta("ok")?;
            Ok(ModelResponse::new("ok"))
        }
    }

    impl ModelStreamer for OrderedStreamer {
        async fn stream_conversation<'a>(
            &'a self,
            _messages: &'a [ConversationMessage<'a>],
            _tools: &'a [ToolSpec],
            _parallel_tool_calls: bool,
            _session_id: SessionId,
            _model: &'a str,
            on_delta: &'a mut (dyn FnMut(&str) -> Result<()> + Send),
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
