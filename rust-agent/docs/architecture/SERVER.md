# Server Architecture

`server.rs` exposes the agent service over a shared Axum router and serves that
router through both HTTP compatibility and native HTTP/3 transports.

## Flow

1. `rust-agent serve` loads `AppConfig`, auth, `ChatGptClient`, configured tool
   policy, and `AgentService`.
2. Server startup initializes the FFF scanner on a background blocking worker
   before binding listeners, without waiting for the scan to finish.
3. `ServerConfig` supplies the HTTP compatibility address, HTTP/3 address, TLS
   identity, bearer token, session bounds, event queue capacity, QUIC stream
   limits, idle timeout, and optional parent process to watch for shutdown.
4. One Axum router is built with shared `ServerState`.
5. A TCP listener serves HTTP/1.1 and HTTP/2 compatibility traffic.
6. A Quinn endpoint serves HTTP/3 over QUIC with ALPN `h3`.
7. HTTP compatibility responses include `Alt-Svc` pointing clients at the HTTP/3
   port.
8. Clients create random server-issued sessions before using session routes.
9. Clients can explicitly delete server sessions when they no longer need the
   in-memory state.
10. Turn requests submit work to `AgentService` and stream named SSE frames.

## Routes

- `GET /` returns server identity and supported protocols.
- `GET /healthz` returns a lightweight health response.
- `POST /sessions` creates a random session and returns its current model.
- `GET /models` returns the default model and allowed model list.
- `GET /sessions/{session_id}/model` returns the session model.
- `PUT /sessions/{session_id}/model` changes the session model when idle.
- `DELETE /sessions/{session_id}` deletes the in-memory session, frees its
  registry slot, and closes its session event channel.
- `POST /sessions/{session_id}/turns:stream` submits a user message and returns
  the turn as SSE.
- `GET /sessions/{session_id}/events` subscribes to session events as SSE.

`GET /` and `GET /healthz` are public. All other routes require
`Authorization: Bearer <token>`. Set `RUST_AGENT_SERVER_TOKEN` for a stable
token; otherwise startup prints a generated token to stderr. Numeric session IDs
must come from `POST /sessions`; generated IDs stay within JavaScript's
JSON-safe integer range.

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

`tool_call.started` includes the tool call id, tool name, and raw JSON `args` so
clients can render the requested tool invocation before the tool result arrives.
The short `args` field is intentional and follows pi-mono's tool execution start
event shape.

## Performance Notes

- One `AgentService` and one `ChatGptClient` are reused for all requests.
- Tool policy is loaded once at startup. The default `read-only` mode exposes no
  write tools; `workspace-write` exposes `apply_patch`; `workspace-exec` exposes
  trusted local shell command execution and dedicated git wrappers.
- Server startup initializes the shared FFF search index in the background. If a
  request reaches an FFF-backed tool before scanning completes, that tool is the
  path that waits on a blocking worker and then runs against the ready index.
- HTTP/3 avoids TCP head-of-line blocking and supports concurrent QUIC streams.
- The event queue capacity is configurable for session broadcast streams.
  Direct turn streams use a per-turn unbounded channel so submitted-turn events
  are not dropped when the response body is polled slowly. This is intentional
  and matches pi-mono's trusted-local tradeoff: preserve all turn-local events
  instead of dropping or backpressuring them.
- SSE bodies are streamed from Tokio channels instead of buffering whole turns.
- HTTP/3 concurrent bidirectional and unidirectional stream limits are
  configurable.
- The event bus is per session, so unrelated sessions do not share broadcast
  receivers.
- Session and event-channel counts are bounded to cap process-local memory
  growth under repeated requests.

## Current Scope

The server is process-local and uses one bearer token for non-health routes.
Bind to loopback by default, set `RUST_AGENT_SERVER_TOKEN` when scripts need a
stable token, and treat the generated token as a local-development convenience.
Sessions are in-memory, random, and process-local. Clients can explicitly delete
sessions, which removes future route access and closes the session event stream;
it does not cancel an already-running direct turn stream. Persistence, turn
cancellation, per-user authorization, and multi-process coordination are not
implemented. Use `workspace-write` and `workspace-exec` only for trusted local
deployments because any bearer-token holder can ask the model to edit workspace
files or run local commands. WebSocket endpoints are not implemented because SSE
matches the current server-to-client event flow with less protocol overhead. Set
`RUST_AGENT_PARENT_PID` only when another local supervisor process should control
server lifetime.
