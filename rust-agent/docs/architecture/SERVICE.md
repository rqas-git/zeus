# Service Layer Architecture

`AgentService` is the backend-facing boundary around the agent loop. It owns one
long-lived model client, an optional SQLite session database, and a process-local
session map, so terminal and server endpoints can submit work without rebuilding
transport state for every request.

## Flow

1. Startup creates `AuthManager` and `ChatGptClient` once, then passes the
   client to `AgentService`.
2. Each request supplies a `SessionId`, pagination bounds, user message,
   optional reasoning effort, session model update, or session tool-policy
   update.
3. `AgentService` validates model and reasoning-effort updates against
   `ModelConfig`, and applies parsed tool-policy updates to the active session.
4. `AgentService` finds or creates the matching session handle, loading any
   durable SQLite state for that `SessionId`.
5. The service locks only that session while a turn is running.
6. The session loop compacts context when needed, streams the selected model
   response, executes any requested built-in tools, and emits `AgentEvent`s,
   including compaction events, tool lifecycle events, and the completed
   assistant message.
7. Metadata requests read compact SQLite session rows and annotate them with
   process-local active-session state.
8. The caller decides how to translate events into terminal output or server
   stream frames.

## Responsibilities

- `AgentService` owns the warm model client.
- Auth is handled inside the model client and stays outside session state.
- `AgentService` owns in-memory session lookup by `SessionId`.
- `AgentService` creates, loads, and deletes SQLite-backed session state when a
  database is configured.
- `AgentService` lists and loads durable session metadata without restoring full
  transcripts.
- `AgentService` enforces the configured model and reasoning-effort allowlists.
- `AgentService` can request cancellation of the currently running turn without
  waiting on that session's execution lock.
- `AgentService` exposes manual compaction with optional caller-provided summary
  focus instructions.
- `AgentService` exposes per-session tool-policy changes for future turns.
- `AgentService` serializes work per session without serializing unrelated
  sessions.
- `AgentLoop` still owns per-session ordering, tool execution, and message
  history.
- Callers own event delivery and request/response framing.

## Performance Notes

- The model client is reused across sessions.
- Session state is reused across turns and restored from SQLite when a session
  handle is recreated.
- The session map lock is held only while finding or creating a session handle.
- Model streaming and tool execution hold an async mutex for the selected
  session only, so different sessions can stream and execute tools concurrently.
- Session history is pruned by each session loop according to the configured
  history limits.
- Compaction settings are passed into each session loop at creation time, so
  restored and newly created sessions share the same process configuration.
- Session model changes do not rebuild the HTTP client.
- Session metadata reads stay outside per-session execution locks and only use
  the session map to mark whether a durable session is currently active.
- SQLite session loads, metadata reads, deletes, and model/status/message writes
  run on Tokio blocking workers so request workers are not pinned on synchronous
  database calls.
- The service avoids frontend assumptions; event sinks stay caller-provided.
- Event sinks are `Send`, so server handlers can spawn turn work onto Tokio's
  multi-threaded runtime.

## Concurrency

`submit_user_message` takes `&self`, so callers can wrap one service in `Arc` and
share it across request handlers. Concurrent submissions for different
`SessionId`s use separate session locks and can overlap model I/O. Concurrent
submissions for the same `SessionId` wait on that session's async lock, preserving
ordered conversation turns and history updates.

Session model and tool-policy updates are intentionally not queued behind an
active turn. If the target session is busy, `set_session_model` or
`set_session_tool_policy` returns an error so a setting change cannot silently
apply after a user message that was already submitted.
Turn cancellation also targets only the active turn. Queued same-session
submissions wait for the running turn to finish or cancel, then continue in
order.

## Storage

Production startup configures a SQLite database, so sessions and ordered
messages are durable. The database uses WAL mode, `synchronous=NORMAL`, foreign
keys, a busy timeout, and cascade deletion of session messages. `AgentLoop`
continues to keep only bounded recent history in memory; SQLite remains the
canonical store for the full ordered message list, including compaction
checkpoints. Session metadata queries use supporting indexes, are ordered by
`updated_at_ms`, and include counts plus a capped preview of the latest user or
assistant message.

## Current Scope

The active session map and cancellation state are process-local; server mode
bounds the active map with configuration. Explicit server deletion removes a
session from memory and SQLite. Durable session listing is implemented for
frontend navigation. Session model selection is durable, while per-session
tool-policy overrides are process-local to the active session handle. TTL
eviction and cross-process coordination should be added only when endpoint
behavior requires them.
