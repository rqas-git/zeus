# Server Supervision Architecture

`RustAgentServer` launches and validates a local `rust-agent serve` child
process before the UI creates a session.

## Flow

1. `ChatViewModel` passes the selected workspace URL to `RustAgentServer`.
2. The server canonicalizes the workspace path and generates a per-launch bearer
   token.
3. `RustAgentLocator` selects a bundled `rust-agent`, a fresh debug binary, or
   `cargo run --manifest-path rust-agent/Cargo.toml -- serve`.
4. Zeus launches the backend with loopback port `0` for HTTP and HTTP/3.
5. stdout and stderr are scanned for the `server_ready` JSON line.
6. Readiness must report `rust-agent`, protocol version `2`, the generated
   token, and the expected canonical workspace.
7. Zeus polls `/healthz`, then validates `/` and `/capabilities`.
8. Startup succeeds only when required features are present:
   `turn_streaming`, `session_events`, and `terminal_command`.

## Environment

Zeus sets these values for the child process:

- `RUST_AGENT_SERVER_TOKEN`
- `RUST_AGENT_SERVER_HTTP_ADDR=127.0.0.1:0`
- `RUST_AGENT_SERVER_H3_ADDR=127.0.0.1:0`
- `RUST_AGENT_CACHE_HEALTH=1`
- `RUST_AGENT_PARENT_PID`
- `RUST_AGENT_WORKSPACE`

`RUST_AGENT_ROOT` can override backend discovery during development.

## Failure Handling

Startup fails on invalid readiness, token mismatch, protocol mismatch, missing
features, workspace mismatch, early child exit, or readiness timeout. Error
messages include the backend output tail to make launch failures diagnosable.

## Current Scope

The Swift client uses the HTTP compatibility endpoint. HTTP/3 is launched and
validated through readiness/capabilities, but not used by the current client.
