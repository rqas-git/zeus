# Running rust-agent

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

Then type a message after the `You:` prompt.

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
