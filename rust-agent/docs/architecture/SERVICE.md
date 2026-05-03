# Service Layer Architecture

`AgentService` is the backend-facing boundary around the agent loop. It owns one
long-lived model client and a session map, so future HTTP or streaming endpoints
can submit work without rebuilding transport state for every request.

## Flow

1. Startup creates `ChatGptClient` once and passes it to `AgentService`.
2. Each request supplies a `SessionId` and user message.
3. `AgentService` finds or creates the matching `AgentLoop`.
4. The session loop streams the model response and emits `AgentEvent`s.
5. The caller decides how to translate events into terminal output, SSE, or
   WebSocket messages.

## Responsibilities

- `AgentService` owns the warm model client.
- `AgentService` owns in-memory session lookup by `SessionId`.
- `AgentLoop` still owns per-session ordering and message history.
- Callers own event delivery and request/response framing.

## Performance Notes

- The model client is reused across sessions.
- Session state is reused across turns.
- The service avoids frontend assumptions; event sinks stay caller-provided.

## Current Scope

The session map is process-local and unbounded. Eviction, persistence, and
cross-process coordination should be added before multi-tenant production use.
