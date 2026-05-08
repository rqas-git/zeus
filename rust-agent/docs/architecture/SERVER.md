# Server Architecture

`server.rs` exposes the agent service over a shared Axum router and serves that
router through both HTTP compatibility and native HTTP/3 transports.

## Flow

1. `rust-agent serve` loads `AppConfig`, auth, `ChatGptClient`, configured tool
   policy, SQLite session storage, and `AgentService`.
2. Server startup initializes the FFF scanner on a background blocking worker
   before binding listeners, without waiting for the scan to finish.
3. `ServerConfig` supplies the HTTP compatibility address, HTTP/3 address, TLS
   identity, bearer token, session bounds, event queue capacity, QUIC stream
   limits, idle timeout, remote-HTTP opt-in, and optional parent process to
   watch for shutdown.
4. One Axum router is built with shared `ServerState`.
5. A TCP listener serves HTTP/1.1 and HTTP/2 compatibility traffic.
6. A Quinn endpoint serves HTTP/3 over QUIC with ALPN `h3`.
7. After both listeners bind, startup emits one JSON readiness line to stderr
   with the bound addresses, bearer token, protocol version, workspace root, and
   process id.
8. HTTP compatibility responses include `Alt-Svc` pointing clients at the HTTP/3
   port.
9. Clients can read workspace Git metadata and request branch switches through
   the backend so tool indexes and UI state stay aligned.
10. Clients can list durable SQLite session metadata for recent-session UIs.
11. Clients create random server-issued sessions before using turn routes.
12. Clients can fetch metadata for one durable SQLite session by id.
13. Clients can restore an existing durable SQLite session by id before using
   session routes.
14. Clients can explicitly delete server sessions when they no longer need the
   state.
15. Turn requests submit work to `AgentService` and stream named SSE frames.
16. Compaction requests summarize the active session and publish session events
   for passive subscribers.

## Routes

- `GET /` returns server identity, supported protocols, and the canonical
  workspace root used by built-in tools.
- `GET /healthz` returns a lightweight health response.
- `GET /capabilities` returns protocol version, contract hash, transports,
  route groups, and feature flags for frontend compatibility checks.
- `GET /models` returns the default model, allowed model list, default
  reasoning effort, and allowed reasoning efforts.
- `GET /permissions` returns the default tool policy and allowed tool policies.
- `GET /workspace` returns the canonical workspace root plus Git branch
  metadata when the workspace is a Git repository.
- `POST /workspace/branch` switches the workspace to a local branch, auto-stashes
  dirty changes, and refreshes backend search state.
- `GET /sessions?limit=50&offset=0` lists durable session metadata ordered by
  recent activity.
- `POST /sessions` creates a random session and returns its current model and
  tool policy.
- `GET /sessions/{session_id}` returns metadata for one durable session.
- `GET /sessions/{session_id}/model` returns the session model.
- `PUT /sessions/{session_id}/model` changes the session model when idle.
- `GET /sessions/{session_id}/permissions` returns the session tool policy.
- `PUT /sessions/{session_id}/permissions` changes the session tool policy when
  idle.
- `POST /sessions:restore` restores an existing SQLite-backed session id into
  the active registry and returns its transcript records.
- `DELETE /sessions/{session_id}` deletes the session from memory and SQLite,
  frees its registry slot, and closes its session event channel.
- `POST /sessions/{session_id}/turns:stream` submits a user message with an
  optional reasoning effort override and returns the turn as SSE.
- `POST /sessions/{session_id}/turns:cancel` requests cancellation of the
  currently running turn for the session and returns whether a turn was active.
- `POST /sessions/{session_id}/terminal:run` runs a user-initiated terminal
  command through the `exec_command` tool when the session policy is
  `workspace-exec`, and records the command plus output in the session
  transcript without changing that policy.
- `POST /sessions/{session_id}/compact` manually compacts the session and
  accepts optional summary instructions and a reasoning effort override, then
  returns the generated summary, first retained message id, token estimate, and
  file operation details.
- `GET /sessions/{session_id}/events` subscribes to session events as SSE.

`GET /`, `GET /healthz`, and `GET /capabilities` are public. All other routes
require `Authorization: Bearer <token>`. Set `RUST_AGENT_SERVER_TOKEN` for a
stable token; otherwise startup prints a generated token to stderr. The
plaintext HTTP compatibility listener rejects non-loopback bind addresses unless
`RUST_AGENT_SERVER_ALLOW_REMOTE_HTTP=true` is set for a trusted deployment.
Numeric session IDs must come from `POST /sessions` or an existing local SQLite
row discovered
through `GET /sessions`, `GET /sessions/{session_id}`, or restored through
`POST /sessions:restore`; generated IDs stay within JavaScript's JSON-safe
integer range.

Session metadata responses are intended for clients such as Zeus's macOS
sidebar. List responses are paginated with `limit`, `offset`, and nullable
`next_offset`. Each session metadata object includes `session_id`, `model`,
`status`, `created_at_ms`, `updated_at_ms`, `message_count`, `active`, and a
nullable `last_message` preview. `last_message` contains the latest user or
assistant message only, not tool transcript items, and includes `message_id`,
`role`, `preview`, `truncated`, and `created_at_ms`. Transcript records remain
exclusive to `POST /sessions:restore` so listing sessions does not load or
return full conversations.

## Transport

The HTTP/3 path is native QUIC through `quinn`, `h3`, `h3-quinn`, and
`h3-axum`. The compatibility listener uses Axum on TCP for local tools, clients
without HTTP/3 support, and easy smoke tests. Both listeners use the same router
so endpoint behavior stays identical across protocols.

Both bind addresses may use port `0`; in that case the OS assigns free ports and
the startup readiness line reports the selected addresses.

HTTP/3 always uses TLS. When `RUST_AGENT_SERVER_TLS_CERT` and
`RUST_AGENT_SERVER_TLS_KEY` are unset, the server generates a self-signed
development certificate for `localhost` and `127.0.0.1`. Production deployments
should provide a stable certificate and key.

## Events

SSE frames use explicit event names and compact JSON payloads. The direct turn
stream is the canonical source for events from the submitted turn. The session
event stream uses a per-session broadcast channel for passive updates and sends
heartbeat frames while connected.

Important event names include:

- `server.connected`
- `server.heartbeat`
- `server.events_lagged`
- `session.status_changed`
- `message.text_delta`
- `message.completed`
- `cache.health`
- `turn.token_usage`
- `compaction.started`
- `compaction.completed`
- `tool_call.started`
- `tool_call.completed`
- `turn.completed`
- `turn.cancelled`
- `session.error`

`cache.health` includes prompt-cache status, stable/request input hashes, and
provider token counters: input, cached input, output, reasoning output, and total
tokens when reported.
`turn.token_usage` aggregates the same provider counters across all model
responses in the completed turn.
`tool_call.started` includes the tool call id, tool name, and raw JSON `args` so
clients can render the requested tool invocation before the tool result arrives.
The short `args` field is intentional and follows pi-mono's tool execution start
event shape.
Terminal command requests emit the same user-message, session-status, and
tool-call lifecycle events as model-initiated tool calls. They require the
session's current tool policy to expose `exec_command`, but the route returns a
bounded JSON result instead of an SSE turn stream and does not mutate the
policy. Compaction requests emit status and compaction lifecycle events and
return a bounded JSON summary result.

## Contract

`rust-agent contract` prints the Zeus API contract artifact. The checked-in
`docs/contracts/zeus-api-contract.json` is generated from Rust request,
response, and event types and is used by Swift contract checks.

## Performance Notes

- One `AgentService` and one `ChatGptClient` are reused for all requests.
- Tool policy is loaded once at startup. The default `read-only` mode exposes no
  write tools; `workspace-write` exposes `apply_patch`; `workspace-exec` exposes
  trusted local shell command execution.
- The workspace root is resolved once at startup from `RUST_AGENT_WORKSPACE` or
  the process current directory and is reported by `GET /` for clients that need
  to verify backend/UI workspace alignment.
- Server startup initializes the shared FFF search index in the background. If a
  request reaches an FFF-backed tool before scanning completes, that tool is the
  path that waits on a blocking worker and then runs against the ready index.
- HTTP/3 avoids TCP head-of-line blocking and supports concurrent QUIC streams.
- The event queue capacity is configurable for session broadcast streams.
  Direct turn streams use a separate bounded per-turn queue. Dropping the
  response body cancels the active turn, and a full turn queue stops the turn
  instead of allowing unbounded memory growth.
- SSE bodies are streamed from Tokio channels instead of buffering whole turns.
- HTTP/3 concurrent bidirectional and unidirectional stream limits are
  configurable.
- The event bus is per session, so unrelated sessions do not share broadcast
  receivers. Direct turn-stream events are not republished to that bus.
- Active session and event-channel counts are bounded to cap process-local
  memory growth under repeated requests.
- Session lists read compact metadata from SQLite and cap the latest-message
  preview before returning it to clients.

## Current Scope

The server is process-local and uses one bearer token for non-health routes.
Bind plaintext HTTP to loopback, set `RUST_AGENT_SERVER_TOKEN` when scripts need
a stable token, and read the startup readiness line when a supervisor needs the
actual token or OS-assigned ports. Treat generated tokens as a local-development
convenience. Set `RUST_AGENT_SERVER_ALLOW_REMOTE_HTTP=true` only for trusted
deployments where remote plaintext HTTP access is acceptable.
Server-issued session IDs are random and the active registry is process-local,
while session content is durable in SQLite. Clients can restore durable session
rows by id, which re-adds that session id to the process-local active registry
and returns ordered message, function-call, and function-output transcript
records for UI replay. Clients can list durable sessions and fetch individual
session metadata without restoring transcripts. Restored transcripts may include
compaction records alongside messages, function calls, and function outputs.
Clients can explicitly delete sessions, which removes future route access,
deletes durable rows, and closes the session event stream; it does not cancel an
already-running direct turn stream. Use
`POST /sessions/{session_id}/turns:cancel` before deletion when the active turn
should stop. Per-user authorization and multi-process coordination are not
implemented. Use `workspace-write` and `workspace-exec` only for trusted local
deployments because any bearer-token holder can ask the model to edit workspace
files or run local commands. User-initiated terminal commands run through a
separate route and require the current session policy to be `workspace-exec`;
the route itself does not grant or revoke `workspace-exec` for future model
turns.
WebSocket endpoints are not implemented because SSE matches the current
server-to-client event flow with less protocol overhead. Set
`RUST_AGENT_PARENT_PID` only when another local supervisor process should
control server lifetime.
