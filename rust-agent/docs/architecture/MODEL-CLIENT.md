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
9. Completed response metadata is parsed from the terminal SSE event only when
   cache telemetry is enabled for the client.
10. Assistant text, tool calls, and cache-health telemetry are returned to the
   session loop.

## Responsibilities

- `ClientConfig` supplies endpoint, instructions, headers, and timeout.
- `AgentService` supplies the selected model for each request.
- `AuthManager` owns rust-agent auth storage, device-code login, refresh, logout,
  and short-lived credentials for model calls.
- `ChatGptClient` adds bearer and `ChatGPT-Account-ID` headers from fresh
  credentials for each backend request.
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
- Prompt-cache keys are stable per configured namespace, session, and model.
  Cache-prefix telemetry includes the stable tool-spec shape so tool changes do
  not look like normal cache reuse.
- Cache-health telemetry records prompt-cache key, stable-prefix hash,
  retained-message shape, response id, and provider token counters.

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
compaction, provider-specific tool repair, or hard byte caps on backend SSE
streams. The only transport retry is the targeted one-shot auth refresh after a
`401 Unauthorized` response. Add stream caps before using this client with
untrusted providers.
