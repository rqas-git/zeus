# Configuration Architecture

`AppConfig` collects runtime settings from environment variables and splits them
into client, model, context-window, output, server, telemetry, and tool
configuration.

## Flow

1. `main` calls `AppConfig::from_env` once at startup.
2. `ClientConfig` configures model transport.
3. `ModelConfig` configures the default model and backend allowlist.
4. `ContextWindowConfig` configures prompt history bounds.
5. `OutputConfig` configures terminal delta buffering.
6. `ServerConfig` configures HTTP compatibility and native HTTP/3 listeners.
7. `TelemetryConfig` configures optional terminal telemetry output.
8. `ToolConfig` configures which built-in tools are exposed to the model.
9. The resulting values are passed into long-lived services.

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
- `RUST_AGENT_CACHE_HEALTH`
- `RUST_AGENT_TOOL_MODE`

## Responsibilities

- Config parsing validates numeric values before service startup.
- Model parsing validates defaults and the allowlist before service startup.
- Defaults keep the CLI usable without extra setup.
- Sub-config structs keep unrelated knobs separate.
- Tool mode parsing validates the requested permission set before any model
  request can run.

## Performance Notes

- Configuration is loaded once, not per turn.
- Model changes validate against an in-memory allowlist.
- Prompt, history, and output limits are plain copyable values.
- Server bind addresses, bearer token, session bounds, queue capacity, stream
  limits, and idle timeout are loaded once before listeners start.
- Telemetry output is opt-in so normal assistant text stays clean.
- Prompt-cache namespace lets backend deployments separate cache keys.
- `RUST_AGENT_TOOL_MODE` defaults to `read-only`; set it to `workspace-write`
  to expose the `apply_patch` file editing tool.
- Set `RUST_AGENT_TOOL_MODE=workspace-exec` to expose `workspace-write` tools,
  a bounded `exec_command` shell tool, and dedicated git wrappers. The shell
  tool rejects direct `git` executable tokens; use `git_status`, `git_diff`,
  `git_log`, and `git_commit` for repository operations.
- `RUST_AGENT_HOME` changes the directory that stores `auth.json`; when unset,
  rust-agent uses `~/.rust-agent/auth.json`.

## Current Scope

Configuration is environment-only. ChatGPT auth tokens are stored separately in
`auth.json`; `RUST_AGENT_SERVER_TOKEN` is the local server bearer token. HTTP/3
uses a generated self-signed development certificate unless
`RUST_AGENT_SERVER_TLS_CERT` and `RUST_AGENT_SERVER_TLS_KEY` are set together.
Tool permissions and server route authentication are process-wide. File config,
dynamic reload, and per-request overrides should be added only when endpoint
behavior requires them.
