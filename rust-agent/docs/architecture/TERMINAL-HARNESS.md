# Terminal Harness Architecture

`main.rs` is a thin CLI adapter around the backend-oriented service. It exists
to exercise the same service path that future endpoints should use.

## Flow

1. Startup loads config and auth.
2. A single `AgentService<ChatGptClient>` is created.
3. One-shot mode submits the CLI prompt to session `1`.
4. Interactive mode reuses session `1` until the user enters a blank line.
5. `/model` shows or changes the session model through `AgentService`.
6. `/models` lists the backend allowlist.
7. `TextDelta` events are buffered and written to stdout.

## Responsibilities

- `main.rs` owns terminal prompts and stdout formatting.
- Interactive commands stay local to the harness and call service methods.
- `DeltaWriter` batches assistant text deltas.
- `AgentService` owns session state and model execution.
- `ChatGptClient` owns remote model I/O.

## Performance Notes

- Terminal flushing is byte and interval bounded.
- The interactive prompt keeps one warm service and session.
- Model changes reuse the same service and transport client.
- The CLI path does not introduce terminal behavior into service internals.

## Current Scope

The terminal harness is single-session and single-process. Endpoint-specific
stream protocols should be implemented outside `main.rs`.
