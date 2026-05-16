//! Native HTTP/3 and HTTP compatibility server.

use std::collections::HashMap;
use std::collections::HashSet;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;
use std::time::Instant;

use anyhow::Context;
use anyhow::Result;
use async_stream::stream;
use axum::body::Body;
use axum::extract::Path as AxumPath;
use axum::extract::Query;
use axum::extract::State;
use axum::http::header;
use axum::http::HeaderMap;
use axum::http::HeaderValue;
use axum::http::Request;
use axum::http::Response;
use axum::http::StatusCode;
use axum::middleware;
use axum::middleware::Next;
use axum::response::IntoResponse;
use axum::response::Json;
use axum::routing::get;
use axum::routing::post;
use axum::Router;
use base64::Engine;
use bytes::Bytes;
use rcgen::CertifiedKey;
use rustls::pki_types::pem::PemObject;
use serde::Deserialize;
use serde::Serialize;
use tokio::net::TcpListener;
use tokio::sync::broadcast;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tokio_stream::StreamExt;

use crate::agent_loop::is_turn_cancelled;
use crate::agent_loop::turn_cancelled_error;
use crate::agent_loop::AgentEvent;
use crate::agent_loop::AgentItem;
use crate::agent_loop::AgentMessage;
use crate::agent_loop::CacheHealth;
use crate::agent_loop::MessageRole;
use crate::agent_loop::ModelStreamer;
use crate::agent_loop::SessionId;
use crate::agent_loop::TokenUsage;
use crate::agent_loop::TurnCancellation;
use crate::compaction::CompactionDetails;
use crate::compaction::CompactionResult;
use crate::config::ServerConfig;
use crate::service::AgentService;
use crate::service::SessionLastMessage;
use crate::service::SessionMetadata;
use crate::service::SessionSnapshot;
use crate::tools::ToolPolicy;

// Server identity and content types are part of the Zeus API contract.
const SERVER_NAME: &str = "rust-agent";
const SSE_CONTENT_TYPE: &str = "text/event-stream";
// Heartbeats keep long-running SSE streams alive through local proxies without
// adding meaningful idle CPU work.
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(10);
// Direct-turn stream buffering mirrors terminal delta batching and prevents
// unbounded memory use if a client stops reading.
const DIRECT_TURN_EVENT_QUEUE_CAPACITY: usize = 1024;
const DIRECT_TURN_DELTA_FLUSH_INTERVAL: Duration = Duration::from_millis(16);
const DIRECT_TURN_DELTA_FLUSH_BYTES: usize = 4096;
// Parent checks are only a supervisor fallback; one second is fast enough to
// stop orphaned local servers without busy polling.
const PARENT_WATCH_INTERVAL: Duration = Duration::from_secs(1);
// 256 bits of token entropy is sufficient for local bearer auth and keeps the
// base64url readiness payload compact.
const GENERATED_TOKEN_BYTES: usize = 32;
// JavaScript clients represent integers exactly only through 2^53 - 1.
const MAX_JSON_SAFE_INTEGER: u64 = (1u64 << 53) - 1;
// Session list pagination defaults keep UI requests small while allowing
// explicit larger pages for sync operations.
const DEFAULT_SESSION_LIST_LIMIT: usize = 50;
const MAX_SESSION_LIST_LIMIT: usize = 200;
// Bump this only when changing the externally visible HTTP contract.
const SERVER_PROTOCOL_VERSION: u32 = 1;
const CONTRACT_SCHEMA_HASH_PLACEHOLDER: &str = "contract-schema-hash";
// These arrays are emitted by `/capabilities`; changing them affects Swift
// feature negotiation and contract fixtures.
const SERVER_TRANSPORTS: &[&str] = &["http/1.1", "http/2", "http/3"];
const SERVER_FEATURES: &[&str] = &[
    "workspace",
    "branch_switching",
    "sessions",
    "session_restore",
    "turn_streaming",
    "session_events",
    "terminal_command",
    "session_compaction",
];
const SERVER_ROUTE_GROUPS: &[&str] = &[
    "identity",
    "models",
    "permissions",
    "workspace",
    "sessions",
    "turns",
    "terminal",
    "compaction",
    "events",
];

/// Runs the local server until interrupted.
///
/// # Errors
/// Returns an error if either listener cannot start or exits unexpectedly.
pub(crate) async fn serve<M>(
    service: AgentService<M>,
    config: ServerConfig,
    workspace_root: PathBuf,
) -> Result<()>
where
    M: ModelStreamer + Send + Sync + 'static,
{
    let service = Arc::new(service);
    let h3_endpoint = h3_endpoint(&config)?;
    let h3_addr = h3_endpoint
        .local_addr()
        .context("failed to read H3 listener address")?;
    let http_listener = bind_http_listener(config.http_addr()).await?;
    let http_addr = http_listener
        .local_addr()
        .context("failed to read HTTP compatibility listener address")?;
    let auth = ServerAuth::from_config(config.auth_token())?;
    emit_server_ready(http_addr, h3_addr, &auth, &workspace_root)?;
    let state_config =
        ServerStateConfig::new(config.event_queue_capacity(), h3_addr, auth, workspace_root)
            .max_sessions(config.max_sessions())
            .max_event_channels(config.max_event_channels());
    let state = ServerState::with_config(service, state_config);
    let app = router(state);
    let parent_pid = config.parent_pid();
    let http_app = app.clone();
    let h3_app = app;

    let http_task = tokio::spawn(async move { run_http_listener(http_app, http_listener).await });
    let h3_task = tokio::spawn(async move { run_h3_listener(h3_app, h3_endpoint).await });

    tokio::select! {
        result = http_task => result.context("HTTP listener task failed")??,
        result = h3_task => result.context("H3 listener task failed")??,
        result = wait_for_parent_process(parent_pid) => result?,
        result = tokio::signal::ctrl_c() => result.context("failed to listen for shutdown signal")?,
    }

    Ok(())
}

async fn bind_http_listener(addr: SocketAddr) -> Result<TcpListener> {
    TcpListener::bind(addr)
        .await
        .with_context(|| format!("failed to bind HTTP compatibility listener at {addr}"))
}

async fn wait_for_parent_process(parent_pid: Option<libc::pid_t>) -> Result<()> {
    let Some(parent_pid) = parent_pid else {
        return futures_util::future::pending::<Result<()>>().await;
    };

    loop {
        if !process_is_running(parent_pid) {
            log_server_event(
                "server.parent.exited",
                "parent process {process.pid} exited; shutting down",
                serde_json::json!({
                    "process.pid": parent_pid,
                }),
            );
            return Ok(());
        }
        tokio::time::sleep(PARENT_WATCH_INTERVAL).await;
    }
}

fn process_is_running(pid: libc::pid_t) -> bool {
    // SAFETY: `kill(pid, 0)` performs permission/existence checks only and does
    // not send a signal. `pid` comes from local configuration.
    let status = unsafe { libc::kill(pid, 0) };
    if status == 0 {
        return true;
    }

    std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

fn router<M>(state: ServerState<M>) -> Router
where
    M: ModelStreamer + Send + Sync + 'static,
{
    Router::new()
        .route("/", get(root::<M>))
        .route("/healthz", get(healthz))
        .route("/capabilities", get(capabilities))
        .route("/models", get(models::<M>))
        .route("/permissions", get(permissions::<M>))
        .route("/workspace", get(workspace::<M>))
        .route("/workspace/branch", post(switch_workspace_branch::<M>))
        .route("/sessions:restore", post(restore_session::<M>))
        .route(
            "/sessions",
            get(list_sessions::<M>).post(create_session::<M>),
        )
        .route(
            "/sessions/{session_id}",
            get(get_session::<M>).delete(delete_session::<M>),
        )
        .route(
            "/sessions/{session_id}/model",
            get(session_model::<M>).put(set_session_model::<M>),
        )
        .route(
            "/sessions/{session_id}/permissions",
            get(session_permissions::<M>).put(set_session_permissions::<M>),
        )
        .route(
            "/sessions/{session_id}/turns:stream",
            post(stream_turn::<M>),
        )
        .route(
            "/sessions/{session_id}/turns:cancel",
            post(cancel_turn::<M>),
        )
        .route(
            "/sessions/{session_id}/terminal:run",
            post(run_terminal_command::<M>),
        )
        .route("/sessions/{session_id}/compact", post(compact_session::<M>))
        .route("/sessions/{session_id}/events", get(session_events::<M>))
        .with_state(state.clone())
        .layer(middleware::from_fn_with_state(
            state.clone(),
            require_auth::<M>,
        ))
        .layer(middleware::from_fn_with_state(state, add_alt_svc::<M>))
}

async fn run_http_listener(app: Router, listener: TcpListener) -> Result<()> {
    let local_addr = listener
        .local_addr()
        .context("failed to read HTTP compatibility listener address")?;
    log_server_event(
        "server.http.listen.start",
        "HTTP compatibility listener started at {server.address}",
        serde_json::json!({
            "server.address": local_addr.to_string(),
            "network.protocol.name": "http",
            "network.protocol.version": "1.1/2",
        }),
    );
    axum::serve(listener, app)
        .await
        .context("HTTP compatibility listener failed")
}

async fn run_h3_listener(app: Router, endpoint: quinn::Endpoint) -> Result<()> {
    let local_addr = endpoint
        .local_addr()
        .context("failed to read H3 listener address")?;
    log_server_event(
        "server.h3.listen.start",
        "HTTP/3 listener started at {server.address}",
        serde_json::json!({
            "server.address": local_addr.to_string(),
            "network.protocol.name": "http",
            "network.protocol.version": "3",
        }),
    );

    while let Some(incoming) = endpoint.accept().await {
        let app = app.clone();
        tokio::spawn(async move {
            if let Err(error) = handle_h3_connection(incoming, app).await {
                log_server_event(
                    "server.h3.connection.error",
                    "HTTP/3 connection failed with {error.message}",
                    serde_json::json!({
                        "error.message": error.to_string(),
                    }),
                );
            }
        });
    }

    Ok(())
}

fn h3_endpoint(config: &ServerConfig) -> Result<quinn::Endpoint> {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let (certs, key) = load_tls_identity(config)?;
    let mut tls_config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)?;
    tls_config.alpn_protocols = vec![b"h3".to_vec()];

    let mut server_config = quinn::ServerConfig::with_crypto(Arc::new(
        quinn::crypto::rustls::QuicServerConfig::try_from(tls_config)
            .context("failed to build QUIC rustls config")?,
    ));
    let transport_config =
        Arc::get_mut(&mut server_config.transport).context("failed to configure QUIC transport")?;
    transport_config
        .max_concurrent_bidi_streams(config.h3_max_concurrent_streams().into())
        .max_concurrent_uni_streams(config.h3_max_concurrent_streams().into())
        .max_idle_timeout(Some(
            config
                .h3_idle_timeout()
                .try_into()
                .context("failed to convert H3 idle timeout")?,
        ));

    quinn::Endpoint::server(server_config, config.h3_addr())
        .with_context(|| format!("failed to bind H3 listener at {}", config.h3_addr()))
}

fn emit_server_ready(
    http_addr: SocketAddr,
    h3_addr: SocketAddr,
    auth: &ServerAuth,
    workspace_root: &Path,
) -> Result<()> {
    let ready = server_ready_message(http_addr, h3_addr, auth, workspace_root);
    let message = serde_json::to_string(&ready).context("failed to serialize readiness message")?;
    eprintln!("{message}");
    Ok(())
}

fn log_server_event(name: &'static str, message: &'static str, fields: impl Serialize) {
    let entry = serde_json::json!({
        "event": name,
        "message": message,
        "fields": fields,
    });
    eprintln!("{entry}");
}

fn server_ready_message(
    http_addr: SocketAddr,
    h3_addr: SocketAddr,
    auth: &ServerAuth,
    workspace_root: &Path,
) -> ServerReadyMessage {
    ServerReadyMessage {
        event: "server_ready",
        name: SERVER_NAME,
        protocol_version: SERVER_PROTOCOL_VERSION,
        http_addr: http_addr.to_string(),
        h3_addr: h3_addr.to_string(),
        token: auth.token().to_string(),
        workspace_root: workspace_root.display().to_string(),
        pid: std::process::id(),
    }
}

fn load_tls_identity(
    config: &ServerConfig,
) -> Result<(
    Vec<rustls::pki_types::CertificateDer<'static>>,
    rustls::pki_types::PrivateKeyDer<'static>,
)> {
    match (config.tls_cert_path(), config.tls_key_path()) {
        (Some(cert_path), Some(key_path)) => {
            Ok((read_cert_chain(cert_path)?, read_private_key(key_path)?))
        }
        (None, None) => {
            let CertifiedKey { cert, signing_key } = generate_self_signed_cert()?;
            Ok((
                vec![cert.into()],
                rustls::pki_types::PrivateKeyDer::Pkcs8(signing_key.serialize_der().into()),
            ))
        }
        _ => anyhow::bail!(
            "RUST_AGENT_SERVER_TLS_CERT and RUST_AGENT_SERVER_TLS_KEY must be set together"
        ),
    }
}

fn generate_self_signed_cert() -> Result<CertifiedKey<rcgen::KeyPair>> {
    rcgen::generate_simple_self_signed(vec!["localhost".to_string(), "127.0.0.1".to_string()])
        .context("failed to generate self-signed H3 certificate")
}

fn read_cert_chain(path: &Path) -> Result<Vec<rustls::pki_types::CertificateDer<'static>>> {
    let certs = rustls::pki_types::CertificateDer::pem_file_iter(path)
        .with_context(|| format!("failed to read TLS certificate {}", path.display()))?
        .collect::<std::result::Result<Vec<_>, _>>()
        .with_context(|| format!("failed to parse TLS certificate {}", path.display()))?;
    anyhow::ensure!(
        !certs.is_empty(),
        "TLS certificate {} did not contain any certificates",
        path.display()
    );
    Ok(certs)
}

fn read_private_key(path: &Path) -> Result<rustls::pki_types::PrivateKeyDer<'static>> {
    rustls::pki_types::PrivateKeyDer::from_pem_file(path)
        .with_context(|| format!("failed to parse TLS private key {}", path.display()))
}

async fn handle_h3_connection(incoming: quinn::Incoming, app: Router) -> Result<()> {
    let connection = incoming.await.context("failed to accept QUIC connection")?;
    let mut h3_connection = h3::server::builder()
        .build(h3_quinn::Connection::new(connection))
        .await
        .context("failed to create H3 connection")?;

    loop {
        match h3_connection.accept().await {
            Ok(Some(resolver)) => {
                let app = app.clone();
                tokio::spawn(async move {
                    if let Err(error) = h3_axum::serve_h3_with_axum(app, resolver).await {
                        log_server_event(
                            "server.h3.request.error",
                            "HTTP/3 request failed with {error.message}",
                            serde_json::json!({
                                "error.message": error.to_string(),
                            }),
                        );
                    }
                });
            }
            Ok(None) => return Ok(()),
            Err(error) if h3_axum::is_graceful_h3_close(&error) => return Ok(()),
            Err(error) => return Err(error).context("H3 connection failed"),
        }
    }
}

struct ServerState<M> {
    service: Arc<AgentService<M>>,
    events: EventBus,
    sessions: SessionRegistry,
    auth: ServerAuth,
    alt_svc: HeaderValue,
    workspace_root: Arc<PathBuf>,
    turn_event_queue_capacity: usize,
}

struct ServerStateConfig {
    event_queue_capacity: usize,
    h3_addr: SocketAddr,
    auth: ServerAuth,
    max_sessions: usize,
    max_event_channels: usize,
    workspace_root: PathBuf,
    turn_event_queue_capacity: usize,
}

impl ServerStateConfig {
    fn new(
        event_queue_capacity: usize,
        h3_addr: SocketAddr,
        auth: ServerAuth,
        workspace_root: PathBuf,
    ) -> Self {
        Self {
            event_queue_capacity,
            h3_addr,
            auth,
            max_sessions: usize::MAX,
            max_event_channels: usize::MAX,
            workspace_root,
            turn_event_queue_capacity: DIRECT_TURN_EVENT_QUEUE_CAPACITY,
        }
    }

    const fn max_sessions(mut self, max_sessions: usize) -> Self {
        self.max_sessions = max_sessions;
        self
    }

    const fn max_event_channels(mut self, max_event_channels: usize) -> Self {
        self.max_event_channels = max_event_channels;
        self
    }
}

impl<M> Clone for ServerState<M> {
    fn clone(&self) -> Self {
        Self {
            service: Arc::clone(&self.service),
            events: self.events.clone(),
            sessions: self.sessions.clone(),
            auth: self.auth.clone(),
            alt_svc: self.alt_svc.clone(),
            workspace_root: Arc::clone(&self.workspace_root),
            turn_event_queue_capacity: self.turn_event_queue_capacity,
        }
    }
}

impl<M> ServerState<M> {
    #[cfg(test)]
    fn new(
        service: Arc<AgentService<M>>,
        event_queue_capacity: usize,
        h3_addr: SocketAddr,
    ) -> Self {
        Self::with_config(
            service,
            ServerStateConfig::new(
                event_queue_capacity,
                h3_addr,
                ServerAuth::for_test(),
                std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            ),
        )
    }

    fn with_config(service: Arc<AgentService<M>>, config: ServerStateConfig) -> Self {
        let alt_svc =
            HeaderValue::from_str(&format!("h3=\":{}\"; ma=86400", config.h3_addr.port()))
                .expect("generated Alt-Svc header must be valid");
        Self {
            service,
            events: EventBus::new(config.event_queue_capacity, config.max_event_channels),
            sessions: SessionRegistry::new(config.max_sessions),
            auth: config.auth,
            alt_svc,
            workspace_root: Arc::new(config.workspace_root),
            turn_event_queue_capacity: config.turn_event_queue_capacity,
        }
    }

    fn require_session(&self, session_id: u64) -> Result<SessionId> {
        let session_id = SessionId::new(session_id);
        anyhow::ensure!(self.sessions.contains(session_id)?, "session not found");
        Ok(session_id)
    }

    async fn delete_session(&self, session_id: SessionId) -> Result<bool>
    where
        M: ModelStreamer + Sync,
    {
        let removed_from_registry = self.sessions.remove(session_id)?;
        let removed_from_service = self.service.delete_session(session_id).await?;
        if removed_from_registry || removed_from_service {
            self.events.remove_session(session_id)?;
            return Ok(true);
        }
        Ok(false)
    }

    #[cfg(test)]
    fn register_session_for_test(&self, session_id: SessionId) {
        self.sessions
            .register_for_test(session_id)
            .expect("test session registration should succeed");
    }
}

#[derive(Clone)]
struct ServerAuth {
    token: Arc<str>,
}

impl ServerAuth {
    fn from_config(configured: Option<&str>) -> Result<Self> {
        match configured {
            Some(token) => {
                let token = token.trim();
                anyhow::ensure!(!token.is_empty(), "server bearer token cannot be empty");
                Ok(Self {
                    token: Arc::from(token),
                })
            }
            None => Ok(Self {
                token: Arc::from(generate_bearer_token()?),
            }),
        }
    }

    #[cfg(test)]
    fn for_test() -> Self {
        Self {
            token: Arc::from("test-token"),
        }
    }

    fn token(&self) -> &str {
        &self.token
    }

    fn authorizes(&self, headers: &HeaderMap) -> bool {
        let Some(value) = headers
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
        else {
            return false;
        };
        let Some((scheme, token)) = value.split_once(' ') else {
            return false;
        };
        scheme.eq_ignore_ascii_case("Bearer")
            && constant_time_eq(token.as_bytes(), self.token.as_bytes())
    }
}

fn generate_bearer_token() -> Result<String> {
    let mut bytes = [0u8; GENERATED_TOKEN_BYTES];
    getrandom::fill(&mut bytes).context("failed to generate server bearer token")?;
    Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes))
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    let mut diff = left.len() ^ right.len();
    let max_len = left.len().max(right.len());
    for index in 0..max_len {
        let left = left.get(index).copied().unwrap_or(0);
        let right = right.get(index).copied().unwrap_or(0);
        diff |= usize::from(left ^ right);
    }
    diff == 0
}

#[derive(Clone)]
struct SessionRegistry {
    sessions: Arc<Mutex<HashSet<SessionId>>>,
    max_sessions: usize,
}

impl SessionRegistry {
    fn new(max_sessions: usize) -> Self {
        Self {
            sessions: Arc::new(Mutex::new(HashSet::new())),
            max_sessions: max_sessions.max(1),
        }
    }

    fn reserve_random(&self) -> Result<SessionId> {
        for _ in 0..128 {
            let session_id = random_session_id()?;
            let mut sessions = self
                .sessions
                .lock()
                .map_err(|error| anyhow::anyhow!("session registry lock was poisoned: {error}"))?;
            anyhow::ensure!(sessions.len() < self.max_sessions, "session limit exceeded");
            if sessions.insert(session_id) {
                return Ok(session_id);
            }
        }
        anyhow::bail!("failed to allocate unique session id")
    }

    fn reserve(&self, session_id: SessionId) -> Result<bool> {
        let mut sessions = self
            .sessions
            .lock()
            .map_err(|error| anyhow::anyhow!("session registry lock was poisoned: {error}"))?;
        if sessions.contains(&session_id) {
            return Ok(false);
        }
        anyhow::ensure!(sessions.len() < self.max_sessions, "session limit exceeded");
        sessions.insert(session_id);
        Ok(true)
    }

    fn contains(&self, session_id: SessionId) -> Result<bool> {
        Ok(self
            .sessions
            .lock()
            .map_err(|error| anyhow::anyhow!("session registry lock was poisoned: {error}"))?
            .contains(&session_id))
    }

    fn remove(&self, session_id: SessionId) -> Result<bool> {
        Ok(self
            .sessions
            .lock()
            .map_err(|error| anyhow::anyhow!("session registry lock was poisoned: {error}"))?
            .remove(&session_id))
    }

    fn release(&self, session_id: SessionId) {
        let _ = self.remove(session_id);
    }

    #[cfg(test)]
    fn register_for_test(&self, session_id: SessionId) -> Result<()> {
        let mut sessions = self
            .sessions
            .lock()
            .map_err(|error| anyhow::anyhow!("session registry lock was poisoned: {error}"))?;
        anyhow::ensure!(sessions.len() < self.max_sessions, "session limit exceeded");
        sessions.insert(session_id);
        Ok(())
    }
}

fn random_session_id() -> Result<SessionId> {
    loop {
        let mut bytes = [0u8; 8];
        getrandom::fill(&mut bytes).context("failed to generate session id")?;
        let value = u64::from_le_bytes(bytes) & MAX_JSON_SAFE_INTEGER;
        if value != 0 {
            return Ok(SessionId::new(value));
        }
    }
}

#[derive(Clone)]
struct EventBus {
    sessions: Arc<Mutex<HashMap<SessionId, broadcast::Sender<ServerEvent>>>>,
    capacity: usize,
    max_channels: usize,
}

impl EventBus {
    fn new(capacity: usize, max_channels: usize) -> Self {
        Self {
            sessions: Arc::new(Mutex::new(HashMap::new())),
            capacity,
            max_channels: max_channels.max(1),
        }
    }

    fn subscribe(&self, session_id: SessionId) -> Result<broadcast::Receiver<ServerEvent>> {
        Ok(self.channel(session_id)?.subscribe())
    }

    fn publish(&self, event: ServerEvent) -> Result<()> {
        if let Some(sender) = self.existing_channel(event.session_id())? {
            let _ = sender.send(event);
        }
        Ok(())
    }

    fn channel(&self, session_id: SessionId) -> Result<broadcast::Sender<ServerEvent>> {
        let mut sessions = self
            .sessions
            .lock()
            .map_err(|error| anyhow::anyhow!("event bus lock was poisoned: {error}"))?;
        cleanup_empty_event_channels(&mut sessions);
        if let Some(sender) = sessions.get(&session_id) {
            return Ok(sender.clone());
        }
        anyhow::ensure!(
            sessions.len() < self.max_channels,
            "event channel limit exceeded"
        );
        let (sender, _receiver) = broadcast::channel(self.capacity);
        sessions.insert(session_id, sender.clone());
        Ok(sender)
    }

    fn existing_channel(
        &self,
        session_id: SessionId,
    ) -> Result<Option<broadcast::Sender<ServerEvent>>> {
        let mut sessions = self
            .sessions
            .lock()
            .map_err(|error| anyhow::anyhow!("event bus lock was poisoned: {error}"))?;
        cleanup_empty_event_channels(&mut sessions);
        Ok(sessions.get(&session_id).cloned())
    }

    fn remove_session(&self, session_id: SessionId) -> Result<()> {
        self.sessions
            .lock()
            .map_err(|error| anyhow::anyhow!("event bus lock was poisoned: {error}"))?
            .remove(&session_id);
        Ok(())
    }
}

fn cleanup_empty_event_channels(sessions: &mut HashMap<SessionId, broadcast::Sender<ServerEvent>>) {
    sessions.retain(|_, sender| sender.receiver_count() > 0);
}

async fn add_alt_svc<M>(
    State(state): State<ServerState<M>>,
    request: Request<Body>,
    next: Next,
) -> Response<Body>
where
    M: ModelStreamer + Send + Sync + 'static,
{
    let mut response = next.run(request).await;
    response
        .headers_mut()
        .entry(header::ALT_SVC)
        .or_insert_with(|| state.alt_svc.clone());
    response
}

async fn require_auth<M>(
    State(state): State<ServerState<M>>,
    request: Request<Body>,
    next: Next,
) -> Response<Body>
where
    M: ModelStreamer + Send + Sync + 'static,
{
    if is_public_path(request.uri().path()) || state.auth.authorizes(request.headers()) {
        return next.run(request).await;
    }
    unauthorized_response()
}

fn is_public_path(path: &str) -> bool {
    path == "/" || path == "/healthz" || path == "/capabilities"
}

fn unauthorized_response() -> Response<Body> {
    Response::builder()
        .status(StatusCode::UNAUTHORIZED)
        .header(header::WWW_AUTHENTICATE, "Bearer")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(r#"{"error":"missing or invalid bearer token"}"#))
        .expect("unauthorized response headers must be valid")
}

fn tool_policy_strings(policies: &[ToolPolicy]) -> Vec<String> {
    policies
        .iter()
        .map(|policy| policy.as_str().to_string())
        .collect()
}

async fn root<M>(State(state): State<ServerState<M>>) -> impl IntoResponse
where
    M: ModelStreamer + Send + Sync + 'static,
{
    Json(RootResponse {
        name: SERVER_NAME,
        protocol: "http/1.1,http/2,http/3",
        workspace_root: state.workspace_root.display().to_string(),
    })
}

async fn healthz() -> impl IntoResponse {
    Json(HealthResponse { healthy: true })
}

async fn capabilities() -> impl IntoResponse {
    Json(capabilities_response(contract_schema_hash()))
}

async fn models<M>(State(state): State<ServerState<M>>) -> impl IntoResponse
where
    M: ModelStreamer + Send + Sync + 'static,
{
    Json(ModelsResponse {
        default_model: state.service.default_model().to_string(),
        allowed_models: state.service.allowed_models().to_vec(),
        default_reasoning_effort: state.service.default_reasoning_effort().to_string(),
        reasoning_efforts: state.service.reasoning_efforts().to_vec(),
    })
}

async fn permissions<M>(State(state): State<ServerState<M>>) -> impl IntoResponse
where
    M: ModelStreamer + Send + Sync + 'static,
{
    Json(PermissionsResponse {
        default_tool_policy: state.service.default_tool_policy().as_str().to_string(),
        allowed_tool_policies: tool_policy_strings(ToolPolicy::all()),
    })
}

async fn workspace<M>(State(state): State<ServerState<M>>) -> impl IntoResponse
where
    M: ModelStreamer + Send + Sync + 'static,
{
    match state.service.workspace_snapshot() {
        Ok(snapshot) => Json(WorkspaceResponse::from_snapshot(snapshot)).into_response(),
        Err(error) => error_response(StatusCode::INTERNAL_SERVER_ERROR, error),
    }
}

async fn switch_workspace_branch<M>(
    State(state): State<ServerState<M>>,
    Json(request): Json<SwitchWorkspaceBranchRequest>,
) -> impl IntoResponse
where
    M: ModelStreamer + Send + Sync + 'static,
{
    match state.service.switch_workspace_branch(&request.branch) {
        Ok(result) => Json(SwitchWorkspaceBranchResponse::from_result(result)).into_response(),
        Err(error) => error_response(StatusCode::BAD_REQUEST, error),
    }
}

async fn create_session<M>(State(state): State<ServerState<M>>) -> impl IntoResponse
where
    M: ModelStreamer + Send + Sync + 'static,
{
    let session_id = match state.sessions.reserve_random() {
        Ok(session_id) => session_id,
        Err(error) => return error_response(StatusCode::TOO_MANY_REQUESTS, error),
    };
    if let Err(error) = state.service.create_session(session_id).await {
        state.sessions.release(session_id);
        return error_response(StatusCode::TOO_MANY_REQUESTS, error);
    }
    Json(CreateSessionResponse {
        session_id: session_id.get(),
        model: state.service.default_model().to_string(),
        tool_policy: state.service.default_tool_policy().as_str().to_string(),
    })
    .into_response()
}

async fn list_sessions<M>(
    State(state): State<ServerState<M>>,
    Query(query): Query<ListSessionsQuery>,
) -> impl IntoResponse
where
    M: ModelStreamer + Send + Sync + 'static,
{
    let (offset, limit) = match session_list_bounds(&query) {
        Ok(bounds) => bounds,
        Err(error) => return error_response(StatusCode::BAD_REQUEST, error),
    };
    let Some(fetch_limit) = limit.checked_add(1) else {
        return error_response(
            StatusCode::BAD_REQUEST,
            anyhow::anyhow!("session list limit overflowed"),
        );
    };

    match state
        .service
        .list_session_metadata(offset, fetch_limit)
        .await
    {
        Ok(mut sessions) => {
            let next_offset = if sessions.len() > limit {
                sessions.truncate(limit);
                offset.checked_add(limit)
            } else {
                None
            };
            Json(ListSessionsResponse {
                sessions: sessions
                    .into_iter()
                    .map(SessionMetadataResponse::from_metadata)
                    .collect(),
                limit,
                offset,
                next_offset,
            })
            .into_response()
        }
        Err(error) => error_response(StatusCode::INTERNAL_SERVER_ERROR, error),
    }
}

async fn get_session<M>(
    State(state): State<ServerState<M>>,
    AxumPath(session_id): AxumPath<u64>,
) -> impl IntoResponse
where
    M: ModelStreamer + Send + Sync + 'static,
{
    let session_id = match validated_session_id(session_id) {
        Ok(session_id) => session_id,
        Err(error) => return error_response(StatusCode::BAD_REQUEST, error),
    };
    match state.service.session_metadata(session_id).await {
        Ok(Some(metadata)) => {
            Json(SessionMetadataResponse::from_metadata(metadata)).into_response()
        }
        Ok(None) => error_response(StatusCode::NOT_FOUND, anyhow::anyhow!("session not found")),
        Err(error) => error_response(StatusCode::INTERNAL_SERVER_ERROR, error),
    }
}

async fn restore_session<M>(
    State(state): State<ServerState<M>>,
    Json(request): Json<RestoreSessionRequest>,
) -> impl IntoResponse
where
    M: ModelStreamer + Send + Sync + 'static,
{
    if !(1..=MAX_JSON_SAFE_INTEGER).contains(&request.session_id) {
        return error_response(
            StatusCode::BAD_REQUEST,
            anyhow::anyhow!("session_id must be between 1 and {MAX_JSON_SAFE_INTEGER}"),
        );
    }

    let session_id = SessionId::new(request.session_id);
    let reserved = match state.sessions.reserve(session_id) {
        Ok(reserved) => reserved,
        Err(error) => return error_response(StatusCode::TOO_MANY_REQUESTS, error),
    };

    match state.service.restore_session(session_id).await {
        Ok(Some(snapshot)) => Json(RestoreSessionResponse::from_snapshot(snapshot)).into_response(),
        Ok(None) => {
            if reserved {
                state.sessions.release(session_id);
            }
            error_response(StatusCode::NOT_FOUND, anyhow::anyhow!("session not found"))
        }
        Err(error) => {
            if reserved {
                state.sessions.release(session_id);
            }
            error_response(StatusCode::INTERNAL_SERVER_ERROR, error)
        }
    }
}

fn validated_session_id(session_id: u64) -> Result<SessionId> {
    anyhow::ensure!(
        (1..=MAX_JSON_SAFE_INTEGER).contains(&session_id),
        "session_id must be between 1 and {MAX_JSON_SAFE_INTEGER}"
    );
    Ok(SessionId::new(session_id))
}

fn session_list_bounds(query: &ListSessionsQuery) -> Result<(usize, usize)> {
    let offset = query.offset.unwrap_or(0);
    let limit = query.limit.unwrap_or(DEFAULT_SESSION_LIST_LIMIT);
    anyhow::ensure!(
        i64::try_from(offset).is_ok(),
        "offset exceeds SQLite signed integer range"
    );
    anyhow::ensure!(
        (1..=MAX_SESSION_LIST_LIMIT).contains(&limit),
        "limit must be between 1 and {MAX_SESSION_LIST_LIMIT}"
    );
    Ok((offset, limit))
}

async fn delete_session<M>(
    State(state): State<ServerState<M>>,
    AxumPath(session_id): AxumPath<u64>,
) -> impl IntoResponse
where
    M: ModelStreamer + Send + Sync + 'static,
{
    let session_id = match validated_session_id(session_id) {
        Ok(session_id) => session_id,
        Err(error) => return error_response(StatusCode::BAD_REQUEST, error),
    };
    match state.delete_session(session_id).await {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => error_response(StatusCode::NOT_FOUND, anyhow::anyhow!("session not found")),
        Err(error) => error_response(StatusCode::INTERNAL_SERVER_ERROR, error),
    }
}

async fn session_model<M>(
    State(state): State<ServerState<M>>,
    AxumPath(session_id): AxumPath<u64>,
) -> impl IntoResponse
where
    M: ModelStreamer + Send + Sync + 'static,
{
    let session_id = match state.require_session(session_id) {
        Ok(session_id) => session_id,
        Err(error) => return error_response(StatusCode::NOT_FOUND, error),
    };
    match state.service.session_model(session_id).await {
        Ok(model) => Json(SessionModelResponse { model }).into_response(),
        Err(error) => error_response(StatusCode::INTERNAL_SERVER_ERROR, error),
    }
}

async fn set_session_model<M>(
    State(state): State<ServerState<M>>,
    AxumPath(session_id): AxumPath<u64>,
    Json(request): Json<SetModelRequest>,
) -> impl IntoResponse
where
    M: ModelStreamer + Send + Sync + 'static,
{
    let session_id = match state.require_session(session_id) {
        Ok(session_id) => session_id,
        Err(error) => return error_response(StatusCode::NOT_FOUND, error),
    };
    match state
        .service
        .set_session_model(session_id, &request.model)
        .await
    {
        Ok(model) => Json(SessionModelResponse { model }).into_response(),
        Err(error) => error_response(StatusCode::BAD_REQUEST, error),
    }
}

async fn session_permissions<M>(
    State(state): State<ServerState<M>>,
    AxumPath(session_id): AxumPath<u64>,
) -> impl IntoResponse
where
    M: ModelStreamer + Send + Sync + 'static,
{
    let session_id = match state.require_session(session_id) {
        Ok(session_id) => session_id,
        Err(error) => return error_response(StatusCode::NOT_FOUND, error),
    };
    match state.service.session_tool_policy(session_id).await {
        Ok(policy) => Json(SessionPermissionsResponse {
            tool_policy: policy.as_str().to_string(),
        })
        .into_response(),
        Err(error) => error_response(StatusCode::INTERNAL_SERVER_ERROR, error),
    }
}

async fn set_session_permissions<M>(
    State(state): State<ServerState<M>>,
    AxumPath(session_id): AxumPath<u64>,
    Json(request): Json<SetPermissionsRequest>,
) -> impl IntoResponse
where
    M: ModelStreamer + Send + Sync + 'static,
{
    let session_id = match state.require_session(session_id) {
        Ok(session_id) => session_id,
        Err(error) => return error_response(StatusCode::NOT_FOUND, error),
    };
    let policy = match ToolPolicy::parse(&request.tool_policy) {
        Ok(policy) => policy,
        Err(error) => return error_response(StatusCode::BAD_REQUEST, error),
    };
    match state
        .service
        .set_session_tool_policy(session_id, policy)
        .await
    {
        Ok(policy) => Json(SessionPermissionsResponse {
            tool_policy: policy.as_str().to_string(),
        })
        .into_response(),
        Err(error) => error_response(StatusCode::BAD_REQUEST, error),
    }
}

async fn stream_turn<M>(
    State(state): State<ServerState<M>>,
    AxumPath(session_id): AxumPath<u64>,
    Json(request): Json<TurnRequest>,
) -> impl IntoResponse
where
    M: ModelStreamer + Send + Sync + 'static,
{
    if request.message.trim().is_empty() {
        return error_response(
            StatusCode::BAD_REQUEST,
            anyhow::anyhow!("message cannot be empty"),
        );
    }

    let session_id = match state.require_session(session_id) {
        Ok(session_id) => session_id,
        Err(error) => return error_response(StatusCode::NOT_FOUND, error),
    };
    let reasoning_effort = match request.reasoning_effort.as_deref() {
        Some(effort) => match state.service.allowed_reasoning_effort(effort) {
            Ok(effort) => Some(effort.to_string()),
            Err(error) => return error_response(StatusCode::BAD_REQUEST, error),
        },
        None => None,
    };
    let (tx, rx) = mpsc::channel(state.turn_event_queue_capacity);
    let service = Arc::clone(&state.service);
    let cancellation = TurnCancellation::new();
    let stream_cancellation = cancellation.clone();
    tokio::spawn(async move {
        let mut error_forwarded = false;
        let event_tx = tx.clone();
        let mut event_buffer = TurnEventBuffer::new(&event_tx);
        let result = service
            .submit_user_message_with_reasoning_effort_and_cancellation(
                session_id,
                request.message,
                reasoning_effort.as_deref(),
                cancellation,
                |event| {
                    let event = ServerEvent::from_agent_event(event);
                    let is_error = matches!(event, ServerEvent::Error { .. });
                    event_buffer.send(event)?;
                    if is_error {
                        error_forwarded = true;
                    }
                    Ok(())
                },
            )
            .await;

        match result {
            Ok(()) => {
                let event = ServerEvent::TurnCompleted {
                    session_id: session_id.get(),
                };
                let _ = event_buffer.send(event);
            }
            Err(error) if is_turn_cancelled(&error) => {
                let event = ServerEvent::TurnCancelled {
                    session_id: session_id.get(),
                };
                let _ = event_buffer.send(event);
            }
            Err(error) if !error_forwarded => {
                let event = ServerEvent::Error {
                    session_id: session_id.get(),
                    message: error.to_string(),
                };
                let _ = event_buffer.send(event);
            }
            Err(_) => {}
        }
    });

    sse_from_mpsc(rx, stream_cancellation)
}

async fn cancel_turn<M>(
    State(state): State<ServerState<M>>,
    AxumPath(session_id): AxumPath<u64>,
) -> impl IntoResponse
where
    M: ModelStreamer + Send + Sync + 'static,
{
    let session_id = match state.require_session(session_id) {
        Ok(session_id) => session_id,
        Err(error) => return error_response(StatusCode::NOT_FOUND, error),
    };
    match state.service.cancel_session_turn(session_id) {
        Ok(cancelled) => Json(CancelTurnResponse { cancelled }).into_response(),
        Err(error) => error_response(StatusCode::INTERNAL_SERVER_ERROR, error),
    }
}

async fn run_terminal_command<M>(
    State(state): State<ServerState<M>>,
    AxumPath(session_id): AxumPath<u64>,
    Json(request): Json<TerminalCommandRequest>,
) -> impl IntoResponse
where
    M: ModelStreamer + Send + Sync + 'static,
{
    if request.command.trim().is_empty() {
        return error_response(
            StatusCode::BAD_REQUEST,
            anyhow::anyhow!("command cannot be empty"),
        );
    }

    let session_id = match state.require_session(session_id) {
        Ok(session_id) => session_id,
        Err(error) => return error_response(StatusCode::NOT_FOUND, error),
    };
    let events = state.events.clone();
    match state
        .service
        .run_terminal_command(session_id, request.command, |event| {
            events.publish(ServerEvent::from_agent_event(event))
        })
        .await
    {
        Ok(result) => Json(TerminalCommandResponse {
            output: result.output,
            success: result.success,
        })
        .into_response(),
        Err(error) if is_turn_cancelled(&error) => error_response(StatusCode::CONFLICT, error),
        Err(error) => error_response(StatusCode::BAD_REQUEST, error),
    }
}

async fn compact_session<M>(
    State(state): State<ServerState<M>>,
    AxumPath(session_id): AxumPath<u64>,
    Json(request): Json<CompactSessionRequest>,
) -> impl IntoResponse
where
    M: ModelStreamer + Send + Sync + 'static,
{
    let session_id = match state.require_session(session_id) {
        Ok(session_id) => session_id,
        Err(error) => return error_response(StatusCode::NOT_FOUND, error),
    };
    let reasoning_effort = match request.reasoning_effort.as_deref() {
        Some(effort) => match state.service.allowed_reasoning_effort(effort) {
            Ok(effort) => Some(effort.to_string()),
            Err(error) => return error_response(StatusCode::BAD_REQUEST, error),
        },
        None => None,
    };
    let events = state.events.clone();
    match state
        .service
        .compact_session(
            session_id,
            request.instructions.as_deref(),
            reasoning_effort.as_deref(),
            |event| events.publish(ServerEvent::from_agent_event(event)),
        )
        .await
    {
        Ok(result) => Json(CompactionResponse::from_result(result)).into_response(),
        Err(error) if is_turn_cancelled(&error) => error_response(StatusCode::CONFLICT, error),
        Err(error) => error_response(StatusCode::BAD_REQUEST, error),
    }
}

async fn session_events<M>(
    State(state): State<ServerState<M>>,
    AxumPath(session_id): AxumPath<u64>,
) -> impl IntoResponse
where
    M: ModelStreamer + Send + Sync + 'static,
{
    let session_id = match state.require_session(session_id) {
        Ok(session_id) => session_id,
        Err(error) => return error_response(StatusCode::NOT_FOUND, error),
    };
    match state.events.subscribe(session_id) {
        Ok(receiver) => sse_from_broadcast(session_id, receiver),
        Err(error) => error_response(StatusCode::INTERNAL_SERVER_ERROR, error),
    }
}

struct PendingTextDelta {
    session_id: u64,
    delta: String,
    started_at: Instant,
}

struct TurnEventBuffer<'a> {
    tx: &'a mpsc::Sender<ServerEvent>,
    pending_delta: Option<PendingTextDelta>,
    last_flush_at: Option<Instant>,
}

impl<'a> TurnEventBuffer<'a> {
    fn new(tx: &'a mpsc::Sender<ServerEvent>) -> Self {
        Self {
            tx,
            pending_delta: None,
            last_flush_at: None,
        }
    }

    fn send(&mut self, event: ServerEvent) -> Result<()> {
        match event {
            ServerEvent::TextDelta { session_id, delta } => self.send_text_delta(session_id, delta),
            event => {
                self.flush_text_delta()?;
                send_turn_event(self.tx, event)
            }
        }
    }

    fn send_text_delta(&mut self, session_id: u64, delta: String) -> Result<()> {
        if delta.is_empty() {
            return Ok(());
        }

        if self.pending_delta.is_none()
            && self
                .last_flush_at
                .is_none_or(|last_flush| last_flush.elapsed() >= DIRECT_TURN_DELTA_FLUSH_INTERVAL)
        {
            self.last_flush_at = Some(Instant::now());
            return send_turn_event(self.tx, ServerEvent::TextDelta { session_id, delta });
        }

        match self.pending_delta.as_mut() {
            Some(pending) if pending.session_id == session_id => pending.delta.push_str(&delta),
            Some(_) => {
                self.flush_text_delta()?;
                self.pending_delta = Some(PendingTextDelta {
                    session_id,
                    delta,
                    started_at: Instant::now(),
                });
            }
            None => {
                self.pending_delta = Some(PendingTextDelta {
                    session_id,
                    delta,
                    started_at: Instant::now(),
                });
            }
        }

        if self.pending_delta.as_ref().is_some_and(|pending| {
            pending.delta.len() >= DIRECT_TURN_DELTA_FLUSH_BYTES
                || pending.started_at.elapsed() >= DIRECT_TURN_DELTA_FLUSH_INTERVAL
        }) {
            self.flush_text_delta()?;
        }
        Ok(())
    }

    fn flush_text_delta(&mut self) -> Result<()> {
        let Some(pending) = self.pending_delta.take() else {
            return Ok(());
        };
        self.last_flush_at = Some(Instant::now());
        send_turn_event(
            self.tx,
            ServerEvent::TextDelta {
                session_id: pending.session_id,
                delta: pending.delta,
            },
        )
    }
}

fn send_turn_event(tx: &mpsc::Sender<ServerEvent>, event: ServerEvent) -> Result<()> {
    match tx.try_send(event) {
        Ok(()) => Ok(()),
        Err(mpsc::error::TrySendError::Closed(_)) => Err(turn_cancelled_error()),
        Err(mpsc::error::TrySendError::Full(_)) => {
            anyhow::bail!("turn event stream backpressure")
        }
    }
}

fn sse_from_mpsc(
    receiver: mpsc::Receiver<ServerEvent>,
    cancellation: TurnCancellation,
) -> axum::response::Response {
    let guard = TurnStreamGuard { cancellation };
    let stream = stream! {
        let _guard = guard;
        let mut receiver = ReceiverStream::new(receiver);
        while let Some(event) = receiver.next().await {
            yield Ok::<Bytes, Infallible>(encode_sse_ref(&event));
        }
    };
    sse_response(Body::from_stream(stream))
}

struct TurnStreamGuard {
    cancellation: TurnCancellation,
}

impl Drop for TurnStreamGuard {
    fn drop(&mut self) {
        self.cancellation.cancel();
    }
}

fn sse_from_broadcast(
    session_id: SessionId,
    mut receiver: broadcast::Receiver<ServerEvent>,
) -> axum::response::Response {
    let stream = stream! {
        yield Ok::<Bytes, Infallible>(encode_sse_ref(&ServerEvent::ServerConnected {
            session_id: session_id.get(),
        }));

        let mut heartbeat = tokio::time::interval(HEARTBEAT_INTERVAL);
        loop {
            tokio::select! {
                _ = heartbeat.tick() => {
                    yield Ok::<Bytes, Infallible>(encode_sse_ref(&ServerEvent::ServerHeartbeat {
                        session_id: session_id.get(),
                    }));
                }
                event = receiver.recv() => {
                    match event {
                        Ok(event) => yield Ok::<Bytes, Infallible>(encode_sse_ref(&event)),
                        Err(broadcast::error::RecvError::Lagged(skipped)) => {
                            yield Ok::<Bytes, Infallible>(encode_sse_ref(&ServerEvent::EventsLagged {
                                session_id: session_id.get(),
                                skipped,
                            }));
                        }
                        Err(broadcast::error::RecvError::Closed) => return,
                    }
                }
            }
        }
    };
    sse_response(Body::from_stream(stream))
}

fn sse_response(body: Body) -> axum::response::Response {
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, SSE_CONTENT_TYPE)
        .header(header::CACHE_CONTROL, "no-cache, no-transform")
        .header("x-accel-buffering", "no")
        .header(header::X_CONTENT_TYPE_OPTIONS, "nosniff")
        .body(body)
        .expect("SSE response headers must be valid")
        .into_response()
}

fn encode_sse_ref(event: &ServerEvent) -> Bytes {
    encode_sse_bytes(event).expect("server event serialization should not fail")
}

fn encode_sse_bytes(event: &ServerEvent) -> Result<Bytes> {
    let data = serde_json::to_vec(event).context("failed to serialize server event")?;
    let mut bytes = Vec::with_capacity(event.event_name().len() + data.len() + 16);
    bytes.extend_from_slice(b"event: ");
    bytes.extend_from_slice(event.event_name().as_bytes());
    bytes.extend_from_slice(b"\ndata: ");
    bytes.extend_from_slice(&data);
    bytes.extend_from_slice(b"\n\n");
    Ok(Bytes::from(bytes))
}

#[expect(
    clippy::needless_pass_by_value,
    reason = "handlers pass owned anyhow errors into a common JSON response builder"
)]
fn error_response(status: StatusCode, error: anyhow::Error) -> axum::response::Response {
    (
        status,
        Json(ErrorResponse {
            error: error.to_string(),
        }),
    )
        .into_response()
}

pub(crate) fn zeus_api_contract_pretty() -> Result<String> {
    serde_json::to_string_pretty(&zeus_api_contract_fixture())
        .context("failed to serialize Zeus API contract")
}

fn contract_schema_hash() -> String {
    let material = contract_fixture_with_schema_hash(CONTRACT_SCHEMA_HASH_PLACEHOLDER);
    let bytes = serde_json::to_vec(&material).expect("contract fixture must serialize");
    format!("{:016x}", fnv1a64(&bytes))
}

fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    hash
}

fn capabilities_response(schema_hash: String) -> CapabilitiesResponse {
    CapabilitiesResponse {
        name: SERVER_NAME,
        protocol_version: SERVER_PROTOCOL_VERSION,
        schema_hash,
        transports: SERVER_TRANSPORTS.to_vec(),
        features: SERVER_FEATURES.to_vec(),
        route_groups: SERVER_ROUTE_GROUPS.to_vec(),
    }
}

fn zeus_api_contract_fixture() -> serde_json::Value {
    contract_fixture_with_schema_hash(&contract_schema_hash())
}

#[expect(
    clippy::too_many_lines,
    reason = "contract fixture intentionally keeps the full API sample in one generator"
)]
fn contract_fixture_with_schema_hash(schema_hash: &str) -> serde_json::Value {
    serde_json::json!({
        "version": 1,
        "requests": {
            "switch_workspace_branch": SwitchWorkspaceBranchRequest {
                branch: "feature".to_string(),
            },
            "set_session_model": SetModelRequest {
                model: "gpt-5.5".to_string(),
            },
            "set_session_permissions": SetPermissionsRequest {
                tool_policy: "workspace-write".to_string(),
            },
            "restore_session": RestoreSessionRequest {
                session_id: 42,
            },
            "list_sessions": ListSessionsQuery {
                limit: Some(50),
                offset: Some(0),
            },
            "turn": TurnRequest {
                message: "hello".to_string(),
                reasoning_effort: Some("medium".to_string()),
            },
            "terminal_command": TerminalCommandRequest {
                command: "printf ok".to_string(),
            },
            "compact_session": CompactSessionRequest {
                instructions: Some("focus on open files".to_string()),
                reasoning_effort: Some("medium".to_string()),
            },
        },
        "responses": {
            "server_ready": ServerReadyMessage {
                event: "server_ready",
                name: SERVER_NAME,
                protocol_version: SERVER_PROTOCOL_VERSION,
                http_addr: "127.0.0.1:4096".to_string(),
                h3_addr: "127.0.0.1:4433".to_string(),
                token: "contract-token".to_string(),
                workspace_root: "/workspace".to_string(),
                pid: 1234,
            },
            "root": RootResponse {
                name: SERVER_NAME,
                protocol: "http/1.1,http/2,http/3",
                workspace_root: "/workspace".to_string(),
            },
            "capabilities": capabilities_response(schema_hash.to_string()),
            "models": ModelsResponse {
                default_model: "gpt-5.5".to_string(),
                allowed_models: vec![
                    "gpt-5.5".to_string(),
                    "gpt-5.4".to_string(),
                ],
                default_reasoning_effort: "medium".to_string(),
                reasoning_efforts: vec![
                    "low".to_string(),
                    "medium".to_string(),
                    "high".to_string(),
                ],
            },
            "permissions": PermissionsResponse {
                default_tool_policy: "read-only".to_string(),
                allowed_tool_policies: vec![
                    "read-only".to_string(),
                    "workspace-write".to_string(),
                    "workspace-exec".to_string(),
                ],
            },
            "workspace": WorkspaceResponse {
                workspace_root: "/workspace".to_string(),
                branch: Some("main".to_string()),
                branches: vec!["main".to_string(), "feature".to_string()],
                git: true,
            },
            "switch_workspace_branch": SwitchWorkspaceBranchResponse {
                previous_branch: Some("main".to_string()),
                branch: "feature".to_string(),
                stashed_changes: true,
                workspace: WorkspaceResponse {
                    workspace_root: "/workspace".to_string(),
                    branch: Some("feature".to_string()),
                    branches: vec!["main".to_string(), "feature".to_string()],
                    git: true,
                },
            },
            "create_session": CreateSessionResponse {
                session_id: 42,
                model: "gpt-5.5".to_string(),
                tool_policy: "read-only".to_string(),
            },
            "restore_session": RestoreSessionResponse {
                session_id: 42,
                model: "gpt-5.5".to_string(),
                tool_policy: "workspace-write".to_string(),
                messages: vec![
                    TranscriptMessageResponse {
                        message_id: 1,
                        kind: "message",
                        role: Some("user"),
                        text: Some("read Cargo.toml".to_string()),
                        tool_call_id: None,
                        tool_name: None,
                        tool_arguments: None,
                        success: None,
                    },
                    TranscriptMessageResponse {
                        message_id: 2,
                        kind: "function_call",
                        role: None,
                        text: None,
                        tool_call_id: Some("call_read".to_string()),
                        tool_name: Some("read_file".to_string()),
                        tool_arguments: Some(r#"{"path":"Cargo.toml"}"#.to_string()),
                        success: None,
                    },
                    TranscriptMessageResponse {
                        message_id: 3,
                        kind: "function_output",
                        role: None,
                        text: Some("name = \"rust-agent\"".to_string()),
                        tool_call_id: Some("call_read".to_string()),
                        tool_name: None,
                        tool_arguments: None,
                        success: Some(true),
                    },
                    TranscriptMessageResponse {
                        message_id: 4,
                        kind: "message",
                        role: Some("assistant"),
                        text: Some("done".to_string()),
                        tool_call_id: None,
                        tool_name: None,
                        tool_arguments: None,
                        success: None,
                    },
                ],
            },
            "session_model": SessionModelResponse {
                model: "gpt-5.5".to_string(),
            },
            "session_permissions": SessionPermissionsResponse {
                tool_policy: "workspace-write".to_string(),
            },
            "cancel_turn": CancelTurnResponse {
                cancelled: true,
            },
            "terminal_command": TerminalCommandResponse {
                output: "ok\n".to_string(),
                success: true,
            },
            "compact_session": CompactionResponse {
                summary: "checkpoint".to_string(),
                first_kept_message_id: 4,
                tokens_before: 12345,
                details: CompactionDetails {
                    read_files: vec!["Cargo.toml".to_string()],
                    modified_files: vec!["src/main.rs".to_string()],
                },
            },
            "error": ErrorResponse {
                error: "session not found".to_string(),
            },
        },
        "events": {
            "server.connected": ServerEvent::ServerConnected { session_id: 42 },
            "server.heartbeat": ServerEvent::ServerHeartbeat { session_id: 42 },
            "server.events_lagged": ServerEvent::EventsLagged {
                session_id: 42,
                skipped: 3,
            },
            "session.status_changed": ServerEvent::StatusChanged {
                session_id: 42,
                status: "running",
            },
            "message.text_delta": ServerEvent::TextDelta {
                session_id: 42,
                delta: "hello".to_string(),
            },
            "message.completed": ServerEvent::MessageCompleted {
                session_id: 42,
                role: "assistant",
                text: "hello".to_string(),
            },
            "cache.health": ServerEvent::CacheHealth {
                session_id: 42,
                cache: CacheHealthEvent {
                    model: "gpt-5.5".to_string(),
                    prompt_cache_key: "contract-cache-key".to_string(),
                    stable_prefix_hash: "000000000000002a".to_string(),
                    stable_prefix_bytes: 128,
                    request_input_hash: "0000000000000040".to_string(),
                    message_count: 4,
                    input_bytes: 1024,
                    response_id: Some("resp_contract".to_string()),
                    usage: Some(TokenUsageEvent {
                        input_tokens: Some(100),
                        cached_input_tokens: Some(80),
                        output_tokens: Some(12),
                        reasoning_output_tokens: Some(4),
                        total_tokens: Some(112),
                    }),
                    cache_status: "reused_prefix",
                },
            },
            "turn.token_usage": ServerEvent::TurnTokenUsage {
                session_id: 42,
                usage: TokenUsageEvent {
                    input_tokens: Some(300),
                    cached_input_tokens: Some(200),
                    output_tokens: Some(30),
                    reasoning_output_tokens: Some(6),
                    total_tokens: Some(330),
                },
            },
            "tool_call.started": ServerEvent::ToolCallStarted {
                session_id: 42,
                tool_call_id: "call_read".to_string(),
                tool_name: "read_file".to_string(),
                args: r#"{"path":"Cargo.toml"}"#.to_string(),
            },
            "tool_call.completed": ServerEvent::ToolCallCompleted {
                session_id: 42,
                tool_call_id: "call_read".to_string(),
                tool_name: "read_file".to_string(),
                success: true,
            },
            "session.error": ServerEvent::Error {
                session_id: 42,
                message: "not logged in".to_string(),
            },
            "compaction.started": ServerEvent::CompactionStarted {
                session_id: 42,
                reason: "manual",
            },
            "compaction.completed": ServerEvent::CompactionCompleted {
                session_id: 42,
                reason: "manual",
                summary: "checkpoint".to_string(),
                first_kept_message_id: 4,
                tokens_before: 12345,
                details: CompactionDetails {
                    read_files: vec!["Cargo.toml".to_string()],
                    modified_files: vec!["src/main.rs".to_string()],
                },
            },
            "turn.completed": ServerEvent::TurnCompleted { session_id: 42 },
            "turn.cancelled": ServerEvent::TurnCancelled { session_id: 42 },
        },
    })
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ServerEvent {
    ServerConnected {
        session_id: u64,
    },
    ServerHeartbeat {
        session_id: u64,
    },
    EventsLagged {
        session_id: u64,
        skipped: u64,
    },
    StatusChanged {
        session_id: u64,
        status: &'static str,
    },
    TextDelta {
        session_id: u64,
        delta: String,
    },
    MessageCompleted {
        session_id: u64,
        role: &'static str,
        text: String,
    },
    CacheHealth {
        session_id: u64,
        cache: CacheHealthEvent,
    },
    TurnTokenUsage {
        session_id: u64,
        usage: TokenUsageEvent,
    },
    CompactionStarted {
        session_id: u64,
        reason: &'static str,
    },
    CompactionCompleted {
        session_id: u64,
        reason: &'static str,
        summary: String,
        first_kept_message_id: u64,
        tokens_before: u64,
        details: CompactionDetails,
    },
    ToolCallStarted {
        session_id: u64,
        tool_call_id: String,
        tool_name: String,
        args: String,
    },
    ToolCallCompleted {
        session_id: u64,
        tool_call_id: String,
        tool_name: String,
        success: bool,
    },
    Error {
        session_id: u64,
        message: String,
    },
    TurnCompleted {
        session_id: u64,
    },
    TurnCancelled {
        session_id: u64,
    },
}

impl ServerEvent {
    fn from_agent_event(event: AgentEvent<'_>) -> Self {
        match event {
            AgentEvent::StatusChanged { session_id, status } => Self::StatusChanged {
                session_id: session_id.get(),
                status: status.as_str(),
            },
            AgentEvent::TextDelta { session_id, delta } => Self::TextDelta {
                session_id: session_id.get(),
                delta: delta.to_string(),
            },
            AgentEvent::MessageCompleted {
                session_id,
                role,
                text,
                ..
            } => Self::MessageCompleted {
                session_id: session_id.get(),
                role: role.as_str(),
                text: text.to_string(),
            },
            AgentEvent::CacheHealth {
                session_id,
                cache_health,
            } => Self::CacheHealth {
                session_id: session_id.get(),
                cache: CacheHealthEvent::from_cache_health(cache_health),
            },
            AgentEvent::TurnTokenUsage { session_id, usage } => Self::TurnTokenUsage {
                session_id: session_id.get(),
                usage: TokenUsageEvent::from_usage(usage),
            },
            AgentEvent::CompactionStarted { session_id, reason } => Self::CompactionStarted {
                session_id: session_id.get(),
                reason: reason.as_str(),
            },
            AgentEvent::CompactionCompleted {
                session_id,
                reason,
                result,
            } => Self::CompactionCompleted {
                session_id: session_id.get(),
                reason: reason.as_str(),
                summary: result.summary.clone(),
                first_kept_message_id: result.first_kept_message_id.get(),
                tokens_before: result.tokens_before,
                details: result.details.clone(),
            },
            AgentEvent::ToolCallStarted {
                session_id,
                tool_call_id,
                tool_name,
                args,
            } => Self::ToolCallStarted {
                session_id: session_id.get(),
                tool_call_id: tool_call_id.to_string(),
                tool_name: tool_name.to_string(),
                args: args.to_string(),
            },
            AgentEvent::ToolCallCompleted {
                session_id,
                tool_call_id,
                tool_name,
                success,
            } => Self::ToolCallCompleted {
                session_id: session_id.get(),
                tool_call_id: tool_call_id.to_string(),
                tool_name: tool_name.to_string(),
                success,
            },
            AgentEvent::Error {
                session_id,
                message,
            } => Self::Error {
                session_id: session_id.get(),
                message: message.to_string(),
            },
        }
    }

    const fn session_id(&self) -> SessionId {
        match self {
            Self::ServerConnected { session_id }
            | Self::ServerHeartbeat { session_id }
            | Self::EventsLagged { session_id, .. }
            | Self::StatusChanged { session_id, .. }
            | Self::TextDelta { session_id, .. }
            | Self::MessageCompleted { session_id, .. }
            | Self::CacheHealth { session_id, .. }
            | Self::TurnTokenUsage { session_id, .. }
            | Self::CompactionStarted { session_id, .. }
            | Self::CompactionCompleted { session_id, .. }
            | Self::ToolCallStarted { session_id, .. }
            | Self::ToolCallCompleted { session_id, .. }
            | Self::Error { session_id, .. }
            | Self::TurnCompleted { session_id }
            | Self::TurnCancelled { session_id } => SessionId::new(*session_id),
        }
    }

    const fn event_name(&self) -> &'static str {
        match self {
            Self::ServerConnected { .. } => "server.connected",
            Self::ServerHeartbeat { .. } => "server.heartbeat",
            Self::EventsLagged { .. } => "server.events_lagged",
            Self::StatusChanged { .. } => "session.status_changed",
            Self::TextDelta { .. } => "message.text_delta",
            Self::MessageCompleted { .. } => "message.completed",
            Self::CacheHealth { .. } => "cache.health",
            Self::TurnTokenUsage { .. } => "turn.token_usage",
            Self::CompactionStarted { .. } => "compaction.started",
            Self::CompactionCompleted { .. } => "compaction.completed",
            Self::ToolCallStarted { .. } => "tool_call.started",
            Self::ToolCallCompleted { .. } => "tool_call.completed",
            Self::Error { .. } => "session.error",
            Self::TurnCompleted { .. } => "turn.completed",
            Self::TurnCancelled { .. } => "turn.cancelled",
        }
    }
}

#[derive(Clone, Debug, Serialize)]
struct CacheHealthEvent {
    model: String,
    prompt_cache_key: String,
    stable_prefix_hash: String,
    stable_prefix_bytes: usize,
    request_input_hash: String,
    message_count: usize,
    input_bytes: usize,
    response_id: Option<String>,
    usage: Option<TokenUsageEvent>,
    cache_status: &'static str,
}

impl CacheHealthEvent {
    fn from_cache_health(cache_health: &CacheHealth) -> Self {
        Self {
            model: cache_health.model.clone(),
            prompt_cache_key: cache_health.prompt_cache_key.clone(),
            stable_prefix_hash: format!("{:016x}", cache_health.stable_prefix_hash),
            stable_prefix_bytes: cache_health.stable_prefix_bytes,
            request_input_hash: format!("{:016x}", cache_health.request_input_hash),
            message_count: cache_health.message_count,
            input_bytes: cache_health.input_bytes,
            response_id: cache_health.response_id.clone(),
            usage: cache_health.usage.map(TokenUsageEvent::from_usage),
            cache_status: cache_health.cache_status.as_str(),
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize)]
#[expect(
    clippy::struct_field_names,
    reason = "SSE usage event fields mirror provider token counter names"
)]
struct TokenUsageEvent {
    input_tokens: Option<u64>,
    cached_input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    reasoning_output_tokens: Option<u64>,
    total_tokens: Option<u64>,
}

impl TokenUsageEvent {
    const fn from_usage(usage: TokenUsage) -> Self {
        Self {
            input_tokens: usage.input_tokens,
            cached_input_tokens: usage.cached_input_tokens,
            output_tokens: usage.output_tokens,
            reasoning_output_tokens: usage.reasoning_output_tokens,
            total_tokens: usage.total_tokens,
        }
    }
}

#[derive(Serialize)]
struct RootResponse {
    name: &'static str,
    protocol: &'static str,
    workspace_root: String,
}

#[derive(Serialize)]
struct ServerReadyMessage {
    event: &'static str,
    name: &'static str,
    protocol_version: u32,
    http_addr: String,
    h3_addr: String,
    token: String,
    workspace_root: String,
    pid: u32,
}

#[derive(Serialize)]
struct HealthResponse {
    healthy: bool,
}

#[derive(Serialize)]
struct CapabilitiesResponse {
    name: &'static str,
    protocol_version: u32,
    schema_hash: String,
    transports: Vec<&'static str>,
    features: Vec<&'static str>,
    route_groups: Vec<&'static str>,
}

#[derive(Serialize)]
struct ModelsResponse {
    default_model: String,
    allowed_models: Vec<String>,
    default_reasoning_effort: String,
    reasoning_efforts: Vec<String>,
}

#[derive(Serialize)]
struct PermissionsResponse {
    default_tool_policy: String,
    allowed_tool_policies: Vec<String>,
}

#[derive(Serialize)]
struct WorkspaceResponse {
    workspace_root: String,
    branch: Option<String>,
    branches: Vec<String>,
    git: bool,
}

impl WorkspaceResponse {
    fn from_snapshot(snapshot: crate::workspace::WorkspaceSnapshot) -> Self {
        Self {
            workspace_root: snapshot.workspace_root,
            branch: snapshot.branch,
            branches: snapshot.branches,
            git: snapshot.git,
        }
    }
}

#[derive(Serialize)]
struct SwitchWorkspaceBranchResponse {
    previous_branch: Option<String>,
    branch: String,
    stashed_changes: bool,
    workspace: WorkspaceResponse,
}

impl SwitchWorkspaceBranchResponse {
    fn from_result(result: crate::workspace::BranchSwitchResult) -> Self {
        Self {
            previous_branch: result.previous_branch,
            branch: result.branch,
            stashed_changes: result.stashed_changes,
            workspace: WorkspaceResponse::from_snapshot(result.workspace),
        }
    }
}

#[derive(Serialize)]
struct CreateSessionResponse {
    session_id: u64,
    model: String,
    tool_policy: String,
}

#[derive(Serialize)]
struct ListSessionsResponse {
    sessions: Vec<SessionMetadataResponse>,
    limit: usize,
    offset: usize,
    next_offset: Option<usize>,
}

#[derive(Serialize)]
struct SessionMetadataResponse {
    session_id: u64,
    model: String,
    status: &'static str,
    created_at_ms: i64,
    updated_at_ms: i64,
    message_count: u64,
    active: bool,
    last_message: Option<SessionLastMessageResponse>,
}

impl SessionMetadataResponse {
    fn from_metadata(metadata: SessionMetadata) -> Self {
        Self {
            session_id: metadata.session_id.get(),
            model: metadata.model,
            status: metadata.status.as_str(),
            created_at_ms: metadata.created_at_ms,
            updated_at_ms: metadata.updated_at_ms,
            message_count: metadata.message_count,
            active: metadata.active,
            last_message: metadata
                .last_message
                .map(SessionLastMessageResponse::from_last_message),
        }
    }
}

#[derive(Serialize)]
struct SessionLastMessageResponse {
    message_id: u64,
    role: &'static str,
    preview: String,
    truncated: bool,
    created_at_ms: i64,
}

impl SessionLastMessageResponse {
    fn from_last_message(message: SessionLastMessage) -> Self {
        Self {
            message_id: message.message_id.get(),
            role: message.role.as_str(),
            preview: message.preview,
            truncated: message.truncated,
            created_at_ms: message.created_at_ms,
        }
    }
}

#[derive(Serialize)]
struct RestoreSessionResponse {
    session_id: u64,
    model: String,
    tool_policy: String,
    messages: Vec<TranscriptMessageResponse>,
}

impl RestoreSessionResponse {
    fn from_snapshot(snapshot: SessionSnapshot) -> Self {
        Self {
            session_id: snapshot.session_id.get(),
            model: snapshot.model,
            tool_policy: snapshot.tool_policy.as_str().to_string(),
            messages: snapshot
                .messages
                .iter()
                .map(TranscriptMessageResponse::from_message)
                .collect(),
        }
    }
}

#[derive(Serialize)]
struct TranscriptMessageResponse {
    message_id: u64,
    kind: &'static str,
    role: Option<&'static str>,
    text: Option<String>,
    tool_call_id: Option<String>,
    tool_name: Option<String>,
    tool_arguments: Option<String>,
    success: Option<bool>,
}

impl TranscriptMessageResponse {
    fn from_message(message: &AgentMessage) -> Self {
        match message.item() {
            AgentItem::Message { role, text } => Self {
                message_id: message.id().get(),
                kind: "message",
                role: Some(role_name(*role)),
                text: Some(text.clone()),
                tool_call_id: None,
                tool_name: None,
                tool_arguments: None,
                success: None,
            },
            AgentItem::FunctionCall {
                call_id,
                name,
                arguments,
                ..
            } => Self {
                message_id: message.id().get(),
                kind: "function_call",
                role: None,
                text: None,
                tool_call_id: Some(call_id.clone()),
                tool_name: Some(name.clone()),
                tool_arguments: Some(arguments.clone()),
                success: None,
            },
            AgentItem::FunctionOutput {
                call_id,
                output,
                success,
            } => Self {
                message_id: message.id().get(),
                kind: "function_output",
                role: None,
                text: Some(output.clone()),
                tool_call_id: Some(call_id.clone()),
                tool_name: None,
                tool_arguments: None,
                success: Some(*success),
            },
            AgentItem::Compaction { summary, .. } => Self {
                message_id: message.id().get(),
                kind: "compaction",
                role: None,
                text: Some(summary.clone()),
                tool_call_id: None,
                tool_name: None,
                tool_arguments: None,
                success: None,
            },
        }
    }
}

const fn role_name(role: MessageRole) -> &'static str {
    match role {
        MessageRole::User => "user",
        MessageRole::Assistant => "assistant",
    }
}

#[derive(Serialize)]
struct SessionModelResponse {
    model: String,
}

#[derive(Serialize)]
struct SessionPermissionsResponse {
    tool_policy: String,
}

#[derive(Serialize)]
struct CancelTurnResponse {
    cancelled: bool,
}

#[derive(Serialize)]
struct TerminalCommandResponse {
    output: String,
    success: bool,
}

#[derive(Serialize)]
struct CompactionResponse {
    summary: String,
    first_kept_message_id: u64,
    tokens_before: u64,
    details: CompactionDetails,
}

impl CompactionResponse {
    fn from_result(result: CompactionResult) -> Self {
        Self {
            summary: result.summary,
            first_kept_message_id: result.first_kept_message_id.get(),
            tokens_before: result.tokens_before,
            details: result.details,
        }
    }
}

#[derive(Serialize)]
struct ErrorResponse {
    error: String,
}

#[derive(Deserialize, Serialize)]
struct SetModelRequest {
    model: String,
}

#[derive(Deserialize, Serialize)]
struct SetPermissionsRequest {
    tool_policy: String,
}

#[derive(Deserialize, Serialize)]
struct RestoreSessionRequest {
    session_id: u64,
}

#[derive(Deserialize, Serialize)]
struct ListSessionsQuery {
    limit: Option<usize>,
    offset: Option<usize>,
}

#[derive(Deserialize, Serialize)]
struct SwitchWorkspaceBranchRequest {
    branch: String,
}

#[derive(Deserialize, Serialize)]
struct TurnRequest {
    message: String,
    reasoning_effort: Option<String>,
}

#[derive(Deserialize, Serialize)]
struct TerminalCommandRequest {
    command: String,
}

#[derive(Deserialize, Serialize)]
struct CompactSessionRequest {
    instructions: Option<String>,
    reasoning_effort: Option<String>,
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::process::Command;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::SystemTime;
    use std::time::UNIX_EPOCH;

    use axum::body::to_bytes;
    use axum::http::Method;
    use bytes::Buf;
    use tokio::sync::Notify;
    use tower::ServiceExt;

    use crate::agent_loop::MessageId;
    use crate::agent_loop::ModelResponse;
    use crate::agent_loop::ModelToolCall;
    use crate::bench_support::mib_per_second;
    use crate::bench_support::usize_per_second;
    use crate::bench_support::DurationSummary;
    use crate::client::ConversationMessage;
    use crate::config::CompactionConfig;
    use crate::config::ContextWindowConfig;
    use crate::config::ModelConfig;
    use crate::storage::SessionDatabase;
    use crate::tools::ToolRegistry;
    use crate::tools::ToolSpec;

    use super::*;

    const TEST_AUTHORIZATION: &str = "Bearer test-token";

    #[tokio::test]
    async fn serves_health_and_models_over_router() {
        let service = Arc::new(AgentService::new(
            StaticStreamer::new("ok", []),
            ContextWindowConfig::default(),
            ModelConfig::new("test-default", ["test-default", "test-fast"]).unwrap(),
        ));
        let state = ServerState::new(service, 16, "127.0.0.1:4433".parse().unwrap());
        let app = router(state);

        let root = app
            .clone()
            .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(root.status(), StatusCode::OK);
        let body = to_bytes(root.into_body(), usize::MAX).await.unwrap();
        let root: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(root["name"], SERVER_NAME);
        assert_eq!(root["protocol"], "http/1.1,http/2,http/3");
        let expected_workspace = std::env::current_dir().unwrap().display().to_string();
        assert_eq!(root["workspace_root"], expected_workspace);

        let health = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/healthz")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(health.status(), StatusCode::OK);
        assert_eq!(
            health.headers().get(header::ALT_SVC).unwrap(),
            "h3=\":4433\"; ma=86400"
        );

        let capabilities = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/capabilities")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(capabilities.status(), StatusCode::OK);
        let body = to_bytes(capabilities.into_body(), usize::MAX)
            .await
            .unwrap();
        let capabilities: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            capabilities["protocol_version"],
            serde_json::json!(SERVER_PROTOCOL_VERSION)
        );
        assert!(capabilities["schema_hash"].as_str().unwrap().len() >= 16);
        assert!(capabilities["features"]
            .as_array()
            .unwrap()
            .contains(&"turn_streaming".into()));

        let models = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/models")
                    .header(header::AUTHORIZATION, TEST_AUTHORIZATION)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = to_bytes(models.into_body(), usize::MAX).await.unwrap();
        assert_eq!(
            std::str::from_utf8(&body).unwrap(),
            r#"{"default_model":"test-default","allowed_models":["test-default","test-fast"],"default_reasoning_effort":"medium","reasoning_efforts":["low","medium","high","xhigh"]}"#
        );

        let permissions = app
            .oneshot(
                Request::builder()
                    .uri("/permissions")
                    .header(header::AUTHORIZATION, TEST_AUTHORIZATION)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = to_bytes(permissions.into_body(), usize::MAX).await.unwrap();
        assert_eq!(
            std::str::from_utf8(&body).unwrap(),
            r#"{"default_tool_policy":"read-only","allowed_tool_policies":["read-only","workspace-write","workspace-exec"]}"#
        );
    }

    #[tokio::test]
    async fn serves_workspace_metadata_and_switches_branches() {
        if Command::new("git").arg("--version").output().is_err() {
            return;
        }

        let root = temp_workspace("server-workspace");
        fs::create_dir_all(&root).unwrap();
        run_git(&root, &["init"]);
        run_git(&root, &["config", "user.email", "zeus@example.invalid"]);
        run_git(&root, &["config", "user.name", "Zeus Test"]);
        fs::write(root.join("README.md"), "main\n").unwrap();
        run_git(&root, &["add", "README.md"]);
        run_git(&root, &["commit", "-m", "initial"]);
        run_git(&root, &["branch", "-M", "main"]);
        run_git(&root, &["branch", "feature"]);
        let canonical_root = root.canonicalize().unwrap();

        let tools = ToolRegistry::for_root(&root);
        let service = Arc::new(AgentService::with_tools(
            StaticStreamer::new("ok", []),
            ContextWindowConfig::default(),
            ModelConfig::new("test-default", ["test-default"]).unwrap(),
            tools,
        ));
        let state = ServerState::with_config(
            service,
            ServerStateConfig::new(
                16,
                "127.0.0.1:4433".parse().unwrap(),
                ServerAuth::for_test(),
                canonical_root.clone(),
            ),
        );
        let app = router(state);

        let metadata = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/workspace")
                    .header(header::AUTHORIZATION, TEST_AUTHORIZATION)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(metadata.status(), StatusCode::OK);
        let body = to_bytes(metadata.into_body(), usize::MAX).await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["workspace_root"], canonical_root.display().to_string());
        assert_eq!(body["branch"], "main");
        assert_eq!(body["git"], true);
        assert!(body["branches"]
            .as_array()
            .unwrap()
            .contains(&"feature".into()));

        let switched = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/workspace/branch")
                    .header(header::AUTHORIZATION, TEST_AUTHORIZATION)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"branch":"feature"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(switched.status(), StatusCode::OK);
        let body = to_bytes(switched.into_body(), usize::MAX).await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["previous_branch"], "main");
        assert_eq!(body["branch"], "feature");
        assert_eq!(body["workspace"]["branch"], "feature");
        assert_eq!(body["stashed_changes"], false);

        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn rejects_protected_routes_without_bearer_token() {
        let service = Arc::new(AgentService::new(
            StaticStreamer::new("ok", []),
            ContextWindowConfig::default(),
            ModelConfig::new("test-default", ["test-default"]).unwrap(),
        ));
        let state = ServerState::new(service, 16, "127.0.0.1:4433".parse().unwrap());
        let response = router(state)
            .oneshot(
                Request::builder()
                    .uri("/models")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(
            response.headers().get(header::WWW_AUTHENTICATE).unwrap(),
            "Bearer"
        );
    }

    #[tokio::test]
    async fn creates_random_sessions_before_turn_routes_are_available() {
        let service = Arc::new(AgentService::new(
            StaticStreamer::new("ok", []),
            ContextWindowConfig::default(),
            ModelConfig::new("test-default", ["test-default"]).unwrap(),
        ));
        let state = ServerState::new(service, 16, "127.0.0.1:4433".parse().unwrap());
        let app = router(state);
        let created = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/sessions")
                    .header(header::AUTHORIZATION, TEST_AUTHORIZATION)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(created.status(), StatusCode::OK);
        let body = to_bytes(created.into_body(), usize::MAX).await.unwrap();
        let created: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let session_id = created["session_id"].as_u64().unwrap();
        assert_ne!(session_id, 0);
        assert_eq!(created["model"], "test-default");
        assert_eq!(created["tool_policy"], "read-only");

        let model = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/sessions/{session_id}/model"))
                    .header(header::AUTHORIZATION, TEST_AUTHORIZATION)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = to_bytes(model.into_body(), usize::MAX).await.unwrap();

        assert_eq!(
            std::str::from_utf8(&body).unwrap(),
            r#"{"model":"test-default"}"#
        );

        let permissions = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/sessions/{session_id}/permissions"))
                    .header(header::AUTHORIZATION, TEST_AUTHORIZATION)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = to_bytes(permissions.into_body(), usize::MAX).await.unwrap();
        assert_eq!(
            std::str::from_utf8(&body).unwrap(),
            r#"{"tool_policy":"read-only"}"#
        );

        let permissions = app
            .oneshot(
                Request::builder()
                    .method(Method::PUT)
                    .uri(format!("/sessions/{session_id}/permissions"))
                    .header(header::AUTHORIZATION, TEST_AUTHORIZATION)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"tool_policy":"edit"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = to_bytes(permissions.into_body(), usize::MAX).await.unwrap();
        assert_eq!(
            std::str::from_utf8(&body).unwrap(),
            r#"{"tool_policy":"workspace-write"}"#
        );
    }

    #[test]
    fn generated_session_ids_are_json_safe() {
        for _ in 0..128 {
            let session_id = random_session_id().unwrap().get();
            assert!((1..=MAX_JSON_SAFE_INTEGER).contains(&session_id));
        }
    }

    #[tokio::test]
    async fn rejects_unknown_numeric_sessions() {
        let service = Arc::new(AgentService::new(
            StaticStreamer::new("ok", []),
            ContextWindowConfig::default(),
            ModelConfig::new("test-default", ["test-default"]).unwrap(),
        ));
        let state = ServerState::new(service, 16, "127.0.0.1:4433".parse().unwrap());
        let response = router(state)
            .oneshot(
                Request::builder()
                    .uri("/sessions/7/model")
                    .header(header::AUTHORIZATION, TEST_AUTHORIZATION)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();

        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(
            std::str::from_utf8(&body).unwrap(),
            r#"{"error":"session not found"}"#
        );
    }

    #[tokio::test]
    async fn returns_too_many_requests_when_session_limit_is_reached() {
        let service = Arc::new(
            AgentService::new(
                StaticStreamer::new("ok", []),
                ContextWindowConfig::default(),
                ModelConfig::new("test-default", ["test-default"]).unwrap(),
            )
            .with_session_limit(1),
        );
        let state = ServerState::with_config(
            service,
            ServerStateConfig::new(
                16,
                "127.0.0.1:4433".parse().unwrap(),
                ServerAuth::for_test(),
                PathBuf::from("/workspace"),
            )
            .max_sessions(1),
        );
        let app = router(state);

        for expected in [StatusCode::OK, StatusCode::TOO_MANY_REQUESTS] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method(Method::POST)
                        .uri("/sessions")
                        .header(header::AUTHORIZATION, TEST_AUTHORIZATION)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), expected);
        }
    }

    #[tokio::test]
    async fn restores_durable_session_by_id() {
        let database = SessionDatabase::in_memory().unwrap();
        let session_id = SessionId::new(77);
        database.ensure_session(session_id, "test-fast").unwrap();
        database
            .insert_message(
                session_id,
                &AgentMessage::from_parts(
                    MessageId::new(1),
                    AgentItem::Message {
                        role: MessageRole::User,
                        text: "hello".to_string(),
                    },
                ),
            )
            .unwrap();
        database
            .insert_message(
                session_id,
                &AgentMessage::from_parts(
                    MessageId::new(2),
                    AgentItem::FunctionCall {
                        item_id: Some("item_read".to_string()),
                        call_id: "call_read".to_string(),
                        name: "read_file".to_string(),
                        arguments: r#"{"path":"Cargo.toml"}"#.to_string(),
                    },
                ),
            )
            .unwrap();
        database
            .insert_message(
                session_id,
                &AgentMessage::from_parts(
                    MessageId::new(3),
                    AgentItem::FunctionOutput {
                        call_id: "call_read".to_string(),
                        output: "file contents".to_string(),
                        success: true,
                    },
                ),
            )
            .unwrap();
        database
            .insert_message(
                session_id,
                &AgentMessage::from_parts(
                    MessageId::new(4),
                    AgentItem::Message {
                        role: MessageRole::Assistant,
                        text: "hi".to_string(),
                    },
                ),
            )
            .unwrap();

        let service = Arc::new(
            AgentService::new(
                StaticStreamer::new("ok", []),
                ContextWindowConfig::default(),
                ModelConfig::new("test-default", ["test-default", "test-fast"]).unwrap(),
            )
            .with_database(database)
            .with_session_limit(1),
        );
        let state = ServerState::new(service, 16, "127.0.0.1:4433".parse().unwrap());
        let app = router(state);

        let restored = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/sessions:restore")
                    .header(header::AUTHORIZATION, TEST_AUTHORIZATION)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"session_id":77}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(restored.status(), StatusCode::OK);
        let body = to_bytes(restored.into_body(), usize::MAX).await.unwrap();
        let body = std::str::from_utf8(&body).unwrap();
        assert!(body.contains(r#""session_id":77"#));
        assert!(body.contains(r#""model":"test-fast""#));
        assert!(body.contains(r#""role":"user","text":"hello""#));
        assert!(body.contains(r#""kind":"function_call""#));
        assert!(body.contains(r#""tool_call_id":"call_read""#));
        assert!(body.contains(r#""success":true"#));

        let model = app
            .oneshot(
                Request::builder()
                    .uri("/sessions/77/model")
                    .header(header::AUTHORIZATION, TEST_AUTHORIZATION)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(model.status(), StatusCode::OK);
    }

    #[tokio::test]
    #[expect(
        clippy::too_many_lines,
        reason = "integration test exercises list and detail session metadata together"
    )]
    async fn lists_session_metadata_for_frontend() {
        let database = SessionDatabase::in_memory().unwrap();
        let older_session_id = SessionId::new(76);
        database
            .ensure_session(older_session_id, "test-default")
            .unwrap();
        database
            .insert_message(
                older_session_id,
                &AgentMessage::from_parts(
                    MessageId::new(1),
                    AgentItem::Message {
                        role: MessageRole::User,
                        text: "older prompt".to_string(),
                    },
                ),
            )
            .unwrap();

        let session_id = SessionId::new(77);
        database.ensure_session(session_id, "test-fast").unwrap();
        database
            .insert_message(
                session_id,
                &AgentMessage::from_parts(
                    MessageId::new(1),
                    AgentItem::Message {
                        role: MessageRole::User,
                        text: "latest prompt".to_string(),
                    },
                ),
            )
            .unwrap();
        database
            .insert_message(
                session_id,
                &AgentMessage::from_parts(
                    MessageId::new(2),
                    AgentItem::Message {
                        role: MessageRole::Assistant,
                        text: "latest assistant reply".to_string(),
                    },
                ),
            )
            .unwrap();

        let service = Arc::new(
            AgentService::new(
                StaticStreamer::new("ok", []),
                ContextWindowConfig::default(),
                ModelConfig::new("test-default", ["test-default", "test-fast"]).unwrap(),
            )
            .with_database(database),
        );
        let state = ServerState::new(service, 16, "127.0.0.1:4433".parse().unwrap());
        let app = router(state);

        let listed = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/sessions?limit=1&offset=0")
                    .header(header::AUTHORIZATION, TEST_AUTHORIZATION)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(listed.status(), StatusCode::OK);
        let body = to_bytes(listed.into_body(), usize::MAX).await.unwrap();
        let body = std::str::from_utf8(&body).unwrap();
        assert!(body.contains(r#""limit":1"#), "{body}");
        assert!(body.contains(r#""offset":0"#), "{body}");
        assert!(body.contains(r#""next_offset":1"#), "{body}");
        assert!(body.contains(r#""session_id":77"#), "{body}");
        assert!(body.contains(r#""model":"test-fast""#), "{body}");
        assert!(body.contains(r#""status":"idle""#), "{body}");
        assert!(body.contains(r#""message_count":2"#), "{body}");
        assert!(body.contains(r#""active":false"#), "{body}");
        assert!(body.contains(r#""role":"assistant""#), "{body}");
        assert!(
            body.contains(r#""preview":"latest assistant reply""#),
            "{body}"
        );
        assert!(!body.contains(r#""session_id":76"#), "{body}");

        let detail = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/sessions/77")
                    .header(header::AUTHORIZATION, TEST_AUTHORIZATION)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(detail.status(), StatusCode::OK);
        let body = to_bytes(detail.into_body(), usize::MAX).await.unwrap();
        let body = std::str::from_utf8(&body).unwrap();
        assert!(body.contains(r#""session_id":77"#), "{body}");
        assert!(body.contains(r#""active":false"#), "{body}");

        let restored = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/sessions:restore")
                    .header(header::AUTHORIZATION, TEST_AUTHORIZATION)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"session_id":77}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(restored.status(), StatusCode::OK);

        let active_detail = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/sessions/77")
                    .header(header::AUTHORIZATION, TEST_AUTHORIZATION)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(active_detail.status(), StatusCode::OK);
        let body = to_bytes(active_detail.into_body(), usize::MAX)
            .await
            .unwrap();
        let body = std::str::from_utf8(&body).unwrap();
        assert!(body.contains(r#""active":true"#), "{body}");

        let deleted_inactive = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::DELETE)
                    .uri("/sessions/76")
                    .header(header::AUTHORIZATION, TEST_AUTHORIZATION)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(deleted_inactive.status(), StatusCode::NO_CONTENT);

        let deleted_detail = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/sessions/76")
                    .header(header::AUTHORIZATION, TEST_AUTHORIZATION)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(deleted_detail.status(), StatusCode::NOT_FOUND);

        let invalid = app
            .oneshot(
                Request::builder()
                    .uri("/sessions?limit=0")
                    .header(header::AUTHORIZATION, TEST_AUTHORIZATION)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(invalid.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn deletes_sessions_and_frees_capacity() {
        let service = Arc::new(
            AgentService::new(
                StaticStreamer::new("ok", []),
                ContextWindowConfig::default(),
                ModelConfig::new("test-default", ["test-default"]).unwrap(),
            )
            .with_session_limit(1),
        );
        let state = ServerState::with_config(
            service,
            ServerStateConfig::new(
                16,
                "127.0.0.1:4433".parse().unwrap(),
                ServerAuth::for_test(),
                PathBuf::from("/workspace"),
            )
            .max_sessions(1),
        );
        let app = router(state);
        let created = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/sessions")
                    .header(header::AUTHORIZATION, TEST_AUTHORIZATION)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = to_bytes(created.into_body(), usize::MAX).await.unwrap();
        let created: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let session_id = created["session_id"].as_u64().unwrap();

        let deleted = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::DELETE)
                    .uri(format!("/sessions/{session_id}"))
                    .header(header::AUTHORIZATION, TEST_AUTHORIZATION)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(deleted.status(), StatusCode::NO_CONTENT);

        let model = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/sessions/{session_id}/model"))
                    .header(header::AUTHORIZATION, TEST_AUTHORIZATION)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(model.status(), StatusCode::NOT_FOUND);

        let recreated = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/sessions")
                    .header(header::AUTHORIZATION, TEST_AUTHORIZATION)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(recreated.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn returns_error_when_event_channel_limit_is_reached() {
        let service = Arc::new(AgentService::new(
            StaticStreamer::new("ok", []),
            ContextWindowConfig::default(),
            ModelConfig::new("test-default", ["test-default"]).unwrap(),
        ));
        let state = ServerState::with_config(
            service,
            ServerStateConfig::new(
                16,
                "127.0.0.1:4433".parse().unwrap(),
                ServerAuth::for_test(),
                PathBuf::from("/workspace"),
            )
            .max_event_channels(1),
        );
        state.register_session_for_test(SessionId::new(1));
        state.register_session_for_test(SessionId::new(2));
        let app = router(state);

        let first = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/sessions/1/events")
                    .header(header::AUTHORIZATION, TEST_AUTHORIZATION)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(first.status(), StatusCode::OK);

        let second = app
            .oneshot(
                Request::builder()
                    .uri("/sessions/2/events")
                    .header(header::AUTHORIZATION, TEST_AUTHORIZATION)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(second.status(), StatusCode::INTERNAL_SERVER_ERROR);
        drop(first);
    }

    #[tokio::test]
    async fn streams_turn_events_as_sse() {
        let service = Arc::new(AgentService::new(
            StaticStreamer::new("hello", ["hel", "lo"]),
            ContextWindowConfig::default(),
            ModelConfig::new("test-default", ["test-default"]).unwrap(),
        ));
        let state = ServerState::new(service, 16, "127.0.0.1:4433".parse().unwrap());
        state.register_session_for_test(SessionId::new(7));
        let app = router(state);
        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/sessions/7/turns:stream")
                    .header(header::AUTHORIZATION, TEST_AUTHORIZATION)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"message":"hello"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(header::CONTENT_TYPE).unwrap(),
            SSE_CONTENT_TYPE
        );
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body = std::str::from_utf8(&body).unwrap();
        assert!(body.contains("event: message.text_delta\ndata: {\"type\":\"text_delta\",\"session_id\":7,\"delta\":\"hel\"}\n\n"));
        assert!(body.contains(
            "event: turn.completed\ndata: {\"type\":\"turn_completed\",\"session_id\":7}\n\n"
        ));
    }

    #[tokio::test]
    async fn turn_stream_does_not_broadcast_duplicate_session_events() {
        let service = Arc::new(AgentService::new(
            StaticStreamer::new("hello", ["hel", "lo"]),
            ContextWindowConfig::default(),
            ModelConfig::new("test-default", ["test-default"]).unwrap(),
        ));
        let state = ServerState::new(service, 16, "127.0.0.1:4433".parse().unwrap());
        state.register_session_for_test(SessionId::new(7));
        let mut receiver = state.events.subscribe(SessionId::new(7)).unwrap();
        let app = router(state.clone());
        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/sessions/7/turns:stream")
                    .header(header::AUTHORIZATION, TEST_AUTHORIZATION)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"message":"hello"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        assert!(std::str::from_utf8(&body)
            .unwrap()
            .contains("event: turn.completed\n"));
        assert!(matches!(
            receiver.try_recv(),
            Err(broadcast::error::TryRecvError::Empty)
        ));
    }

    #[tokio::test]
    async fn direct_turn_stream_batches_text_deltas() {
        const DELTAS: usize = 32;

        let service = Arc::new(AgentService::new(
            ManyDeltaStreamer { deltas: DELTAS },
            ContextWindowConfig::default(),
            ModelConfig::new("test-default", ["test-default"]).unwrap(),
        ));
        let state = ServerState::new(service, 1, "127.0.0.1:4433".parse().unwrap());
        state.register_session_for_test(SessionId::new(7));
        let app = router(state);
        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/sessions/7/turns:stream")
                    .header(header::AUTHORIZATION, TEST_AUTHORIZATION)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"message":"hello"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body = std::str::from_utf8(&body).unwrap();

        let text_delta_count = body.matches("event: message.text_delta\n").count();
        assert!(text_delta_count > 0, "{body}");
        assert!(text_delta_count < DELTAS, "{body}");
        assert!(body.contains(&format!(r#""text":"{}""#, "x".repeat(DELTAS))));
        assert!(body.contains(
            "event: turn.completed\ndata: {\"type\":\"turn_completed\",\"session_id\":7}\n\n"
        ));
    }

    #[tokio::test]
    async fn failed_turn_streams_one_error_event() {
        let service = Arc::new(AgentService::new(
            FailingStreamer,
            ContextWindowConfig::default(),
            ModelConfig::new("test-default", ["test-default"]).unwrap(),
        ));
        let state = ServerState::new(service, 16, "127.0.0.1:4433".parse().unwrap());
        state.register_session_for_test(SessionId::new(7));
        let app = router(state);
        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/sessions/7/turns:stream")
                    .header(header::AUTHORIZATION, TEST_AUTHORIZATION)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"message":"hello"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body = std::str::from_utf8(&body).unwrap();
        assert_eq!(body.matches("event: session.error\n").count(), 1);
        assert!(body.contains(
            "event: session.error\ndata: {\"type\":\"error\",\"session_id\":7,\"message\":\"model failed\"}\n\n"
        ));
        assert!(!body.contains("event: turn.completed\n"));
    }

    #[tokio::test]
    async fn cancels_running_turn_stream() {
        let started = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let service = Arc::new(AgentService::new(
            CancellableStreamer {
                started: Arc::clone(&started),
                release: Arc::clone(&release),
            },
            ContextWindowConfig::default(),
            ModelConfig::new("test-default", ["test-default"]).unwrap(),
        ));
        let state = ServerState::new(service, 16, "127.0.0.1:4433".parse().unwrap());
        state.register_session_for_test(SessionId::new(7));
        let app = router(state);
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/sessions/7/turns:stream")
                    .header(header::AUTHORIZATION, TEST_AUTHORIZATION)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"message":"hello"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        started.notified().await;
        let cancelled = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/sessions/7/turns:cancel")
                    .header(header::AUTHORIZATION, TEST_AUTHORIZATION)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(cancelled.status(), StatusCode::OK);
        let cancel_body = to_bytes(cancelled.into_body(), usize::MAX).await.unwrap();
        assert_eq!(
            std::str::from_utf8(&cancel_body).unwrap(),
            r#"{"cancelled":true}"#
        );

        let body = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            to_bytes(response.into_body(), usize::MAX),
        )
        .await
        .expect("cancelled turn stream should finish")
        .unwrap();
        let body = std::str::from_utf8(&body).unwrap();
        assert!(body.contains(
            "event: turn.cancelled\ndata: {\"type\":\"turn_cancelled\",\"session_id\":7}\n\n"
        ));
        assert!(body.contains(
            "event: session.status_changed\ndata: {\"type\":\"status_changed\",\"session_id\":7,\"status\":\"idle\"}\n\n"
        ));
        assert!(!body.contains("event: session.error\n"));
        assert!(!body.contains("event: turn.completed\n"));
        release.notify_waiters();
    }

    #[tokio::test]
    async fn dropping_direct_turn_stream_cancels_running_turn() {
        let started = Arc::new(Notify::new());
        let turn = Arc::new(AtomicUsize::new(0));
        let service = Arc::new(AgentService::new(
            DropCancelStreamer {
                started: Arc::clone(&started),
                turn: Arc::clone(&turn),
            },
            ContextWindowConfig::default(),
            ModelConfig::new("test-default", ["test-default"]).unwrap(),
        ));
        let state = ServerState::new(service, 16, "127.0.0.1:4433".parse().unwrap());
        state.register_session_for_test(SessionId::new(7));
        let app = router(state);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/sessions/7/turns:stream")
                    .header(header::AUTHORIZATION, TEST_AUTHORIZATION)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"message":"block"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        started.notified().await;
        drop(response);

        let second_response = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/sessions/7/turns:stream")
                    .header(header::AUTHORIZATION, TEST_AUTHORIZATION)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"message":"after drop"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(second_response.status(), StatusCode::OK);

        let body = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            to_bytes(second_response.into_body(), usize::MAX),
        )
        .await
        .expect("dropped turn stream should release the session for another turn")
        .unwrap();
        let body = std::str::from_utf8(&body).unwrap();
        assert!(body.contains(
            "event: message.completed\ndata: {\"type\":\"message_completed\",\"session_id\":7,\"role\":\"assistant\",\"text\":\"after drop\"}\n\n"
        ));
        assert!(body.contains(
            "event: turn.completed\ndata: {\"type\":\"turn_completed\",\"session_id\":7}\n\n"
        ));
        assert_eq!(turn.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn dropping_old_direct_turn_stream_does_not_cancel_new_turn() {
        let second_started = Arc::new(Notify::new());
        let release_second = Arc::new(Notify::new());
        let turn = Arc::new(AtomicUsize::new(0));
        let service = Arc::new(AgentService::new(
            StaleDropStreamer {
                second_started: Arc::clone(&second_started),
                release_second: Arc::clone(&release_second),
                turn: Arc::clone(&turn),
            },
            ContextWindowConfig::default(),
            ModelConfig::new("test-default", ["test-default"]).unwrap(),
        ));
        let state = ServerState::new(service, 16, "127.0.0.1:4433".parse().unwrap());
        state.register_session_for_test(SessionId::new(7));
        let app = router(state);

        let first_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/sessions/7/turns:stream")
                    .header(header::AUTHORIZATION, TEST_AUTHORIZATION)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"message":"first"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(first_response.status(), StatusCode::OK);

        let second_response = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/sessions/7/turns:stream")
                    .header(header::AUTHORIZATION, TEST_AUTHORIZATION)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"message":"second"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(second_response.status(), StatusCode::OK);

        second_started.notified().await;
        drop(first_response);
        release_second.notify_waiters();

        let body = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            to_bytes(second_response.into_body(), usize::MAX),
        )
        .await
        .expect("dropping an old stream should not cancel a newer turn")
        .unwrap();
        let body = std::str::from_utf8(&body).unwrap();
        assert!(body.contains(
            "event: message.completed\ndata: {\"type\":\"message_completed\",\"session_id\":7,\"role\":\"assistant\",\"text\":\"second\"}\n\n"
        ));
        assert!(body.contains(
            "event: turn.completed\ndata: {\"type\":\"turn_completed\",\"session_id\":7}\n\n"
        ));
        assert!(!body.contains("event: turn.cancelled\n"));
        assert_eq!(turn.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn cancel_turn_reports_false_when_idle() {
        let service = Arc::new(AgentService::new(
            StaticStreamer::new("ok", []),
            ContextWindowConfig::default(),
            ModelConfig::new("test-default", ["test-default"]).unwrap(),
        ));
        let state = ServerState::new(service, 16, "127.0.0.1:4433".parse().unwrap());
        state.register_session_for_test(SessionId::new(7));
        let response = router(state)
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/sessions/7/turns:cancel")
                    .header(header::AUTHORIZATION, TEST_AUTHORIZATION)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        assert_eq!(
            std::str::from_utf8(&body).unwrap(),
            r#"{"cancelled":false}"#
        );
    }

    #[tokio::test]
    async fn runs_terminal_commands_through_session_route() {
        let _shell_guard = crate::tools::SHELL_TEST_LOCK.lock().await;
        let service = Arc::new(AgentService::new(
            StaticStreamer::new("ok", []),
            ContextWindowConfig::default(),
            ModelConfig::new("test-default", ["test-default"]).unwrap(),
        ));
        let state = ServerState::new(service, 16, "127.0.0.1:4433".parse().unwrap());
        state.register_session_for_test(SessionId::new(7));
        let app = router(state);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/sessions/7/terminal:run")
                    .header(header::AUTHORIZATION, TEST_AUTHORIZATION)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"command":"printf denied"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        assert_eq!(
            std::str::from_utf8(&body).unwrap(),
            r#"{"error":"terminal commands require workspace-exec tool policy"}"#
        );

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::PUT)
                    .uri("/sessions/7/permissions")
                    .header(header::AUTHORIZATION, TEST_AUTHORIZATION)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"tool_policy":"workspace-exec"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/sessions/7/terminal:run")
                    .header(header::AUTHORIZATION, TEST_AUTHORIZATION)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"command":"printf server-terminal"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["success"], true);
        assert!(body["output"].as_str().unwrap().contains("server-terminal"));

        let permissions = app
            .oneshot(
                Request::builder()
                    .uri("/sessions/7/permissions")
                    .header(header::AUTHORIZATION, TEST_AUTHORIZATION)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = to_bytes(permissions.into_body(), usize::MAX).await.unwrap();
        assert_eq!(
            std::str::from_utf8(&body).unwrap(),
            r#"{"tool_policy":"workspace-exec"}"#
        );
    }

    #[tokio::test]
    async fn compacts_session_through_route() {
        let service = Arc::new(
            AgentService::new(
                StaticStreamer::new("checkpoint", []),
                ContextWindowConfig::default(),
                ModelConfig::new("test-default", ["test-default"]).unwrap(),
            )
            .with_compaction(CompactionConfig::for_test(10_000, 0, 5)),
        );
        let state = ServerState::new(service, 16, "127.0.0.1:4433".parse().unwrap());
        state.register_session_for_test(SessionId::new(7));
        let mut receiver = state.events.subscribe(SessionId::new(7)).unwrap();
        let app = router(state);

        for message in ["first", "second"] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method(Method::POST)
                        .uri("/sessions/7/turns:stream")
                        .header(header::AUTHORIZATION, TEST_AUTHORIZATION)
                        .header(header::CONTENT_TYPE, "application/json")
                        .body(Body::from(format!(r#"{{"message":"{message}"}}"#)))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let _body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        }

        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/sessions/7/compact")
                    .header(header::AUTHORIZATION, TEST_AUTHORIZATION)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        r#"{"instructions":"focus paths","reasoning_effort":"medium"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["summary"], "checkpoint");
        assert_eq!(body["first_kept_message_id"], 3);
        let events = (0..4)
            .map(|_| receiver.try_recv().unwrap())
            .collect::<Vec<_>>();
        assert!(events.iter().any(|event| matches!(
            event,
            ServerEvent::CompactionStarted {
                reason: "manual",
                ..
            }
        )));
        assert!(events.iter().any(|event| matches!(
            event,
            ServerEvent::CompactionCompleted {
                first_kept_message_id: 3,
                ..
            }
        )));
    }

    #[tokio::test]
    async fn streams_tool_call_args_in_started_event() {
        let turn = Arc::new(AtomicUsize::new(0));
        let service = Arc::new(AgentService::new(
            ToolThenContinueStreamer {
                turn: Arc::clone(&turn),
            },
            ContextWindowConfig::default(),
            ModelConfig::new("test-default", ["test-default"]).unwrap(),
        ));
        let state = ServerState::new(service, 16, "127.0.0.1:4433".parse().unwrap());
        state.register_session_for_test(SessionId::new(7));
        let response = router(state)
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/sessions/7/turns:stream")
                    .header(header::AUTHORIZATION, TEST_AUTHORIZATION)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"message":"read the manifest"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body = std::str::from_utf8(&body).unwrap();

        assert!(body.contains(
            "event: tool_call.started\ndata: {\"type\":\"tool_call_started\",\"session_id\":7,\"tool_call_id\":\"call_read\",\"tool_name\":\"read_file\",\"args\":\"{\\\"path\\\":\\\"Cargo.toml\\\"}\"}\n\n"
        ));
        assert_eq!(turn.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn stream_queue_backpressure_does_not_fail_turn() {
        let turn = Arc::new(AtomicUsize::new(0));
        let service = Arc::new(AgentService::new(
            ToolThenContinueStreamer {
                turn: Arc::clone(&turn),
            },
            ContextWindowConfig::default(),
            ModelConfig::new("test-default", ["test-default"]).unwrap(),
        ));
        let state = ServerState::new(service, 5, "127.0.0.1:4433".parse().unwrap());
        state.register_session_for_test(SessionId::new(7));
        let app = router(state);

        let first_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/sessions/7/turns:stream")
                    .header(header::AUTHORIZATION, TEST_AUTHORIZATION)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"message":"read the manifest"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(first_response.status(), StatusCode::OK);

        let second_response = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/sessions/7/turns:stream")
                    .header(header::AUTHORIZATION, TEST_AUTHORIZATION)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"message":"continue"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(second_response.status(), StatusCode::OK);

        let body = to_bytes(second_response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body = std::str::from_utf8(&body).unwrap();
        assert!(body.contains(
            "event: message.completed\ndata: {\"type\":\"message_completed\",\"session_id\":7,\"role\":\"assistant\",\"text\":\"ok\"}\n\n"
        ));
        assert!(body.contains(
            "event: turn.completed\ndata: {\"type\":\"turn_completed\",\"session_id\":7}\n\n"
        ));
        assert_eq!(turn.load(Ordering::SeqCst), 3);
        drop(first_response);
    }

    #[test]
    fn encodes_named_sse_events() {
        let event = ServerEvent::ServerConnected { session_id: 42 };
        let bytes = encode_sse_bytes(&event).unwrap();
        assert_eq!(
            std::str::from_utf8(&bytes).unwrap(),
            "event: server.connected\ndata: {\"type\":\"server_connected\",\"session_id\":42}\n\n"
        );
    }

    #[test]
    fn zeus_api_contract_fixture_matches_server_types() {
        let expected: serde_json::Value =
            serde_json::from_str(include_str!("../docs/contracts/zeus-api-contract.json")).unwrap();
        assert_eq!(super::zeus_api_contract_fixture(), expected);
    }

    #[test]
    #[ignore = "release-mode SSE event encoding benchmark; run explicitly with --ignored --nocapture"]
    fn benchmark_sse_event_encoding() {
        const EVENTS: usize = 100_000;
        const SAMPLES: usize = 15;

        let events = sample_server_events(EVENTS);
        let mut samples = Vec::with_capacity(SAMPLES);
        let mut encoded_bytes = 0usize;

        for _ in 0..SAMPLES {
            let started = Instant::now();
            let mut bytes = 0usize;
            for event in &events {
                let encoded = encode_sse_bytes(event).expect("event should encode");
                bytes = bytes.saturating_add(encoded.len());
                std::hint::black_box(&encoded);
            }
            let elapsed = started.elapsed();

            assert!(bytes > EVENTS * 32);
            encoded_bytes = bytes;
            samples.push(elapsed);
        }

        let summary = DurationSummary::from_samples(&mut samples);
        let events_per_s = usize_per_second(EVENTS, summary.median);
        let throughput_mib_s = mib_per_second(encoded_bytes, summary.median);
        println!(
            "sse_event_encoding events={EVENTS} bytes={encoded_bytes} samples={SAMPLES} min_ms={:.3} median_ms={:.3} max_ms={:.3} events_per_s={:.0} throughput_mib_s={:.1}",
            summary.min_ms(),
            summary.median_ms(),
            summary.max_ms(),
            events_per_s,
            throughput_mib_s,
        );
    }

    #[tokio::test]
    async fn binds_h3_endpoint_with_self_signed_tls() {
        let config = ServerConfig::new(
            "127.0.0.1:0".parse().unwrap(),
            "127.0.0.1:0".parse().unwrap(),
        );
        let endpoint = h3_endpoint(&config).unwrap();
        let local_addr = endpoint.local_addr().unwrap();

        assert_eq!(local_addr.ip().to_string(), "127.0.0.1");
        assert_ne!(local_addr.port(), 0);
        endpoint.close(0u32.into(), b"test complete");
    }

    #[tokio::test]
    async fn advertises_bound_h3_port_for_port_zero() {
        let service = Arc::new(AgentService::new(
            StaticStreamer::new("ok", []),
            ContextWindowConfig::default(),
            ModelConfig::new("test-default", ["test-default"]).unwrap(),
        ));
        let config = ServerConfig::new(
            "127.0.0.1:0".parse().unwrap(),
            "127.0.0.1:0".parse().unwrap(),
        );
        let endpoint = h3_endpoint(&config).unwrap();
        let h3_addr = endpoint.local_addr().unwrap();
        let state = ServerState::new(service, 16, h3_addr);
        let response = router(state)
            .oneshot(
                Request::builder()
                    .uri("/healthz")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_ne!(h3_addr.port(), 0);
        assert_eq!(
            response.headers().get(header::ALT_SVC).unwrap(),
            &format!("h3=\":{}\"; ma=86400", h3_addr.port())
        );
        endpoint.close(0u32.into(), b"test complete");
    }

    #[tokio::test]
    async fn readiness_message_reports_bound_addresses_and_token() {
        let http_listener = bind_http_listener("127.0.0.1:0".parse().unwrap())
            .await
            .unwrap();
        let http_addr = http_listener.local_addr().unwrap();
        let config = ServerConfig::new(
            "127.0.0.1:0".parse().unwrap(),
            "127.0.0.1:0".parse().unwrap(),
        );
        let h3_endpoint = h3_endpoint(&config).unwrap();
        let h3_addr = h3_endpoint.local_addr().unwrap();
        let auth = ServerAuth::from_config(Some("ready-token")).unwrap();
        let ready = server_ready_message(http_addr, h3_addr, &auth, Path::new("/workspace"));
        let ready = serde_json::to_value(ready).unwrap();

        assert_eq!(ready["event"], "server_ready");
        assert_eq!(ready["name"], SERVER_NAME);
        assert_eq!(
            ready["protocol_version"].as_u64(),
            Some(u64::from(SERVER_PROTOCOL_VERSION))
        );
        assert_eq!(ready["http_addr"], http_addr.to_string());
        assert_eq!(ready["h3_addr"], h3_addr.to_string());
        assert_eq!(ready["token"], "ready-token");
        assert_eq!(ready["workspace_root"], "/workspace");
        assert_eq!(ready["pid"].as_u64(), Some(u64::from(std::process::id())));

        h3_endpoint.close(0u32.into(), b"test complete");
        drop(http_listener);
    }

    #[tokio::test]
    async fn serves_health_over_http3() {
        let service = Arc::new(AgentService::new(
            StaticStreamer::new("ok", []),
            ContextWindowConfig::default(),
            ModelConfig::new("test-default", ["test-default"]).unwrap(),
        ));
        let state = ServerState::new(service, 16, "127.0.0.1:0".parse().unwrap());
        let app = router(state);
        let config = ServerConfig::new(
            "127.0.0.1:0".parse().unwrap(),
            "127.0.0.1:0".parse().unwrap(),
        );
        let endpoint = h3_endpoint(&config).unwrap();
        let local_addr = endpoint.local_addr().unwrap();
        let server_task = tokio::spawn(async move {
            let incoming = endpoint.accept().await.expect("test H3 connection");
            handle_h3_connection(incoming, app).await
        });

        let mut client_endpoint = quinn::Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
        client_endpoint.set_default_client_config(insecure_h3_client_config().unwrap());
        let connection = client_endpoint
            .connect(local_addr, "localhost")
            .unwrap()
            .await
            .unwrap();
        let (mut driver, mut send_request) = h3::client::builder()
            .build::<_, _, Bytes>(h3_quinn::Connection::new(connection))
            .await
            .unwrap();
        let driver_task = tokio::spawn(async move {
            let _ = driver.wait_idle().await;
        });

        let body = tokio::time::timeout(Duration::from_secs(2), async {
            let mut stream = send_request
                .send_request(
                    Request::builder()
                        .uri("https://localhost/healthz")
                        .body(())
                        .unwrap(),
                )
                .await
                .unwrap();
            stream.finish().await.unwrap();

            let response = stream.recv_response().await.unwrap();
            assert_eq!(response.status(), StatusCode::OK);

            let mut body = Vec::new();
            while let Some(mut chunk) = stream.recv_data().await.unwrap() {
                while chunk.has_remaining() {
                    let bytes = chunk.chunk();
                    body.extend_from_slice(bytes);
                    chunk.advance(bytes.len());
                }
            }
            body
        })
        .await
        .expect("H3 health request timed out");

        assert_eq!(std::str::from_utf8(&body).unwrap(), r#"{"healthy":true}"#);
        client_endpoint.close(0u32.into(), b"test complete");
        server_task.abort();
        driver_task.abort();
    }

    fn insecure_h3_client_config() -> Result<quinn::ClientConfig> {
        let mut tls_config = rustls::ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(SkipServerVerification::new())
            .with_no_client_auth();
        tls_config.alpn_protocols = vec![b"h3".to_vec()];
        Ok(quinn::ClientConfig::new(Arc::new(
            quinn::crypto::rustls::QuicClientConfig::try_from(tls_config)
                .context("failed to build test QUIC client config")?,
        )))
    }

    /// Test-only verifier for the generated self-signed development certificate.
    #[derive(Debug)]
    struct SkipServerVerification(Arc<rustls::crypto::CryptoProvider>);

    impl SkipServerVerification {
        fn new() -> Arc<Self> {
            Arc::new(Self(
                Arc::new(rustls::crypto::aws_lc_rs::default_provider()),
            ))
        }
    }

    impl rustls::client::danger::ServerCertVerifier for SkipServerVerification {
        fn verify_server_cert(
            &self,
            _end_entity: &rustls::pki_types::CertificateDer<'_>,
            _intermediates: &[rustls::pki_types::CertificateDer<'_>],
            _server_name: &rustls::pki_types::ServerName<'_>,
            _ocsp: &[u8],
            _now: rustls::pki_types::UnixTime,
        ) -> std::result::Result<rustls::client::danger::ServerCertVerified, rustls::Error>
        {
            Ok(rustls::client::danger::ServerCertVerified::assertion())
        }

        fn verify_tls12_signature(
            &self,
            message: &[u8],
            cert: &rustls::pki_types::CertificateDer<'_>,
            dss: &rustls::DigitallySignedStruct,
        ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error>
        {
            rustls::crypto::verify_tls12_signature(
                message,
                cert,
                dss,
                &self.0.signature_verification_algorithms,
            )
        }

        fn verify_tls13_signature(
            &self,
            message: &[u8],
            cert: &rustls::pki_types::CertificateDer<'_>,
            dss: &rustls::DigitallySignedStruct,
        ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error>
        {
            rustls::crypto::verify_tls13_signature(
                message,
                cert,
                dss,
                &self.0.signature_verification_algorithms,
            )
        }

        fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
            self.0.signature_verification_algorithms.supported_schemes()
        }
    }

    fn sample_server_events(count: usize) -> Vec<ServerEvent> {
        let mut events = Vec::with_capacity(count);
        for index in 0..count {
            let session_id = (index % 32) as u64;
            match index % 5 {
                0 => events.push(ServerEvent::TextDelta {
                    session_id,
                    delta: "streamed benchmark token ".repeat(2),
                }),
                1 => events.push(ServerEvent::StatusChanged {
                    session_id,
                    status: "running",
                }),
                2 => events.push(ServerEvent::ToolCallStarted {
                    session_id,
                    tool_call_id: format!("call_{index}"),
                    tool_name: "read_file".to_string(),
                    args: r#"{"path":"benchmark.txt"}"#.to_string(),
                }),
                3 => events.push(ServerEvent::ToolCallCompleted {
                    session_id,
                    tool_call_id: format!("call_{index}"),
                    tool_name: "read_file".to_string(),
                    success: true,
                }),
                _ => events.push(ServerEvent::MessageCompleted {
                    session_id,
                    role: "assistant",
                    text: format!("Benchmark message {index} completed."),
                }),
            }
        }
        events
    }

    struct StaticStreamer<const N: usize> {
        text: &'static str,
        deltas: [&'static str; N],
    }

    impl<const N: usize> StaticStreamer<N> {
        const fn new(text: &'static str, deltas: [&'static str; N]) -> Self {
            Self { text, deltas }
        }
    }

    impl<const N: usize> ModelStreamer for StaticStreamer<N> {
        async fn stream_conversation<'a>(
            &'a self,
            _messages: &'a [ConversationMessage<'a>],
            _tools: &'a [ToolSpec],
            _parallel_tool_calls: bool,
            _session_id: SessionId,
            _model: &'a str,
            on_delta: &'a mut (dyn FnMut(&str) -> Result<()> + Send),
        ) -> Result<ModelResponse> {
            for delta in self.deltas {
                on_delta(delta)?;
            }
            Ok(ModelResponse::new(self.text))
        }
    }

    struct ManyDeltaStreamer {
        deltas: usize,
    }

    impl ModelStreamer for ManyDeltaStreamer {
        async fn stream_conversation<'a>(
            &'a self,
            _messages: &'a [ConversationMessage<'a>],
            _tools: &'a [ToolSpec],
            _parallel_tool_calls: bool,
            _session_id: SessionId,
            _model: &'a str,
            on_delta: &'a mut (dyn FnMut(&str) -> Result<()> + Send),
        ) -> Result<ModelResponse> {
            for _ in 0..self.deltas {
                on_delta("x")?;
            }
            Ok(ModelResponse::new("x".repeat(self.deltas)))
        }
    }

    struct FailingStreamer;

    impl ModelStreamer for FailingStreamer {
        async fn stream_conversation<'a>(
            &'a self,
            _messages: &'a [ConversationMessage<'a>],
            _tools: &'a [ToolSpec],
            _parallel_tool_calls: bool,
            _session_id: SessionId,
            _model: &'a str,
            _on_delta: &'a mut (dyn FnMut(&str) -> Result<()> + Send),
        ) -> Result<ModelResponse> {
            anyhow::bail!("model failed")
        }
    }

    struct CancellableStreamer {
        started: Arc<Notify>,
        release: Arc<Notify>,
    }

    impl ModelStreamer for CancellableStreamer {
        async fn stream_conversation<'a>(
            &'a self,
            _messages: &'a [ConversationMessage<'a>],
            _tools: &'a [ToolSpec],
            _parallel_tool_calls: bool,
            _session_id: SessionId,
            _model: &'a str,
            _on_delta: &'a mut (dyn FnMut(&str) -> Result<()> + Send),
        ) -> Result<ModelResponse> {
            self.started.notify_one();
            self.release.notified().await;
            Ok(ModelResponse::new("released"))
        }
    }

    struct DropCancelStreamer {
        started: Arc<Notify>,
        turn: Arc<AtomicUsize>,
    }

    impl ModelStreamer for DropCancelStreamer {
        async fn stream_conversation<'a>(
            &'a self,
            _messages: &'a [ConversationMessage<'a>],
            _tools: &'a [ToolSpec],
            _parallel_tool_calls: bool,
            _session_id: SessionId,
            _model: &'a str,
            _on_delta: &'a mut (dyn FnMut(&str) -> Result<()> + Send),
        ) -> Result<ModelResponse> {
            match self.turn.fetch_add(1, Ordering::SeqCst) {
                0 => {
                    self.started.notify_one();
                    std::future::pending::<()>().await;
                    unreachable!("pending first turn should only finish if cancelled");
                }
                1 => Ok(ModelResponse::new("after drop")),
                turn => panic!("unexpected turn {turn}"),
            }
        }
    }

    struct StaleDropStreamer {
        second_started: Arc<Notify>,
        release_second: Arc<Notify>,
        turn: Arc<AtomicUsize>,
    }

    impl ModelStreamer for StaleDropStreamer {
        async fn stream_conversation<'a>(
            &'a self,
            _messages: &'a [ConversationMessage<'a>],
            _tools: &'a [ToolSpec],
            _parallel_tool_calls: bool,
            _session_id: SessionId,
            _model: &'a str,
            _on_delta: &'a mut (dyn FnMut(&str) -> Result<()> + Send),
        ) -> Result<ModelResponse> {
            match self.turn.fetch_add(1, Ordering::SeqCst) {
                0 => Ok(ModelResponse::new("first")),
                1 => {
                    self.second_started.notify_one();
                    self.release_second.notified().await;
                    Ok(ModelResponse::new("second"))
                }
                turn => panic!("unexpected turn {turn}"),
            }
        }
    }

    struct ToolThenContinueStreamer {
        turn: Arc<AtomicUsize>,
    }

    impl ModelStreamer for ToolThenContinueStreamer {
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
                    on_delta("checking")?;
                    Ok(ModelResponse::with_tool_calls(
                        "checking",
                        [ModelToolCall {
                            item_id: Some("fc_read".to_string()),
                            call_id: "call_read".to_string(),
                            name: "read_file".to_string(),
                            arguments: r#"{"path":"Cargo.toml"}"#.to_string(),
                        }],
                    ))
                }
                1 => {
                    assert_eq!(messages.len(), 4);
                    assert_eq!(messages[0], ConversationMessage::user("read the manifest"));
                    assert_eq!(messages[1], ConversationMessage::assistant("checking"));
                    assert!(matches!(
                        messages[2],
                        ConversationMessage::FunctionCall { .. }
                    ));
                    assert!(matches!(
                        messages[3],
                        ConversationMessage::FunctionOutput { .. }
                    ));
                    on_delta("done")?;
                    Ok(ModelResponse::new("done"))
                }
                2 => {
                    assert_eq!(
                        messages,
                        &[
                            ConversationMessage::user("read the manifest"),
                            ConversationMessage::assistant("checking"),
                            messages[2].clone(),
                            messages[3].clone(),
                            ConversationMessage::assistant("done"),
                            ConversationMessage::user("continue"),
                        ]
                    );
                    Ok(ModelResponse::new("ok"))
                }
                _ => unreachable!("unexpected turn"),
            }
        }
    }

    fn temp_workspace(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "rust-agent-{name}-{}-{}",
            std::process::id(),
            unique_nanos()
        ))
    }

    fn unique_nanos() -> u128 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    }

    fn run_git(root: &Path, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(root)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {} failed: {}\n{}",
            args.join(" "),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
