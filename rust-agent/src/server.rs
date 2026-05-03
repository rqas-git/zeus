//! Native HTTP/3 and HTTP compatibility server.

use std::collections::HashMap;
use std::convert::Infallible;
use std::io::BufReader;
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
use bytes::Bytes;
use rcgen::CertifiedKey;
use serde::Deserialize;
use serde::Serialize;
use tokio::net::TcpListener;
use tokio::sync::broadcast;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tokio_stream::StreamExt;

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
    let state = ServerState::new(service, config.event_queue_capacity(), h3_addr);
    let app = router(state);
    let http_addr = config.http_addr();
    let http_app = app.clone();
    let h3_app = app;

    let http_task = tokio::spawn(async move { run_http_listener(http_app, http_addr).await });
    let h3_task = tokio::spawn(async move { run_h3_listener(h3_app, h3_endpoint).await });

    tokio::select! {
        result = http_task => result.context("HTTP listener task failed")??,
        result = h3_task => result.context("H3 listener task failed")??,
        result = tokio::signal::ctrl_c() => result.context("failed to listen for shutdown signal")?,
    }

    Ok(())
}

fn router<M>(state: ServerState<M>) -> Router
where
    M: ModelStreamer + Send + Sync + 'static,
{
    Router::new()
        .route("/", get(root))
        .route("/healthz", get(healthz))
        .route("/models", get(models::<M>))
        .route(
            "/sessions/{session_id}/model",
            get(session_model::<M>).put(set_session_model::<M>),
        )
        .route(
            "/sessions/{session_id}/turns:stream",
            post(stream_turn::<M>),
        )
        .route("/sessions/{session_id}/events", get(session_events::<M>))
        .with_state(state.clone())
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
    let bytes = std::fs::read(path)
        .with_context(|| format!("failed to read TLS certificate {}", path.display()))?;
    let mut reader = BufReader::new(bytes.as_slice());
    let certs = rustls_pemfile::certs(&mut reader)
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
    let bytes = std::fs::read(path)
        .with_context(|| format!("failed to read TLS private key {}", path.display()))?;
    let mut reader = BufReader::new(bytes.as_slice());
    rustls_pemfile::private_key(&mut reader)
        .with_context(|| format!("failed to parse TLS private key {}", path.display()))?
        .with_context(|| {
            format!(
                "TLS private key {} did not contain a private key",
                path.display()
            )
        })
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
    event_queue_capacity: usize,
    alt_svc: HeaderValue,
}

impl<M> Clone for ServerState<M> {
    fn clone(&self) -> Self {
        Self {
            service: Arc::clone(&self.service),
            events: self.events.clone(),
            event_queue_capacity: self.event_queue_capacity,
            alt_svc: self.alt_svc.clone(),
        }
    }
}

impl<M> ServerState<M> {
    fn new(
        service: Arc<AgentService<M>>,
        event_queue_capacity: usize,
        h3_addr: SocketAddr,
    ) -> Self {
        let alt_svc = HeaderValue::from_str(&format!("h3=\":{}\"; ma=86400", h3_addr.port()))
            .expect("generated Alt-Svc header must be valid");
        Self {
            service,
            events: EventBus::new(event_queue_capacity),
            event_queue_capacity,
            alt_svc,
        }
    }
}

#[derive(Clone)]
struct EventBus {
    sessions: Arc<Mutex<HashMap<SessionId, broadcast::Sender<ServerEvent>>>>,
    capacity: usize,
}

impl EventBus {
    fn new(capacity: usize) -> Self {
        Self {
            sessions: Arc::new(Mutex::new(HashMap::new())),
            capacity,
        }
    }

    fn subscribe(&self, session_id: SessionId) -> Result<broadcast::Receiver<ServerEvent>> {
        Ok(self.channel(session_id)?.subscribe())
    }

    fn publish(&self, event: ServerEvent) -> Result<()> {
        let sender = self.channel(event.session_id())?;
        let _ = sender.send(event);
        Ok(())
    }

    fn channel(&self, session_id: SessionId) -> Result<broadcast::Sender<ServerEvent>> {
        let mut sessions = self
            .sessions
            .lock()
            .map_err(|_| anyhow::anyhow!("event bus lock was poisoned"))?;
        Ok(sessions
            .entry(session_id)
            .or_insert_with(|| broadcast::channel(self.capacity).0)
            .clone())
    }
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
    })
}

async fn session_model<M>(
    State(state): State<ServerState<M>>,
    AxumPath(session_id): AxumPath<u64>,
) -> impl IntoResponse
where
    M: ModelStreamer + Send + Sync + 'static,
{
    match state
        .service
        .session_model(SessionId::new(session_id))
        .await
    {
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
    match state
        .service
        .set_session_model(SessionId::new(session_id), &request.model)
    {
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

    let session_id = SessionId::new(session_id);
    let (tx, rx) = mpsc::channel(state.event_queue_capacity);
    let service = Arc::clone(&state.service);
    let events = state.events.clone();
    tokio::spawn(async move {
        let mut error_forwarded = false;
        let result = service
            .submit_user_message(session_id, request.message, |event| {
                let event = ServerEvent::from_agent_event(event);
                let is_error = matches!(event, ServerEvent::Error { .. });
                events.publish(event.clone())?;
                tx.try_send(event)
                    .context("event stream client is too slow")?;
                if is_error {
                    error_forwarded = true;
                }
                Ok(())
            })
            .await;

        match result {
            Ok(()) => {
                let event = ServerEvent::TurnCompleted {
                    session_id: session_id.get(),
                };
                let _ = events.publish(event.clone());
                let _ = tx.try_send(event);
            }
            Err(error) if !error_forwarded => {
                let event = ServerEvent::Error {
                    session_id: session_id.get(),
                    message: error.to_string(),
                };
                let _ = events.publish(event.clone());
                let _ = tx.try_send(event);
            }
            Err(_) => {}
        }
    });

    sse_from_mpsc(rx)
}

async fn session_events<M>(
    State(state): State<ServerState<M>>,
    AxumPath(session_id): AxumPath<u64>,
) -> impl IntoResponse
where
    M: ModelStreamer + Send + Sync + 'static,
{
    let session_id = SessionId::new(session_id);
    match state.events.subscribe(session_id) {
        Ok(receiver) => sse_from_broadcast(session_id, receiver),
        Err(error) => error_response(StatusCode::INTERNAL_SERVER_ERROR, error),
    }
}

fn sse_from_mpsc(receiver: mpsc::Receiver<ServerEvent>) -> axum::response::Response {
    let stream = ReceiverStream::new(receiver).map(encode_sse);
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
            } => Self::ToolCallStarted {
                session_id: session_id.get(),
                tool_call_id: tool_call_id.to_string(),
                tool_name: tool_name.to_string(),
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
            | Self::TurnCompleted { session_id } => SessionId::new(*session_id),
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
}

#[derive(Serialize)]
struct SessionModelResponse {
    model: String,
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
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use axum::body::to_bytes;
    use axum::http::Method;
    use axum::http::Request;
    use bytes::Buf;
    use tower::ServiceExt;

    use crate::agent_loop::ModelResponse;
    use crate::bench_support::DurationSummary;
    use crate::client::ConversationMessage;
    use crate::config::ContextWindowConfig;
    use crate::config::ModelConfig;
    use crate::tools::ToolSpec;

    use super::*;

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
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = to_bytes(models.into_body(), usize::MAX).await.unwrap();
        assert_eq!(
            std::str::from_utf8(&body).unwrap(),
            r#"{"default_model":"test-default","allowed_models":["test-default","test-fast"]}"#
        );
    }

    #[tokio::test]
    async fn streams_turn_events_as_sse() {
        let service = Arc::new(AgentService::new(
            StaticStreamer::new("hello", ["hel", "lo"]),
            ContextWindowConfig::default(),
            ModelConfig::new("test-default", ["test-default"]).unwrap(),
        ));
        let state = ServerState::new(service, 16, "127.0.0.1:4433".parse().unwrap());
        let app = router(state);
        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/sessions/7/turns:stream")
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
    async fn failed_turn_streams_one_error_event() {
        let service = Arc::new(AgentService::new(
            FailingStreamer,
            ContextWindowConfig::default(),
            ModelConfig::new("test-default", ["test-default"]).unwrap(),
        ));
        let state = ServerState::new(service, 16, "127.0.0.1:4433".parse().unwrap());
        let app = router(state);
        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/sessions/7/turns:stream")
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
}
