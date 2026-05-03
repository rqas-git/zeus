# Model Client Architecture

`ChatGptClient` is the async transport for ChatGPT Codex Responses calls. It
implements `ModelStreamer`, which keeps the agent loop independent from a
specific backend provider.

## Flow

1. `ChatGptClient::new` builds one reusable async `reqwest::Client`.
2. `stream_conversation` receives a borrowed prompt window, model-visible tool
   specs, `SessionId`, and selected model.
3. The request is serialized from typed borrowed structs, including prior
   `function_call` and `function_call_output` items.
4. The response body is parsed as SSE with a small chunk parser.
5. Assistant text deltas are forwarded immediately through the callback.
6. Completed `function_call` output items are captured as raw-argument tool
   calls for the session loop to execute.
7. Completed response metadata is parsed from the terminal SSE event only when
   cache telemetry is enabled for the client.
8. Assistant text, tool calls, and cache-health telemetry are returned to the
   session loop.

## Responsibilities

- `ClientConfig` supplies endpoint, instructions, headers, and timeout.
- `AgentService` supplies the selected model for each request.
- `ChatGptClient` authenticates with Codex OAuth credentials.
- The typed request structs shape Responses API payloads.
- `AssistantText` accumulates streamed text, handles fallback completed items,
  captures completed function calls, and captures response id plus token usage
  when the backend reports it.

## Performance Notes

- Async HTTP avoids blocking backend request workers.
- Typed request serialization avoids constructing a generic JSON tree first.
- Tool specs and structured transcript items serialize directly from typed
  borrowed data.
- SSE parsing is local and chunk-based. The line splitter uses `memchr` to find
  newlines across received byte chunks instead of checking every byte in a Rust
  loop.
- SSE event JSON parsing borrows event fields and raw nested payloads where
  possible.
- Prompt-cache keys are stable per configured namespace, session, and model.
  Cache-prefix telemetry includes the stable tool-spec shape so tool changes do
  not look like normal cache reuse.
- Cache-health telemetry records prompt-cache key, stable-prefix hash,
  retained-message shape, response id, and provider token counters.

## Benchmark

Run the ignored release benchmark for the SSE parser with:

```bash
cargo test --release client::tests::benchmark_sse_parser_large_stream -- --ignored --nocapture
```

The benchmark parses a synthetic 20,000-event stream in 8 KiB chunks and prints
min, median, max, and throughput statistics.

## Current Scope

The client does not yet support request cancellation, retries, websocket
transport, provider failover, remote compaction, or provider-specific tool
repair.
