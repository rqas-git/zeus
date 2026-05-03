# Agent Loop Architecture

The agent loop is the in-memory conversation runner used by the terminal
harness. It keeps terminal I/O, model transport, and session state separate.

## Flow

1. `main` creates one `AgentLoop` for the terminal session.
2. Each user message is submitted with a model-streaming closure.
3. The loop appends the user message, marks the session `Running`, builds the
   conversation history, streams the model response, emits text deltas, stores
   the assistant message, then returns to `Idle`.
4. Interactive mode reuses the same loop, so history lasts only for the current
   terminal session.

## Responsibilities

- `AgentLoop` enforces ordered turns, status transitions, and event emission.
- `InMemorySessionStore` stores messages, message ids, and current status.
- `ChatGptClient` sends full history to the Codex backend and parses SSE output.
- `main.rs` handles terminal behavior by printing `TextDelta` events.

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
context compaction are intentionally out of scope until product behavior requires
them.
