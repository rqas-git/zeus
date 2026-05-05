//! Native HTTP/3 and HTTP compatibility server.

use std::collections::HashMap;
use std::collections::HashSet;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

use anyhow::Context;
use anyhow::Result;
use async_stream::stream;
use axum::body::Body;
use axum::extract::Path as AxumPath;
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
use axum::routing::delete;
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
use tokio_stream::wrappers::UnboundedReceiverStream;
use tokio_stream::StreamExt;

use crate::agent_loop::is_turn_cancelled;
use crate::agent_loop::AgentEvent;
use crate::agent_loop::CacheHealth;
use crate::agent_loop::ModelStreamer;
use crate::agent_loop::SessionId;
use crate::agent_loop::TokenUsage;
use crate::config::ServerConfig;
use crate::service::AgentService;

const SERVER_NAME: &str = "rust-agent";
const SSE_CONTENT_TYPE: &str = "text/event-stream";
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(10);
const PARENT_WATCH_INTERVAL: Duration = Duration::from_secs(1);
const GENERATED_TOKEN_BYTES: usize = 32;
const MAX_JSON_SAFE_INTEGER: u64 = (1u64 << 53) - 1;

/// Runs the local server until interrupted.
///
/// # Errors
/// Returns an error if either listener cannot start or exits unexpectedly.
pub(crate) async fn serve<M>(service: AgentService<M>, config: ServerConfig) -> Result<()>
where
    M: ModelStreamer + Send + Sync + 'static,
{
    let service = Arc::new(service);
    let h3_endpoint = h3_endpoint(&config)?;
    let h3_addr = h3_endpoint
        .local_addr()
        .context("failed to read H3 listener address")?;
    let auth = ServerAuth::from_config(config.auth_token())?;
    if auth.is_generated() {
        eprintln!("rust-agent server bearer token: {}", auth.token());
    } else {
        eprintln!("rust-agent server bearer token loaded from RUST_AGENT_SERVER_TOKEN");
    }
    let state = ServerState::with_limits(
        service,
        config.event_queue_capacity(),
        h3_addr,
        auth,
        config.max_sessions(),
        config.max_event_channels(),
    );
    let app = router(state);
    let http_addr = config.http_addr();
    let parent_pid = config.parent_pid();
    let http_app = app.clone();
    let h3_app = app;

    let http_task = tokio::spawn(async move { run_http_listener(http_app, http_addr).await });
    let h3_task = tokio::spawn(async move { run_h3_listener(h3_app, h3_endpoint).await });

    tokio::select! {
        result = http_task => result.context("HTTP listener task failed")??,
        result = h3_task => result.context("H3 listener task failed")??,
        result = wait_for_parent_process(parent_pid) => result?,
        result = tokio::signal::ctrl_c() => result.context("failed to listen for shutdown signal")?,
    }

    Ok(())
}

async fn wait_for_parent_process(parent_pid: Option<libc::pid_t>) -> Result<()> {
    let Some(parent_pid) = parent_pid else {
        return futures_util::future::pending::<Result<()>>().await;
    };

    loop {
        if !process_is_running(parent_pid) {
            eprintln!("rust-agent parent process {parent_pid} exited; shutting down");
            return Ok(());
        }
        tokio::time::sleep(PARENT_WATCH_INTERVAL).await;
    }
}

fn process_is_running(pid: libc::pid_t) -> bool {
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
        .route("/", get(root))
        .route("/healthz", get(healthz))
        .route("/models", get(models::<M>))
        .route("/sessions", post(create_session::<M>))
        .route("/sessions/{session_id}", delete(delete_session::<M>))
        .route(
            "/sessions/{session_id}/model",
            get(session_model::<M>).put(set_session_model::<M>),
        )
        .route(
            "/sessions/{session_id}/turns:stream",
            post(stream_turn::<M>),
        )
        .route(
            "/sessions/{session_id}/turns:cancel",
            post(cancel_turn::<M>),
        )
        .route("/sessions/{session_id}/events", get(session_events::<M>))
        .with_state(state.clone())
        .layer(middleware::from_fn_with_state(
            state.clone(),
            require_auth::<M>,
        ))
        .layer(middleware::from_fn_with_state(state, add_alt_svc::<M>))
}

async fn run_http_listener(app: Router, addr: SocketAddr) -> Result<()> {
    let listener = TcpListener::bind(addr)
        .await
        .with_context(|| format!("failed to bind HTTP compatibility listener at {addr}"))?;
    let local_addr = listener
        .local_addr()
        .context("failed to read HTTP compatibility listener address")?;
    eprintln!("rust-agent HTTP compatibility listening on http://{local_addr}");
    axum::serve(listener, app)
        .await
        .context("HTTP compatibility listener failed")
}

async fn run_h3_listener(app: Router, endpoint: quinn::Endpoint) -> Result<()> {
    let local_addr = endpoint
        .local_addr()
        .context("failed to read H3 listener address")?;
    eprintln!("rust-agent HTTP/3 listening on https://{local_addr}");

    while let Some(incoming) = endpoint.accept().await {
        let app = app.clone();
        tokio::spawn(async move {
            if let Err(error) = handle_h3_connection(incoming, app).await {
                eprintln!("rust-agent H3 connection error: {error}");
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
                        eprintln!("rust-agent H3 request error: {error}");
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
}

impl<M> Clone for ServerState<M> {
    fn clone(&self) -> Self {
        Self {
            service: Arc::clone(&self.service),
            events: self.events.clone(),
            sessions: self.sessions.clone(),
            auth: self.auth.clone(),
            alt_svc: self.alt_svc.clone(),
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
        Self::with_limits(
            service,
            event_queue_capacity,
            h3_addr,
            ServerAuth::for_test(),
            usize::MAX,
            usize::MAX,
        )
    }

    fn with_limits(
        service: Arc<AgentService<M>>,
        event_queue_capacity: usize,
        h3_addr: SocketAddr,
        auth: ServerAuth,
        max_sessions: usize,
        max_event_channels: usize,
    ) -> Self {
        let alt_svc = HeaderValue::from_str(&format!("h3=\":{}\"; ma=86400", h3_addr.port()))
            .expect("generated Alt-Svc header must be valid");
        Self {
            service,
            events: EventBus::new(event_queue_capacity, max_event_channels),
            sessions: SessionRegistry::new(max_sessions),
            auth,
            alt_svc,
        }
    }

    fn require_session(&self, session_id: u64) -> Result<SessionId> {
        let session_id = SessionId::new(session_id);
        anyhow::ensure!(self.sessions.contains(session_id)?, "session not found");
        Ok(session_id)
    }

    fn delete_session(&self, session_id: u64) -> Result<bool>
    where
        M: ModelStreamer + Sync,
    {
        let session_id = SessionId::new(session_id);
        if !self.sessions.remove(session_id)? {
            return Ok(false);
        }
        let _ = self.service.delete_session(session_id)?;
        self.events.remove_session(session_id)?;
        Ok(true)
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
    generated: bool,
}

impl ServerAuth {
    fn from_config(configured: Option<&str>) -> Result<Self> {
        match configured {
            Some(token) => {
                let token = token.trim();
                anyhow::ensure!(!token.is_empty(), "server bearer token cannot be empty");
                Ok(Self {
                    token: Arc::from(token),
                    generated: false,
                })
            }
            None => Ok(Self {
                token: Arc::from(generate_bearer_token()?),
                generated: true,
            }),
        }
    }

    #[cfg(test)]
    fn for_test() -> Self {
        Self {
            token: Arc::from("test-token"),
            generated: false,
        }
    }

    fn token(&self) -> &str {
        &self.token
    }

    const fn is_generated(&self) -> bool {
        self.generated
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
                .map_err(|_| anyhow::anyhow!("session registry lock was poisoned"))?;
            anyhow::ensure!(sessions.len() < self.max_sessions, "session limit exceeded");
            if sessions.insert(session_id) {
                return Ok(session_id);
            }
        }
        anyhow::bail!("failed to allocate unique session id")
    }

    fn contains(&self, session_id: SessionId) -> Result<bool> {
        Ok(self
            .sessions
            .lock()
            .map_err(|_| anyhow::anyhow!("session registry lock was poisoned"))?
            .contains(&session_id))
    }

    fn remove(&self, session_id: SessionId) -> Result<bool> {
        Ok(self
            .sessions
            .lock()
            .map_err(|_| anyhow::anyhow!("session registry lock was poisoned"))?
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
            .map_err(|_| anyhow::anyhow!("session registry lock was poisoned"))?;
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
            .map_err(|_| anyhow::anyhow!("event bus lock was poisoned"))?;
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
            .map_err(|_| anyhow::anyhow!("event bus lock was poisoned"))?;
        cleanup_empty_event_channels(&mut sessions);
        Ok(sessions.get(&session_id).cloned())
    }

    fn remove_session(&self, session_id: SessionId) -> Result<()> {
        self.sessions
            .lock()
            .map_err(|_| anyhow::anyhow!("event bus lock was poisoned"))?
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
    path == "/" || path == "/healthz"
}

fn unauthorized_response() -> Response<Body> {
    Response::builder()
        .status(StatusCode::UNAUTHORIZED)
        .header(header::WWW_AUTHENTICATE, "Bearer")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(r#"{"error":"missing or invalid bearer token"}"#))
        .expect("unauthorized response headers must be valid")
}

async fn root() -> impl IntoResponse {
    Json(RootResponse {
        name: SERVER_NAME,
        protocol: "http/1.1,http/2,http/3",
    })
}

async fn healthz() -> impl IntoResponse {
    Json(HealthResponse { healthy: true })
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

async fn create_session<M>(State(state): State<ServerState<M>>) -> impl IntoResponse
where
    M: ModelStreamer + Send + Sync + 'static,
{
    let session_id = match state.sessions.reserve_random() {
        Ok(session_id) => session_id,
        Err(error) => return error_response(StatusCode::TOO_MANY_REQUESTS, error),
    };
    if let Err(error) = state.service.create_session(session_id) {
        state.sessions.release(session_id);
        return error_response(StatusCode::TOO_MANY_REQUESTS, error);
    }
    Json(CreateSessionResponse {
        session_id: session_id.get(),
        model: state.service.default_model().to_string(),
    })
    .into_response()
}

async fn delete_session<M>(
    State(state): State<ServerState<M>>,
    AxumPath(session_id): AxumPath<u64>,
) -> impl IntoResponse
where
    M: ModelStreamer + Send + Sync + 'static,
{
    match state.delete_session(session_id) {
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
    match state.service.set_session_model(session_id, &request.model) {
        Ok(model) => Json(SessionModelResponse { model }).into_response(),
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
    let (tx, rx) = mpsc::unbounded_channel();
    let service = Arc::clone(&state.service);
    let events = state.events.clone();
    tokio::spawn(async move {
        let mut error_forwarded = false;
        let result = service
            .submit_user_message_with_reasoning_effort(
                session_id,
                request.message,
                reasoning_effort.as_deref(),
                |event| {
                    let event = ServerEvent::from_agent_event(event);
                    let is_error = matches!(event, ServerEvent::Error { .. });
                    events.publish(event.clone())?;
                    let _ = tx.send(event);
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
                let _ = events.publish(event.clone());
                let _ = tx.send(event);
            }
            Err(error) if is_turn_cancelled(&error) => {
                let event = ServerEvent::TurnCancelled {
                    session_id: session_id.get(),
                };
                let _ = events.publish(event.clone());
                let _ = tx.send(event);
            }
            Err(error) if !error_forwarded => {
                let event = ServerEvent::Error {
                    session_id: session_id.get(),
                    message: error.to_string(),
                };
                let _ = events.publish(event.clone());
                let _ = tx.send(event);
            }
            Err(_) => {}
        }
    });

    sse_from_unbounded_mpsc(rx)
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

fn sse_from_unbounded_mpsc(
    receiver: mpsc::UnboundedReceiver<ServerEvent>,
) -> axum::response::Response {
    let stream = UnboundedReceiverStream::new(receiver).map(encode_sse);
    sse_response(Body::from_stream(stream))
}

fn sse_from_broadcast(
    session_id: SessionId,
    mut receiver: broadcast::Receiver<ServerEvent>,
) -> axum::response::Response {
    let stream = stream! {
        yield encode_sse_ref(&ServerEvent::ServerConnected {
            session_id: session_id.get(),
        });

        let mut heartbeat = tokio::time::interval(HEARTBEAT_INTERVAL);
        loop {
            tokio::select! {
                _ = heartbeat.tick() => {
                    yield encode_sse_ref(&ServerEvent::ServerHeartbeat {
                        session_id: session_id.get(),
                    });
                }
                event = receiver.recv() => {
                    match event {
                        Ok(event) => yield encode_sse(event),
                        Err(broadcast::error::RecvError::Lagged(skipped)) => {
                            yield encode_sse_ref(&ServerEvent::EventsLagged {
                                session_id: session_id.get(),
                                skipped,
                            });
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

fn encode_sse(event: ServerEvent) -> std::result::Result<Bytes, Infallible> {
    encode_sse_ref(&event)
}

fn encode_sse_ref(event: &ServerEvent) -> std::result::Result<Bytes, Infallible> {
    Ok(encode_sse_bytes(event).expect("server event serialization should not fail"))
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

fn error_response(status: StatusCode, error: anyhow::Error) -> axum::response::Response {
    (
        status,
        Json(ErrorResponse {
            error: error.to_string(),
        }),
    )
        .into_response()
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
            message_count: cache_health.message_count,
            input_bytes: cache_health.input_bytes,
            response_id: cache_health.response_id.clone(),
            usage: cache_health.usage.map(TokenUsageEvent::from_usage),
            cache_status: cache_health.cache_status.as_str(),
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize)]
struct TokenUsageEvent {
    input_tokens: Option<u64>,
    cached_input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    total_tokens: Option<u64>,
}

impl TokenUsageEvent {
    const fn from_usage(usage: TokenUsage) -> Self {
        Self {
            input_tokens: usage.input_tokens,
            cached_input_tokens: usage.cached_input_tokens,
            output_tokens: usage.output_tokens,
            total_tokens: usage.total_tokens,
        }
    }
}

#[derive(Serialize)]
struct RootResponse {
    name: &'static str,
    protocol: &'static str,
}

#[derive(Serialize)]
struct HealthResponse {
    healthy: bool,
}

#[derive(Serialize)]
struct ModelsResponse {
    default_model: String,
    allowed_models: Vec<String>,
    default_reasoning_effort: String,
    reasoning_efforts: Vec<String>,
}

#[derive(Serialize)]
struct CreateSessionResponse {
    session_id: u64,
    model: String,
}

#[derive(Serialize)]
struct SessionModelResponse {
    model: String,
}

#[derive(Serialize)]
struct CancelTurnResponse {
    cancelled: bool,
}

#[derive(Serialize)]
struct ErrorResponse {
    error: String,
}

#[derive(Deserialize)]
struct SetModelRequest {
    model: String,
}

#[derive(Deserialize)]
struct TurnRequest {
    message: String,
    reasoning_effort: Option<String>,
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Instant;

    use axum::body::to_bytes;
    use axum::http::Method;
    use axum::http::Request;
    use bytes::Buf;
    use tokio::sync::Notify;
    use tower::ServiceExt;

    use crate::agent_loop::ModelResponse;
    use crate::agent_loop::ModelToolCall;
    use crate::bench_support::DurationSummary;
    use crate::client::ConversationMessage;
    use crate::config::ContextWindowConfig;
    use crate::config::ModelConfig;
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

        let models = app
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

        let model = app
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
        let state = ServerState::with_limits(
            service,
            16,
            "127.0.0.1:4433".parse().unwrap(),
            ServerAuth::for_test(),
            1,
            usize::MAX,
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
    async fn deletes_sessions_and_frees_capacity() {
        let service = Arc::new(
            AgentService::new(
                StaticStreamer::new("ok", []),
                ContextWindowConfig::default(),
                ModelConfig::new("test-default", ["test-default"]).unwrap(),
            )
            .with_session_limit(1),
        );
        let state = ServerState::with_limits(
            service,
            16,
            "127.0.0.1:4433".parse().unwrap(),
            ServerAuth::for_test(),
            1,
            usize::MAX,
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
        let state = ServerState::with_limits(
            service,
            16,
            "127.0.0.1:4433".parse().unwrap(),
            ServerAuth::for_test(),
            usize::MAX,
            1,
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
    async fn direct_turn_stream_preserves_events_above_configured_queue_capacity() {
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

        assert_eq!(body.matches("event: message.text_delta\n").count(), DELTAS);
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
        let events_per_s = EVENTS as f64 / summary.median.as_secs_f64();
        let throughput_mib_s =
            encoded_bytes as f64 / summary.median.as_secs_f64() / 1024.0 / 1024.0;
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
                            messages[2],
                            messages[3],
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
}
