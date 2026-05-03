# Configuration Architecture

`AppConfig` collects runtime settings from environment variables and splits them
into client, model, context-window, and output configuration.

## Flow

1. `main` calls `AppConfig::from_env` once at startup.
2. `ClientConfig` configures model transport.
3. `ModelConfig` configures the default model and backend allowlist.
4. `ContextWindowConfig` configures prompt history bounds.
5. `OutputConfig` configures terminal delta buffering.
6. The resulting values are passed into long-lived services.

## Environment Variables

- `RUST_AGENT_MODEL`
- `RUST_AGENT_ALLOWED_MODELS`
- `RUST_AGENT_INSTRUCTIONS`
- `RUST_AGENT_RESPONSES_URL`
- `RUST_AGENT_ORIGINATOR`
- `RUST_AGENT_VERSION`
- `RUST_AGENT_REQUEST_TIMEOUT_SECS`
- `RUST_AGENT_PROMPT_CACHE_NAMESPACE`
- `RUST_AGENT_CONTEXT_MAX_MESSAGES`
- `RUST_AGENT_CONTEXT_MAX_BYTES`
- `RUST_AGENT_DELTA_FLUSH_INTERVAL_MS`
- `RUST_AGENT_DELTA_FLUSH_BYTES`

## Responsibilities

- Config parsing validates numeric values before service startup.
- Model parsing validates defaults and the allowlist before service startup.
- Defaults keep the CLI usable without extra setup.
- Sub-config structs keep unrelated knobs separate.

## Performance Notes

- Configuration is loaded once, not per turn.
- Model changes validate against an in-memory allowlist.
- Context and output limits are plain copyable values.
- Prompt-cache namespace lets backend deployments separate cache keys.

## Current Scope

Configuration is environment-only. File config, dynamic reload, and per-request
overrides should be added only when endpoint behavior requires them.
