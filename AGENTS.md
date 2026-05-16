# Repository Agent Guide

This repository is a two-application workspace:

- `rust-agent/` is the Rust backend. It owns ChatGPT authentication, the agent
  loop, SQLite session persistence, workspace tools, the local HTTP/H3 server,
  and the Zeus API contract.
- `zeus-swift/` is the macOS Swift frontend. It launches `rust-agent serve`,
  validates the server readiness/capabilities contract, streams SSE events, and
  renders the terminal-style chat UI.

The repository root has a SwiftPM manifest for running the Zeus frontend from
the combined checkout. Backend commands still run from `rust-agent/`. Some
inherited docs still show the old standalone checkout paths such as
`/Users/ajc/rust-agent` or `/Users/ajc/zeus-swift`; in this combined checkout,
translate those to `rust-agent/` and `zeus-swift/` under the repository root.

## Start With Docs

Read the existing docs before changing code:

- Backend working rules: `rust-agent/AGENTS.md`
- Backend Rust style: `rust-agent/RUST-GUIDELINES.md`
- Backend architecture:
  - `rust-agent/docs/architecture/AGENT-LOOP.md`
  - `rust-agent/docs/architecture/SERVICE.md`
  - `rust-agent/docs/architecture/SERVER.md`
  - `rust-agent/docs/architecture/TOOLS.md`
  - `rust-agent/docs/architecture/CONFIGURATION.md`
  - `rust-agent/docs/architecture/MODEL-CLIENT.md`
  - `rust-agent/docs/architecture/CONTEXT-WINDOW.md`
  - `rust-agent/docs/architecture/TERMINAL-HARNESS.md`
  - `rust-agent/docs/architecture/SECURITY.md`
- Frontend working rules: `zeus-swift/AGENTS.md`
- Frontend architecture and docs:
  - `zeus-swift/docs/architecture/APP-LIFECYCLE.md`
  - `zeus-swift/docs/architecture/SERVER-SUPERVISION.md`
  - `zeus-swift/docs/architecture/API-CLIENT.md`
  - `zeus-swift/docs/architecture/CHAT-STATE.md`
  - `zeus-swift/docs/architecture/TERMINAL-UI.md`
  - `zeus-swift/docs/architecture/ZEUSCORE.md`
  - `zeus-swift/docs/architecture/CONTRACT.md`
  - `zeus-swift/docs/architecture/SECURITY.md`
  - `zeus-swift/docs/testing.md`
  - `zeus-swift/docs/shortcuts.md`
  - `zeus-swift/docs/packaging.md`

Nested instructions apply. When working in `rust-agent/`, follow
`rust-agent/AGENTS.md` in addition to this file. When working in
`zeus-swift/`, follow `zeus-swift/AGENTS.md` in addition to this file.

## Backend Structure

`rust-agent/src/main.rs` is the CLI entry point. It dispatches one-shot chat,
interactive chat, `serve`, `contract`, `login`, and `logout` commands.

Key backend modules:

- `config.rs`: environment-based startup config for model, context, compaction,
  storage, telemetry, server, and tool policy.
- `auth.rs`: ChatGPT device login, token refresh, logout, and private auth file
  storage.
- `client.rs`: async ChatGPT Codex Responses transport, SSE parsing, model tool
  calls, cache-health telemetry, and compaction summary requests.
- `agent_loop.rs`: ordered per-session turn execution, prompt window building,
  tool-call rounds, event emission, cancellation, and status transitions.
- `service.rs`: backend-facing service boundary with the shared model client,
  session map, SQLite-backed session restoration, model validation, and
  cancellation.
- `storage.rs`: SQLite session and transcript persistence.
- `server.rs`: Axum HTTP/1.1 and HTTP/2 compatibility server, native HTTP/3
  server, bearer auth, routes, SSE event encoding, readiness output, and the
  generated Zeus API contract fixture.
- `tools.rs`: workspace-confined model tools, FFF search integration,
  `apply_patch`, and trusted-local `exec_command`.
- `workspace.rs`: Git workspace metadata and branch switching.
- `compaction.rs`: semantic transcript compaction and file-operation summaries.
- `test_http.rs` and `bench_support.rs`: test-only support.

Backend docs are intentionally detailed. Update them when behavior,
configuration, routes, events, tool policy, storage, or operational expectations
change.

## Frontend Structure

`Package.swift` at the repository root and `zeus-swift/Package.swift` define
Swift 6.2 packages with Swift language mode 5 targets:

- `ZeusCore`: shared, testable model and parsing code.
- `Zeus`: the macOS SwiftUI executable.
- `ZeusCheckSuite`: reusable pure Swift checks.
- `ZeusTests`: Swift Testing wrapper around the check suite.
- `ZeusChecks`: executable check runner for CI-style validation.

Key frontend files:

- `zeus-swift/Sources/Zeus/ZeusApp.swift`: app entry point and window creation.
- `zeus-swift/Sources/Zeus/ChatWindow.swift`: SwiftUI terminal UI, footer controls,
  shortcuts, transcript, search, and input handling.
- `zeus-swift/Sources/Zeus/ChatViewModel.swift`: main UI state machine for server
  startup, sessions, streaming, login, model/effort/permission selection,
  terminal passthrough, restore, branch switching, search, and cache stats.
- `zeus-swift/Sources/Zeus/AgentAPIClient.swift`: HTTP client and SSE parser for the
  rust-agent server.
- `zeus-swift/Sources/Zeus/RustAgentServer.swift`: launches `rust-agent serve`,
  injects server environment, reads readiness JSON, validates
  identity/capabilities, and owns backend process lifetime.
- `zeus-swift/Sources/Zeus/RustAgentLocator.swift`: finds a bundled, debug, or
  Cargo-run backend binary.
- `zeus-swift/Sources/Zeus/RustAgentAuth.swift`: runs backend login commands and
  reports auth status.
- `zeus-swift/Sources/Zeus/AgentDependencies.swift`: protocols used to isolate
  the UI from concrete server/client/auth implementations.
- `zeus-swift/Sources/Zeus/PromptTextField.swift`: AppKit-backed prompt input.
- `zeus-swift/Sources/Zeus/TerminalMarkdownView.swift`: SwiftUI rendering for
  parsed terminal Markdown.
- `zeus-swift/Sources/Zeus/TerminalPalette.swift`: shared terminal colors.
- `zeus-swift/Sources/ZeusCore/AgentAPIContract.swift`: Swift request/response
  types for the backend contract.
- `zeus-swift/Sources/ZeusCore/AgentServerEvent.swift`: typed decoding for
  server SSE events.
- `zeus-swift/Sources/ZeusCore/ToolMetadata.swift`: display metadata and
  argument summaries for tool calls.
- `zeus-swift/Sources/ZeusCore/TerminalMarkdownParser.swift`: lightweight
  transcript Markdown parser.
- `zeus-swift/Sources/ZeusCore/PromptHistory.swift`: shell-like submitted
  message navigation.
- `zeus-swift/Sources/ZeusCore/PathDisplay.swift`: local path display helpers.

Keep UI-specific behavior in `Zeus` and reusable parsing/contract logic in
`ZeusCore`.

## Backend And Frontend Contract

The contract boundary is the `rust-agent` server API.

- Rust owns the canonical contract fixture via `cargo run -- contract`.
- The checked-in fixture is `rust-agent/docs/contracts/zeus-api-contract.json`.
- Swift decoders and checks in `zeus-swift/Sources/ZeusCore` and
  `zeus-swift/Tests/ZeusCheckSuite` must stay compatible with that fixture.

When changing a route, request, response, readiness line, capability, feature
flag, tool policy, transcript record, or SSE event:

1. Update the Rust route/event/types and the contract fixture generation in
   `rust-agent/src/server.rs`.
2. Regenerate `rust-agent/docs/contracts/zeus-api-contract.json`.
3. Update Swift contract types in `zeus-swift/Sources/ZeusCore/`.
4. Update `AgentAPIClient`, `AgentDependencies`, and `ChatViewModel` if the UI
   needs to call or render the changed behavior.
5. Update `zeus-swift/Tests/ZeusCheckSuite/APIContractChecks.swift`.
6. Run both backend and frontend checks.

Do not make the frontend infer undocumented server behavior. Add explicit
contract data or capabilities instead.

## Integration Rules

Zeus launches the backend as an owned child process:

- It sets `RUST_AGENT_SERVER_TOKEN` to a per-launch token.
- It sets `RUST_AGENT_SERVER_HTTP_ADDR=127.0.0.1:0` and
  `RUST_AGENT_SERVER_H3_ADDR=127.0.0.1:0`.
- It sets `RUST_AGENT_WORKSPACE` to the selected workspace and verifies the
  backend reports the same canonical path.
- It sets `RUST_AGENT_PARENT_PID` so the backend can exit with the supervisor.
- It expects protocol version `2` and required features including
  `turn_streaming`, `session_events`, and `terminal_command`.

The Swift client currently uses the HTTP compatibility endpoint and SSE. Keep
endpoint behavior identical across HTTP compatibility and HTTP/3 transports.

Workspace branch switches must go through the backend workspace API so backend
search indexes and frontend workspace state stay aligned. Terminal passthrough
must go through `POST /sessions/{session_id}/terminal:run`; it records command
output in the transcript but does not change model tool permissions.

## Security And Tool Policy

The backend default tool policy is `read-only`.

- `read-only` exposes file read/list/search tools.
- `workspace-write` adds `apply_patch`.
- `workspace-exec` adds bounded shell execution and is trusted-local only.

Do not expose write or exec capability by default. Treat bearer-token holders as
fully authorized for the active local server. Plaintext HTTP should stay bound
to loopback unless a trusted deployment explicitly sets
`RUST_AGENT_SERVER_ALLOW_REMOTE_HTTP=true`.

`cargo audit` uses `rust-agent/.cargo/audit.toml`. The current ignored advisory
is documented in `rust-agent/docs/architecture/SECURITY.md`.

## Common Commands

Backend:

```bash
cd rust-agent
cargo fmt
cargo test
cargo audit
cargo run -- "Say hello in one sentence"
RUST_AGENT_SERVER_TOKEN=dev-token cargo run -- serve
cargo run -- contract > docs/contracts/zeus-api-contract.json
```

Run ignored release benchmarks only when touching performance-sensitive paths:

```bash
cd rust-agent
cargo test --release -- --ignored --nocapture
```

Frontend:

```bash
cd /Users/ajc/zeus
swift build
swift run zeus-checks
swift run zeus
```

Package a local DMG that embeds a release `rust-agent` binary:

```bash
cd /Users/ajc/zeus
zeus-swift/scripts/package-release.sh
```

For frontend manual runs against a nonstandard backend checkout, set
`RUST_AGENT_ROOT` before launching Zeus. For runtime workspace selection, use
`ZEUS_WORKSPACE` when packaging or choose the workspace passed to
`RustAgentServer.start(workspaceURL:)` in code/tests.

## Change Guidance

Keep changes scoped to the affected application unless the backend/frontend
contract requires coordinated edits. For cross-application work, update the Rust
contract, Swift decoders/client code, UI behavior, docs, and checks together.

Always create small, incremental commits while making changes. Keep each commit
atomic and independently understandable; avoid mixing unrelated behavior,
documentation, formatting, and cleanup in the same commit.

Prefer existing module boundaries over new abstractions. In Rust, follow
`RUST-GUIDELINES.md`, keep async service boundaries explicit, avoid unbounded
buffers, and preserve per-session ordering. In Swift, keep UI state mutations on
`@MainActor`, keep protocol seams in `AgentDependencies.swift`, and keep
testable pure logic in `ZeusCore`.

Do not commit generated build artifacts such as `.build/`,
`rust-agent/target/`, `zeus-swift/.build/`, or `zeus-swift/dist/`. The contract
JSON is a checked-in artifact and should be updated when its Rust generator
changes.

Before finishing a change, run the narrowest relevant checks. For contract or
integration changes, run both `cargo test` in `rust-agent/` and
`swift run zeus-checks` from the repository root.
