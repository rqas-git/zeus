# App Lifecycle Architecture

Zeus is a SwiftUI app that starts one `rust-agent serve` process per chat
window and talks to it over the local HTTP compatibility API.

## Flow

1. `ZeusApp` creates a `ChatWindowScene`.
2. The scene owns an Observation-backed `ChatViewModel` in SwiftUI `@State`.
3. `ChatViewModel.start()` appends startup status, asks `RustAgentServer` to
   launch the backend for the selected workspace, and receives an
   `AgentAPIClient`.
4. The view model loads models, tool permissions, and workspace metadata.
5. The view model creates a backend session and starts the passive session event
   stream.
6. The UI becomes ready after the session exists and the event stream is
   connected.
7. User prompts stream through `POST /sessions/{id}/turns:stream`.
8. Window teardown cancels Swift tasks, cancels login if active, and stops the
   owned backend process.

## Responsibilities

- `ZeusApp` owns app and window setup only.
- `ChatWindow` renders state and forwards user actions.
- `ChatViewModel` owns frontend state, tasks, and backend session lifecycle.
- `RustAgentServer` owns backend process lifetime.
- `AgentAPIClient` owns HTTP requests and SSE parsing.
- `RustAgentAuth` owns login command execution.

## Current Scope

Each window creates a fresh server process and session. Session content is
durable in the backend SQLite database, but the active frontend state is
process-local to the window. Shared multi-window session coordination is not
implemented.
