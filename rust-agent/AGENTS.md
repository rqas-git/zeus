# Running rust-agent

## Development Guidance

When making changes, always validate all implemented functionality before finishing. Run the relevant automated checks and any live or end-to-end validation needed to prove the changed behavior works.

Write concise, correct code. Follow DRY and YAGNI principles: avoid duplication, avoid speculative abstractions, and keep the implementation as small as the current behavior allows.

Always create concise, atomic commits for changes made.

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

Then type messages after the `You:` prompt. The interactive session keeps conversation history in memory until it exits. Submit a blank line to exit.

## Authentication

The application expects a valid Codex login at `~/.codex/auth.json`.

Check login status with:

```bash
codex login status
```

If authentication is missing or expired, run:

```bash
codex login
```

## Release Binary

Build and run a standalone binary with:

```bash
cd /Users/ajc/rust-agent
cargo build --release
./target/release/rust-agent "Reply with exactly: rust-agent-ok"
```
