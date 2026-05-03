# Configuration Architecture

`AppConfig` collects runtime settings from environment variables and splits them
into client, model, context-window, output, and telemetry configuration.

## Flow

1. `main` calls `AppConfig::from_env` once at startup.
2. `ClientConfig` configures model transport.
3. `ModelConfig` configures the default model and backend allowlist.
4. `ContextWindowConfig` configures prompt history bounds.
5. `OutputConfig` configures terminal delta buffering.
6. `TelemetryConfig` configures optional terminal telemetry output.
7. The resulting values are passed into long-lived services.

## Environment Variables

- `RUST_AGENT_MODEL`
- `RUST_AGENT_ALLOWED_MODELS`
- `RUST_AGENT_INSTRUCTIONS`
- `RUST_AGENT_RESPONSES_URL`
- `RUST_AGENT_ORIGINATOR`
- `RUST_AGENT_VERSION`
- `RUST_AGENT_HOME`
- `RUST_AGENT_REQUEST_TIMEOUT_SECS`
- `RUST_AGENT_PROMPT_CACHE_NAMESPACE`
- `RUST_AGENT_CONTEXT_MAX_MESSAGES`
- `RUST_AGENT_CONTEXT_MAX_BYTES`
- `RUST_AGENT_HISTORY_MAX_MESSAGES`
- `RUST_AGENT_HISTORY_MAX_BYTES`
- `RUST_AGENT_DELTA_FLUSH_INTERVAL_MS`
- `RUST_AGENT_DELTA_FLUSH_BYTES`
- `RUST_AGENT_CACHE_HEALTH`

## Responsibilities

- Config parsing validates numeric values before service startup.
- Model parsing validates defaults and the allowlist before service startup.
- Defaults keep the CLI usable without extra setup.
- Sub-config structs keep unrelated knobs separate.

## Performance Notes

- Configuration is loaded once, not per turn.
- Model changes validate against an in-memory allowlist.
- Prompt, history, and output limits are plain copyable values.
- Telemetry output is opt-in so normal assistant text stays clean.
- Prompt-cache namespace lets backend deployments separate cache keys.
- `RUST_AGENT_HOME` changes the directory that stores `auth.json`; when unset,
  rust-agent uses `~/.rust-agent/auth.json`.

## Current Scope

Configuration is environment-only. Auth tokens are stored separately in
`auth.json`. File config, dynamic reload, and per-request overrides should be
added only when endpoint behavior requires them.
