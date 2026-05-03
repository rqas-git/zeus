# Terminal Harness Architecture

`main.rs` is a thin CLI adapter around the backend-oriented service. It exists
to exercise the same service path that future endpoints should use.

## Flow

1. Startup parses the CLI command.
2. `login`, `login status`, and `logout` run directly against `AuthManager`.
3. Chat commands load config and auth, then create one `AgentService<ChatGptClient>`.
4. One-shot mode submits the CLI prompt to session `1`.
5. Interactive mode reuses session `1` until the user enters a blank line.
6. `/model` shows or changes the session model through `AgentService`.
7. `/models` lists the backend allowlist.
8. `TextDelta` events are buffered and written to stdout.
9. When `RUST_AGENT_CACHE_HEALTH=1`, cache-health events are printed to stderr.

## Responsibilities

- `main.rs` owns terminal prompts and stdout formatting.
- Login commands own terminal auth output and do not build the model service.
- Interactive commands stay local to the harness and call service methods.
- `DeltaWriter` batches assistant text deltas.
- Cache-health telemetry is opt-in and stays off stdout.
- `AgentService` owns session state and model execution.
- `ChatGptClient` owns remote model I/O.

## Performance Notes

- Terminal flushing is byte and interval bounded.
- The interactive prompt keeps one warm service and session.
- Model changes reuse the same service and transport client.
- The CLI uses Tokio's current-thread runtime because it drives one terminal
  session at a time.
- Cache-health event details are cloned for terminal output only when telemetry
  is enabled.
- The CLI path does not introduce terminal behavior into service internals.

## Current Scope

The terminal harness is single-session and single-process. Endpoint-specific
stream protocols should be implemented outside `main.rs`.
