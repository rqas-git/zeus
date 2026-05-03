# Agent Loop Future-State Plan

## Goal

Build a backend agent loop that can serve terminal, web, or API clients without changing the core execution model. The loop should preserve strict turn ordering, stream updates as they happen, and stay small until the product needs persistence, tools, or multi-session coordination.

## Design Principles

- Follow DRY and YAGNI: keep the first implementation focused on chat turns, then add tools, compaction, persistence, and queues only when the behavior requires them.
- Follow `RUST-GUIDELINES.md`: use strong types for session/message identifiers, keep APIs testable, validate observable behavior, and profile before optimizing speculative hot paths.
- Treat the model stream as the primary performance path. Network/model latency dominates, so stream bytes incrementally instead of buffering complete responses.
- Keep ownership simple. Store messages once, pass borrowed views when serializing requests, and avoid cloning conversation history.
- Prefer concrete async dependencies over broad async traits unless external customization is required.

## Target Architecture

```text
client request
  -> append user message
  -> start or wake one runner for the session
  -> runner builds request from session state
  -> runner streams model events
  -> runner persists assistant/tool events
  -> runner publishes updates
  -> runner continues for tool calls or queued user input
  -> runner idles when no work remains
```

Each session should have exactly one active runner. Additional user input for that session should append to the session log and either wait for the current turn to finish or be consumed by the next loop iteration. This keeps message ordering deterministic and avoids multiple model requests racing over the same history.

## Core Types

- `SessionId`: strong identifier for a conversation.
- `MessageId`: strong identifier for a stored message.
- `ConversationRole`: `User` or `Assistant`.
- `SessionStatus`: `Idle`, `Running`, `Cancelling`, or `Failed`.
- `AgentEvent`: streamed event emitted by the runner, such as text delta, completed message, error, or status change.
- `SessionStore`: concrete storage boundary for loading and appending session state.
- `EventSink`: concrete publisher for terminal, SSE, WebSocket, or test capture output.

Keep these types narrow. Avoid introducing trait-heavy abstractions until there are multiple real implementations.

## Runner Model

Use `tokio` for the future backend service:

```rust
async fn run_session(session_id: SessionId) -> Result<()> {
    loop {
        let history = store.history(session_id).await?;
        let mut stream = model.stream_response(&history).await?;

        while let Some(event) = stream.next().await {
            let event = event?;
            store.apply_event(session_id, &event).await?;
            events.publish(session_id, &event).await?;
        }

        if !store.has_pending_user_input(session_id).await? {
            break;
        }
    }

    Ok(())
}
```

The runner should not poll. It should be started by incoming work and should park when the session has no pending input. A session registry can hold one `JoinHandle` or state guard per active session.

## Streaming

The model client should expose an incremental stream instead of returning only a final `String`.

- Parse SSE incrementally from the response body.
- Emit text deltas immediately.
- Accumulate assistant text once for persistence and conversation history.
- Treat malformed SSE, backend `response.failed`, and premature EOF as errors.
- Keep a fallback path for completed output items in case delta events are missing.

For the current terminal harness, the streaming callback can write directly to stdout. For the backend, the same events should be published to connected clients.

## Message History

Initial implementation can continue resending full history because it is simple and correct. Future implementation should add compaction when either condition is true:

- the serialized request approaches model context limits;
- latency or token cost from full-history resend becomes material.

Compaction should create an explicit summary message rather than mutating historical messages in place. This makes resume, audit, and debugging easier.

## Tool Loop

Tools should be added only after plain chat is stable.

Future tool loop:

1. Send model request with available tools.
2. Persist assistant text and tool calls as streamed events.
3. Execute approved tool calls.
4. Persist tool results.
5. Continue the runner until the assistant reaches a final response.

Tool execution should use explicit permission checks and cancellation boundaries. Tool results must be represented as structured messages rather than string-spliced transcript text.

## Cancellation

The session registry should keep an abort handle for each active runner. Cancellation should:

- stop reading the model stream;
- mark the active assistant message as interrupted;
- leave already persisted deltas intact;
- return the session to `Idle` unless queued input remains.

## Persistence

Start with SQLite or JSONL only when session resume becomes required. Store:

- session metadata;
- ordered messages;
- ordered message parts or stream events;
- compaction summaries;
- current status.

Do not persist provider credentials in session storage.

## Testing Strategy

- Unit test request serialization.
- Unit test SSE parsing with LF, CRLF, multi-line data blocks, failed events, incomplete streams, and chunk boundaries.
- Unit test runner behavior with a fake model stream and fake store.
- Integration test a live one-shot message and a two-turn memory conversation when credentials are available.
- Validate terminal streaming by asserting deltas are printed before final completion where practical.

## Performance Notes

Based on `RUST-GUIDELINES.md`, the likely hot path is stream parsing and message serialization, not terminal input.

- Reuse one HTTP client.
- Avoid repeated full-history clones.
- Avoid repeated string formatting in stream processing.
- Avoid buffering full response bodies.
- Use `mimalloc` for the app allocator.
- Add benchmarks only after the loop becomes performance or cost relevant.
