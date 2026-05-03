# Server Architecture

`server.rs` exposes the agent service over a shared Axum router and serves that
router through both HTTP compatibility and native HTTP/3 transports.

## Flow

1. `rust-agent serve` loads `AppConfig`, auth, `ChatGptClient`, and
   `AgentService`.
2. Server startup initializes the FFF scanner on a background blocking worker
   before binding listeners, without waiting for the scan to finish.
3. `ServerConfig` supplies the HTTP compatibility address, HTTP/3 address, TLS
   identity, event queue capacity, QUIC stream limits, and idle timeout.
4. One Axum router is built with shared `ServerState`.
5. A TCP listener serves HTTP/1.1 and HTTP/2 compatibility traffic.
6. A Quinn endpoint serves HTTP/3 over QUIC with ALPN `h3`.
7. HTTP compatibility responses include `Alt-Svc` pointing clients at the HTTP/3
   port.
8. Turn requests submit work to `AgentService` and stream named SSE frames.

## Routes

- `GET /` returns server identity and supported protocols.
- `GET /healthz` returns a lightweight health response.
- `GET /models` returns the default model and allowed model list.
- `GET /sessions/{session_id}/model` returns the session model.
- `PUT /sessions/{session_id}/model` changes the session model when idle.
- `POST /sessions/{session_id}/turns:stream` submits a user message and returns
  the turn as SSE.
- `GET /sessions/{session_id}/events` subscribes to session events as SSE.

## Transport

The HTTP/3 path is native QUIC through `quinn`, `h3`, `h3-quinn`, and
`h3-axum`. The compatibility listener uses Axum on TCP for local tools, clients
without HTTP/3 support, and easy smoke tests. Both listeners use the same router
so endpoint behavior stays identical across protocols.

HTTP/3 always uses TLS. When `RUST_AGENT_SERVER_TLS_CERT` and
`RUST_AGENT_SERVER_TLS_KEY` are unset, the server generates a self-signed
development certificate for `localhost` and `127.0.0.1`. Production deployments
should provide a stable certificate and key.

## Events

SSE frames use explicit event names and compact JSON payloads. The direct turn
stream receives events from the submitted turn. The session event stream uses a
per-session broadcast channel and sends heartbeat frames while connected.

Important event names include:

- `session.status_changed`
- `message.text_delta`
- `message.completed`
- `cache.health`
- `tool_call.started`
- `tool_call.completed`
- `turn.completed`
- `session.error`
- `server.heartbeat`
- `server.events_lagged`

## Performance Notes

- One `AgentService` and one `ChatGptClient` are reused for all requests.
- Server startup initializes the shared FFF search index in the background. If a
  request reaches an FFF-backed tool before scanning completes, that tool is the
  path that waits on a blocking worker and then runs against the ready index.
- HTTP/3 avoids TCP head-of-line blocking and supports concurrent QUIC streams.
- The event queue capacity is configurable and applies to direct turn streams
  and session broadcast streams.
- SSE bodies are streamed from Tokio channels instead of buffering whole turns.
- HTTP/3 concurrent bidirectional and unidirectional stream limits are
  configurable.
- The event bus is per session, so unrelated sessions do not share broadcast
  receivers.

## Current Scope

The server is process-local and unauthenticated on its listening sockets. Bind to
loopback by default, and put authentication, authorization, persistence,
cancellation, and multi-process coordination behind explicit product decisions.
WebSocket endpoints are not implemented because SSE matches the current
server-to-client event flow with less protocol overhead.
