# Zeus Swift Agent Guide

This directory contains the macOS Swift frontend for Zeus. It owns process
supervision for `rust-agent`, the HTTP/SSE client, SwiftUI chat state, keyboard
navigation, and reusable contract/parsing logic in `ZeusCore`.

## Start With Docs

Before changing frontend behavior, read the relevant docs:

- `docs/architecture/APP-LIFECYCLE.md`
- `docs/architecture/SERVER-SUPERVISION.md`
- `docs/architecture/API-CLIENT.md`
- `docs/architecture/CHAT-STATE.md`
- `docs/architecture/TERMINAL-UI.md`
- `docs/architecture/ZEUSCORE.md`
- `docs/architecture/CONTRACT.md`
- `docs/architecture/SECURITY.md`
- `docs/testing.md`
- `docs/shortcuts.md`
- `docs/packaging.md`

Root `AGENTS.md` still applies. Backend contract changes also require the Rust
docs and contract fixture workflow described at the repository root.

## Structure

- `Sources/Zeus/` contains app, process, network, auth, and SwiftUI code.
- `Sources/ZeusCore/` contains reusable pure Swift contract, event, markdown,
  prompt-history, path-display, and tool-display logic.
- `Tests/ZeusCheckSuite/` contains reusable checks.
- `Tests/ZeusTests/` exposes the check suite through Swift Testing.
- `Tests/ZeusChecks/` is a command-line check runner.

Keep UI-specific behavior in `Zeus`. Keep backend contract types, event
decoding, and pure formatting/parsing logic in `ZeusCore`.

## Development Rules

- Keep UI state mutations on `@MainActor`, usually in `ChatViewModel`.
- Keep `AgentDependencies.swift` protocols small and test-oriented.
- Do not make the frontend infer undocumented backend behavior. Add contract
  data or capabilities in `rust-agent` when behavior crosses the API boundary.
- Route workspace branch changes, terminal commands, model changes, permission
  changes, restore, cancellation, and compaction through the backend API.
- Keep keyboard and footer behavior consistent with `docs/shortcuts.md`.
- Update docs when changing lifecycle, routes, events, state transitions,
  shortcuts, packaging, security assumptions, or validation expectations.

## Validation

Use the narrowest relevant checks:

```bash
cd /Users/ajc/zeus
swift run zeus-checks
```

For contract or backend integration changes, also regenerate the Rust contract
fixture and run the backend checks described in the root guide.
