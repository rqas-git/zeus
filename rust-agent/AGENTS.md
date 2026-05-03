# Running rust-agent

## Development Guidance

When making changes, always validate all implemented functionality before finishing. Run the relevant automated checks and any live or end-to-end validation needed to prove the changed behavior works.

When implementation changes affect behavior, configuration, architecture, commands, or operational expectations, update all relevant documentation before finishing.

Write concise, correct code. Follow DRY and YAGNI principles: avoid duplication, avoid speculative abstractions, and keep the implementation as small as the current behavior allows.

Always create concise, atomic commits for changes made.
Keep commits small, independent, and informative. Avoid commits that touch many files or combine unrelated behavior, documentation, and cleanup.

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

Then type messages after the `You:` prompt. The interactive session keeps recent conversation history in memory until it exits, bounded by `RUST_AGENT_HISTORY_MAX_MESSAGES` and `RUST_AGENT_HISTORY_MAX_BYTES`. Submit a blank line to exit.

## Authentication

The application owns its ChatGPT login state at `~/.rust-agent/auth.json` by default.
Set `RUST_AGENT_HOME` to use a different auth directory.

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
