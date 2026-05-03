# Agent Loop Architecture

The agent loop is the per-session conversation runner used by the service and
terminal harness. It keeps terminal I/O, model transport, and session state
separate so the same core can sit behind future HTTP or streaming endpoints.

## Flow

1. `main` loads `AppConfig`, creates one async `ChatGptClient`, and wraps it in
   an `AgentService`.
2. `AgentService` keeps a map of warm `AgentLoop`s by `SessionId`.
3. Each user message is submitted to a session. Missing sessions are created
   with the configured context-window bounds.
4. The loop appends the user message, marks the session `Running`, builds a
   bounded borrowed prompt window, streams the model response, emits text
   deltas, stores the assistant message, then returns to `Idle`.
5. Interactive mode reuses the same service and session, so history lasts for
   the current terminal process.

## Responsibilities

- `AgentLoop` enforces ordered turns, status transitions, and event emission.
- `InMemorySessionStore` stores messages, message ids, and current status. It
  creates borrowed prompt windows instead of cloning message text.
- `AgentService` owns the long-lived model client and session map expected by a
  backend service.
- `ChatGptClient` sends typed async Responses requests to the Codex backend and
  parses SSE output.
- `main.rs` handles terminal behavior by buffering `TextDelta` events before
  flushing stdout.

## Performance Defaults

- Model, instructions, endpoint, timeout, context-window, and delta-flush
  settings are environment-configurable.
- Prompt payloads keep the latest messages within configurable message and byte
  budgets. The latest message is always retained.
- Prompt request bodies serialize from typed borrowed structures rather than
  first building a generic JSON value.
- Streaming uses async HTTP and SSE parsing, so future endpoints do not need to
  block request workers on model I/O.
- Prompt-cache keys are stable per service/session namespace.

## Events

`AgentEvent` reports status changes, streamed assistant text, completed messages,
and errors. This gives a future HTTP, WebSocket, or SSE frontend a clear boundary
without coupling it to terminal output.

## Error Handling

The loop rejects new submissions while a session is `Running`. If publishing the
`Running` status fails, the loop rolls back to `Idle` before returning the error
so the session is not left stuck. Model failures mark the session `Failed` and
emit an error event.

## Current Scope

Conversation history is in memory only. Persistence, tools, cancellation, and
semantic context compaction are intentionally out of scope until product behavior
requires them.
