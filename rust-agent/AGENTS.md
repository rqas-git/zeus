# Running rust-agent

## Development Guidance

When making changes, always validate all implemented functionality before finishing. Run the relevant automated checks and any live or end-to-end validation needed to prove the changed behavior works.

When implementation changes affect behavior, configuration, architecture, commands, or operational expectations, update all relevant documentation before finishing.

Write concise, correct code. Follow DRY and YAGNI principles: avoid duplication, avoid speculative abstractions, and keep the implementation as small as the current behavior allows.

Always create concise, atomic commits for changes made.
Keep commits very small, independent, and informative. Avoid commits that touch many files or combine unrelated behavior, documentation, and cleanup. Break them up incrementally.

Use [RUST-GUIDELINES.md](RUST-GUIDELINES.md) as the Rust design and style guide for this repository.

Run the application from the repository root:

```bash
cd /Users/ajc/rust-agent
cargo run -- "Say hello in one sentence"
```

The application prints the assistant response:

```text
Assistant: Hello.
```

To use the interactive prompt, run:

```bash
cd /Users/ajc/rust-agent
cargo run
```

Then type messages after the `You:` prompt. Session history is stored in SQLite
at `~/.rust-agent/sessions.db` by default and restored for the default terminal
session across restarts. Recent in-memory history is bounded by
`RUST_AGENT_HISTORY_MAX_MESSAGES` and `RUST_AGENT_HISTORY_MAX_BYTES`. Submit a
blank line to exit.

## Server Mode

Run the native HTTP/3 server from the repository root:

```bash
cd /Users/ajc/rust-agent
RUST_AGENT_SERVER_TOKEN=dev-token cargo run -- serve
```

By default this starts:

- HTTP/1.1 and HTTP/2 compatibility on `127.0.0.1:4096`
- Native HTTP/3 over QUIC/TLS on `127.0.0.1:4433`

The HTTP compatibility responses advertise the HTTP/3 endpoint with `Alt-Svc`.
When TLS files are not configured, the HTTP/3 listener generates a self-signed
certificate for local development.
Set `RUST_AGENT_SERVER_TOKEN` for a stable bearer token. If it is unset, server
startup prints a generated bearer token.

Useful smoke checks:

```bash
curl -s http://127.0.0.1:4096/healthz
curl --http3 -k https://127.0.0.1:4433/healthz
SESSION_ID=$(curl -s -X POST \
  -H "authorization: Bearer $RUST_AGENT_SERVER_TOKEN" \
  http://127.0.0.1:4096/sessions | sed -E 's/.*"session_id":([0-9]+).*/\1/')
curl -s -X POST \
  -H "authorization: Bearer $RUST_AGENT_SERVER_TOKEN" \
  -H 'content-type: application/json' \
  -d "{\"session_id\":$SESSION_ID}" \
  http://127.0.0.1:4096/sessions:restore
curl -N -H "authorization: Bearer $RUST_AGENT_SERVER_TOKEN" \
  -H 'content-type: application/json' \
  -d '{"message":"Say hello in one sentence"}' \
  "http://127.0.0.1:4096/sessions/$SESSION_ID/turns:stream"
```

Server configuration:

- `RUST_AGENT_SERVER_HTTP_ADDR`
- `RUST_AGENT_SERVER_H3_ADDR`
- `RUST_AGENT_SERVER_TOKEN`
- `RUST_AGENT_SERVER_TLS_CERT`
- `RUST_AGENT_SERVER_TLS_KEY`
- `RUST_AGENT_SERVER_EVENT_QUEUE_CAPACITY`
- `RUST_AGENT_SERVER_MAX_SESSIONS`
- `RUST_AGENT_SERVER_MAX_EVENT_CHANNELS`
- `RUST_AGENT_SERVER_H3_MAX_CONCURRENT_STREAMS`
- `RUST_AGENT_SERVER_H3_IDLE_TIMEOUT_SECS`
- `RUST_AGENT_PARENT_PID`
- `RUST_AGENT_STATE_DB`

## Tool Mode

Tools are read-only by default. Enable workspace file edits with:

```bash
cd /Users/ajc/rust-agent
RUST_AGENT_TOOL_MODE=workspace-write cargo run -- "Update the requested file"
```

`workspace-write` exposes `apply_patch` for workspace-confined UTF-8 file edits.
Use it only for trusted local sessions, especially in server mode.

Enable trusted command execution with:

```bash
cd /Users/ajc/rust-agent
RUST_AGENT_TOOL_MODE=workspace-exec cargo run -- "Run the relevant checks"
```

`workspace-exec` exposes `workspace-write` tools plus `exec_command`.
`exec_command` runs shell commands from the workspace and does not currently
apply command-level allow/deny protections; those restrictions are intentionally
deferred and must be implemented before using `workspace-exec` for untrusted
sessions. When stdout or stderr exceeds the retained preview, the tool returns
the tail and saves the full stream under
`target/rust-agent-tool-output/`. Use `workspace-exec` only for trusted local
sessions.

## Authentication

The application owns its ChatGPT login state at `~/.rust-agent/auth.json` and
session database at `~/.rust-agent/sessions.db` by default. Set
`RUST_AGENT_HOME` to use a different directory for both, or set
`RUST_AGENT_STATE_DB` to override only the session database path.

Check login status with:

```bash
cargo run -- login status
```

If authentication is missing or expired, run:

```bash
cargo run -- login --device-code
```

To remove local auth and revoke the refresh token when possible, run:

```bash
cargo run -- logout
```

## Release Binary

Build and run a standalone binary with:

```bash
cd /Users/ajc/rust-agent
cargo build --release
./target/release/rust-agent "Reply with exactly: rust-agent-ok"
```

Release builds strip symbols, use thin LTO, and abort on panic to keep the standalone binary smaller.
