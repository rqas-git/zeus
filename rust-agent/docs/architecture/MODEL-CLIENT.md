# Model Client Architecture

`ChatGptClient` is the async transport for ChatGPT Codex Responses calls. It
implements `ModelStreamer`, which keeps the agent loop independent from a
specific backend provider.

## Flow

1. `ChatGptClient::new` builds one reusable async `reqwest::Client`.
2. `stream_conversation` receives a borrowed prompt window, `SessionId`, and
   selected model.
3. The request is serialized from typed borrowed structs.
4. The response body is parsed as SSE.
5. Assistant text deltas are forwarded immediately through the callback.
6. Completed response metadata is parsed from the terminal SSE event.
7. Assistant text and cache-health telemetry are returned to the session loop.

## Responsibilities

- `ClientConfig` supplies endpoint, instructions, headers, and timeout.
- `AgentService` supplies the selected model for each request.
- `ChatGptClient` authenticates with Codex OAuth credentials.
- The typed request structs shape Responses API payloads.
- `AssistantText` accumulates streamed text, handles fallback completed items,
  and captures response id plus token usage when the backend reports it.

## Performance Notes

- Async HTTP avoids blocking backend request workers.
- Typed request serialization avoids constructing a generic JSON tree first.
- SSE event parsing borrows event fields and raw nested payloads where possible.
- Prompt-cache keys are stable per configured namespace, session, and model.
- Cache-health telemetry records prompt-cache key, stable-prefix hash,
  retained-message shape, response id, and provider token counters.

## Current Scope

The client does not yet support request cancellation, retries, websocket
transport, provider failover, or remote compaction.
