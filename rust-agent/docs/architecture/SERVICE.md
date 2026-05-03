# Service Layer Architecture

`AgentService` is the backend-facing boundary around the agent loop. It owns one
long-lived model client and a session map, so future HTTP or streaming endpoints
can submit work without rebuilding transport state for every request.

## Flow

1. Startup creates `AuthManager` and `ChatGptClient` once, then passes the
   client to `AgentService`.
2. Each request supplies a `SessionId` and user message or session model update.
3. `AgentService` validates model updates against `ModelConfig`.
4. `AgentService` finds or creates the matching session handle.
5. The service locks only that session while a turn is running.
6. The session loop streams the selected model response, executes any requested
   built-in tools, and emits `AgentEvent`s, including tool lifecycle events and
   the completed assistant message.
7. The caller decides how to translate events into terminal output, SSE, or
   WebSocket messages.

## Responsibilities

- `AgentService` owns the warm model client.
- Auth is handled inside the model client and stays outside session state.
- `AgentService` owns in-memory session lookup by `SessionId`.
- `AgentService` enforces the configured model allowlist.
- `AgentService` serializes work per session without serializing unrelated
  sessions.
- `AgentLoop` still owns per-session ordering, tool execution, and message
  history.
- Callers own event delivery and request/response framing.

## Performance Notes

- The model client is reused across sessions.
- Session state is reused across turns.
- The session map lock is held only while finding or creating a session handle.
- Model streaming and tool execution hold an async mutex for the selected
  session only, so different sessions can stream and execute tools concurrently.
- Session history is pruned by each session loop according to the configured
  history limits.
- Session model changes do not rebuild the HTTP client.
- The service avoids frontend assumptions; event sinks stay caller-provided.

## Concurrency

`submit_user_message` takes `&self`, so callers can wrap one service in `Arc` and
share it across request handlers. Concurrent submissions for different
`SessionId`s use separate session locks and can overlap model I/O. Concurrent
submissions for the same `SessionId` wait on that session's async lock, preserving
ordered conversation turns and history updates.

Session model updates are intentionally not queued behind an active turn. If the
target session is busy, `set_session_model` returns an error so a model change
cannot silently apply after a user message that was already submitted.

## Current Scope

The session map is process-local and unbounded, but each session's retained
message history is bounded. Session eviction, persistence, and cross-process
coordination should be added before multi-tenant production use.
