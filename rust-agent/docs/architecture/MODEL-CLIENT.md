# Model Client Architecture

`ChatGptClient` is the async transport for ChatGPT Codex Responses calls. It
implements `ModelStreamer`, which keeps the agent loop independent from a
specific backend provider.

## Flow

1. `ChatGptClient::new` builds one reusable async `reqwest::Client`.
2. `stream_conversation` receives a borrowed prompt window, model-visible tool
   specs, `SessionId`, and selected model.
3. `AuthManager` returns fresh ChatGPT credentials, refreshing stored tokens
   when the access token is expired, near expiry, or due for rotation.
4. The request is serialized from typed borrowed structs, including prior
   `function_call` and `function_call_output` items.
5. If the backend returns `401 Unauthorized`, the client refreshes credentials
   once and rebuilds the same typed request for a single retry.
6. The response body is parsed as SSE with a small chunk parser.
7. Assistant text deltas are forwarded immediately through the callback.
8. Completed `function_call` output items are captured as raw-argument tool
   calls for the session loop to execute.
9. Compaction summary requests use the same transport with a summarization
   system prompt, no tools, and no streamed user-visible deltas.
10. Completed response metadata is always parsed from the terminal SSE event so
   server clients can render provider token usage; terminal rendering of the
   same cache telemetry remains opt-in.
11. Assistant text, tool calls, and cache-health telemetry are returned to the
   session loop.

## Responsibilities

- `ClientConfig` supplies endpoint, instructions, headers, and the stream idle
  timeout.
- `AgentService` supplies the selected model for each request.
- `AuthManager` owns rust-agent auth storage, device-code login, refresh, logout,
  and short-lived credentials for model calls.
- `ChatGptClient` adds bearer and `ChatGPT-Account-ID` headers from fresh
  credentials for each backend request. It also sends the prompt-cache key as
  `session_id` and `x-client-request-id` so the Codex backend can keep repeated
  session prefixes on the same cache route.
- During a model-driven turn, `ChatGptClient` captures the backend
  `x-codex-turn-state` response header and replays it on later requests in that
  same turn. The value is not persisted across turns.
- The typed request structs shape Responses API payloads.
- `AssistantText` accumulates streamed text, handles fallback completed items,
  captures completed function calls, and captures response id plus token usage
  when the backend reports it.

## Performance Notes

- Async HTTP avoids blocking backend request workers.
- The long-lived model HTTP client is reused; auth has a separate long-lived
  HTTP client so token requests do not rebuild transport state.
- Access tokens are read fresh per request, while refresh is serialized by a
  small async mutex to avoid concurrent token file rewrites.
- Typed request serialization avoids constructing a generic JSON tree first.
- Tool specs and structured transcript items serialize directly from typed
  borrowed data.
- SSE parsing is local and chunk-based. The line splitter uses `memchr` to find
  newlines across received byte chunks instead of checking every byte in a Rust
  loop.
- SSE event JSON parsing borrows event fields and raw nested payloads where
  possible.
- The SSE parser intentionally does not enforce per-line, per-event, or
  full-response byte caps. Like pi-mono's provider parsers, it assumes the
  configured backend is trusted and relies on context/history limits after
  parsing rather than truncating provider streams mid-response.
- Model streams follow Codex timeout behavior: there is no fixed total request
  timeout, but waiting for the next SSE body chunk is bounded by the configured
  stream idle timeout.
- Prompt-cache keys are stable per configured namespace and session.
  The same key is sent in the Responses body and session-affinity headers.
  Turn-scoped Codex sticky-routing state is replayed only inside the active
  model-driven turn.
  Cache-prefix telemetry includes the stable tool-spec shape and retained input
  prefix hash so tool, compaction, or pruning changes do not look like normal
  cache reuse.
- Cache-health telemetry records prompt-cache key, stable-prefix hash,
  retained input hash, retained-message shape, response id, and provider token
  counters, including cached input and reasoning output tokens when the backend
  reports them. The loop aggregates those counters into one turn-level usage
  event for clients that need per-turn totals.

## Benchmarks

The client has ignored release benchmarks for SSE parsing and typed Responses
request serialization. Run all performance benchmarks with:

```bash
cargo test --release -- --ignored --nocapture
```

See [Performance Benchmarks](PERFORMANCE-BENCHMARKS.md) for the full benchmark
list and individual commands.

## Current Scope

Request cancellation is caller-driven by dropping the streaming future; the
client does not expose provider-side cancellation ids. The client does not yet
support general retries, websocket transport, provider failover, remote
provider-managed compaction, provider-specific tool repair, or hard byte caps on
backend SSE streams. The client also does not apply a total request timeout to
model streams; stalled streams fail only when no SSE chunk arrives within the
configured idle window. The only transport retry is the targeted one-shot auth
refresh after a `401 Unauthorized` response. Add stream caps before using this
client with untrusted providers.
