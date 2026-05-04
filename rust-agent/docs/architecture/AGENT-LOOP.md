# Agent Loop Architecture

The agent loop is the per-session conversation runner used by the service,
terminal harness, and HTTP server. It keeps terminal I/O, network framing, model
transport, and session state separate.

## Flow

1. `main` loads `AppConfig`, creates one async `ChatGptClient`, and wraps it in
   an `AgentService`.
2. `AgentService` keeps a map of warm session handles by `SessionId`.
3. Each user message is submitted to a session. Missing sessions are created
   with the configured context-window bounds and default model.
4. The service locks the selected session so unrelated sessions can run
   concurrently while same-session turns remain ordered.
5. The loop appends the user message, marks the session `Running`, prunes stored
   history, builds a bounded borrowed prompt window, streams the selected model
   response, emits text deltas and cache telemetry, stores assistant text, stores
   any completed tool-call items, executes those tools, stores tool outputs, and
   repeats until the model returns no more tool calls.
6. Tool calls are bounded by a fixed per-turn round limit so a malformed or
   looping model response cannot run forever.
7. Interactive mode reuses the same service and session, so history lasts for
   the current terminal process.

## Responsibilities

- `AgentLoop` enforces ordered turns, status transitions, and event emission.
- `InMemorySessionStore` stores retained messages, message ids, current status,
  and session settings. It stores user/assistant messages, function-call items,
  and function-call outputs, then creates borrowed prompt windows instead of
  cloning transcript text.
- `ToolRegistry` owns the built-in tools, permission policy, and model-visible
  tool specs. It keeps exact filesystem tools (`read_file`, `read_file_range`,
  `list_dir`) alongside a shared FFF-backed index for fuzzy file/path search and
  content search. In `workspace-write` mode it also exposes `apply_patch` for
  workspace-confined UTF-8 file edits. FFF index initialization is lazy by
  default, but callers can start the shared scanner on a background blocking
  worker before any tool request. In `workspace-exec` mode it also exposes a
  bounded shell command tool plus dedicated git wrappers. It executes tool
  batches in parallel when every requested tool is marked parallel-safe.
- `AgentService` owns the long-lived model client and session map expected by a
  backend service. It also validates model changes before updating a session.
- `AgentService` holds only a per-session async lock while a turn streams, so
  different sessions can progress independently.
- `ChatGptClient` sends typed async Responses requests to the Codex backend and
  parses SSE output.
- `main.rs` handles terminal behavior by buffering `TextDelta` events before
  flushing stdout.

## Performance Defaults

- Default model, model allowlist, instructions, endpoint, timeout,
  context-window, and delta-flush settings are environment-configurable.
- Prompt payloads keep the latest messages within configurable message and byte
  budgets. Consecutive function-call and function-output transcript items are
  retained or dropped as one unit so follow-up requests do not contain orphaned
  tool outputs. The latest message or tool transcript unit is always retained.
- Stored session history is retained within configurable message and byte
  budgets.
- Prompt request bodies serialize from typed borrowed structures rather than
  first building a generic JSON value.
- Tool specs serialize from static typed definitions rather than allocating
  `serde_json::Value` schemas for each request.
- Tool arguments are retained as raw JSON until the tool boundary, where they are
  deserialized once into typed argument structs.
- Read-only tool calls are executed concurrently when they are marked safe, and
  their outputs are replayed to the model in one follow-up request. FFF search
  tools run on blocking workers and are marked sequential within a batch to
  avoid oversubscribing the Tokio and Rayon worker pools. Cross-session FFF
  searches also pass through a process-local semaphore configured by
  `RUST_AGENT_TOOL_SEARCH_CONCURRENCY`. If an FFF tool call arrives while the
  index is still scanning, that blocking worker waits for the scan to finish
  before running the search. `read_file` reads only one byte past its output cap
  before truncating, and paged reads bound each line before building returned
  strings.
- `apply_patch` is sequential, parses bounded patch input, validates all touched
  paths before writing, caps target file size, and replaces individual files via
  temporary-file rename.
- `exec_command` and the git wrappers are sequential, timeout-bounded, and
  return capped stdout and stderr. Oversized command output kills the process
  group and returns a failed tool result. `exec_command` rejects direct git
  executable tokens so repository operations flow through the dedicated wrappers.
- Streaming uses async HTTP and SSE parsing, so request workers do not block on
  model I/O.
- The service-level session locks keep ordered turns local to one session
  instead of serializing all sessions through one mutable service borrow.
- Prompt-cache keys are stable per service/session/model namespace.

## Events

`AgentEvent` reports status changes, streamed assistant text, cache-health
telemetry, tool-call start/completion, completed messages, and errors. The
terminal harness renders a subset to stdout, while the server converts events
into named SSE frames over HTTP compatibility and HTTP/3 transports.

## Error Handling

The loop rejects direct submissions while a session is `Running`; the service
normally prevents this by queuing same-session submissions on the session's async
lock. If publishing the `Running` status fails, the loop rolls back to `Idle`
before returning the error so the session is not left stuck. Model failures mark
the session `Failed` and emit an error event. Session model changes are rejected
while a turn is running.

## Current Scope

Conversation history is recent and in memory only. The default built-in tool set
is read-only (`read_file`, `read_file_range`, `list_dir`, `search_files`, and
`search_text`).
`RUST_AGENT_TOOL_MODE=workspace-write` adds `apply_patch`, and
`RUST_AGENT_TOOL_MODE=workspace-exec` adds trusted local command execution plus
git wrappers. Persistence, cancellation, and semantic context compaction are
intentionally out of scope until product behavior requires them.

## Related Docs

- [Service Layer](SERVICE.md)
- [Model Client](MODEL-CLIENT.md)
- [Configuration](CONFIGURATION.md)
- [Context Window](CONTEXT-WINDOW.md)
- [Terminal Harness](TERMINAL-HARNESS.md)
- [Server](SERVER.md)
- [Tooling](TOOLS.md)
- [Performance Benchmarks](PERFORMANCE-BENCHMARKS.md)
