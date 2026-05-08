# Terminal Harness Architecture

`main.rs` is a thin CLI adapter around the backend-oriented service. It also
dispatches server mode, while terminal formatting stays local to the CLI path.

## Flow

1. Startup parses the CLI command.
2. `login`, `login status`, and `logout` run directly against `AuthManager`.
3. Chat and server commands load config and auth, then create one
   `AgentService<ChatGptClient>`.
4. One-shot mode submits the CLI prompt to session `1` with the configured tool
   policy and leaves FFF indexing lazy unless the model actually calls a search
   tool.
5. Interactive mode initializes the FFF scanner in the background, then reuses
   session `1` until the user enters a blank line.
6. `/model` shows or changes the session model through `AgentService`.
7. `/models` lists the backend allowlist.
8. `/compact` manually compacts the active session; text after the command is
   passed as summary focus instructions.
9. `TextDelta` events are buffered and written to stdout.
10. When `RUST_AGENT_CACHE_HEALTH=1`, cache-health events are printed to stderr.

## Responsibilities

- `main.rs` owns terminal prompts and stdout formatting.
- Login commands own terminal auth output and do not build the model service.
- Interactive commands stay local to the harness and call service methods.
- Manual compaction prints the token estimate and first retained message id.
- `DeltaWriter` batches assistant text deltas.
- Cache-health telemetry is opt-in and stays off stdout.
- Tool mode is loaded once at startup; `workspace-write` exposes `apply_patch`
  to both one-shot and interactive terminal sessions. `workspace-exec` also
  exposes bounded shell command execution.
- `AgentService` owns session state and model execution.
- `ChatGptClient` owns remote model I/O.

## Performance Notes

- Terminal flushing is byte and interval bounded.
- The interactive prompt keeps one warm service and session.
- Interactive startup initializes FFF on a blocking worker without waiting
  before showing the prompt. A search tool call is the only path that waits for
  scanning if it is still running.
- One-shot prompt mode does not prewarm FFF, so prompts that do not need search
  avoid the index initialization cost.
- Shell command execution is opt-in through `workspace-exec`; `exec_command`
  currently permits any bash command string. Command-level restrictions are
  intentionally deferred, so this mode is for trusted local sessions only.
- Model changes reuse the same service and transport client.
- The binary uses Tokio's multi-threaded runtime so server mode can run
  concurrent listeners and request tasks.
- Cache-health event details are cloned for terminal output only when telemetry
  is enabled.
- The CLI path does not introduce terminal behavior into service internals.

## Current Scope

The terminal harness is single-session and single-process. Endpoint-specific
stream protocols live in `server.rs`.
