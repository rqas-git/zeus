# Configuration Architecture

`AppConfig` collects runtime settings from environment variables and splits them
into client, model, context-window, compaction, output, server, storage,
telemetry, and tool configuration.

## Flow

1. `main` calls `AppConfig::from_env` once at startup.
2. `ClientConfig` configures model transport.
3. `ModelConfig` configures the default model and backend allowlist.
4. `ContextWindowConfig` configures prompt history bounds.
5. `CompactionConfig` configures semantic compaction thresholds.
6. `OutputConfig` configures terminal delta buffering.
7. `ServerConfig` configures HTTP compatibility and native HTTP/3 listeners.
8. `StorageConfig` configures the SQLite session database path.
9. `TelemetryConfig` configures optional terminal telemetry output.
10. `ToolConfig` configures which built-in tools are exposed to the model.
11. The resulting values are passed into long-lived services.

## Environment Variables

- `RUST_AGENT_MODEL`
- `RUST_AGENT_ALLOWED_MODELS`
- `RUST_AGENT_INSTRUCTIONS`
- `RUST_AGENT_RESPONSES_URL`
- `RUST_AGENT_ORIGINATOR`
- `RUST_AGENT_VERSION`
- `RUST_AGENT_HOME`
- `RUST_AGENT_STATE_DB`
- `RUST_AGENT_REQUEST_TIMEOUT_SECS`
- `RUST_AGENT_PROMPT_CACHE_NAMESPACE`
- `RUST_AGENT_CONTEXT_MAX_MESSAGES`
- `RUST_AGENT_CONTEXT_MAX_BYTES`
- `RUST_AGENT_HISTORY_MAX_MESSAGES`
- `RUST_AGENT_HISTORY_MAX_BYTES`
- `RUST_AGENT_COMPACTION_ENABLED`
- `RUST_AGENT_COMPACTION_CONTEXT_TOKENS`
- `RUST_AGENT_COMPACTION_RESERVE_TOKENS`
- `RUST_AGENT_COMPACTION_KEEP_RECENT_TOKENS`
- `RUST_AGENT_DELTA_FLUSH_INTERVAL_MS`
- `RUST_AGENT_DELTA_FLUSH_BYTES`
- `RUST_AGENT_SERVER_HTTP_ADDR`
- `RUST_AGENT_SERVER_ALLOW_REMOTE_HTTP`
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
- `RUST_AGENT_CACHE_HEALTH`
- `RUST_AGENT_TOOL_MODE`
- `RUST_AGENT_WORKSPACE`

## Responsibilities

- Config parsing validates numeric values before service startup.
- Model parsing validates defaults and the allowlist before service startup.
  When `RUST_AGENT_ALLOWED_MODELS` is unset, rust-agent reads Codex's local
  `$CODEX_HOME/models_cache.json` catalog, or `~/.codex/models_cache.json` when
  `CODEX_HOME` is unset, and exposes models marked visible in the picker.
- Defaults keep the CLI usable without extra setup.
- Sub-config structs keep unrelated knobs separate.
- Tool mode parsing validates the requested permission set before any model
  request can run.

## Performance Notes

- Configuration is loaded once, not per turn.
- Model changes validate against an in-memory allowlist.
- Prompt, history, and output limits are plain copyable values.
- Compaction defaults match pi-style behavior: enabled, 272,000 context tokens,
  16,384 reserve tokens, and 20,000 recent tokens retained verbatim.
- Server bind addresses, remote-HTTP opt-in, bearer token, session bounds,
  queue capacity, stream limits, idle timeout, and optional parent-process watch
  are loaded once before listeners start. Set either server address to port `0`
  to let the OS assign a free port and read the selected address from the
  startup readiness JSON. Plaintext HTTP binds must stay on loopback unless
  `RUST_AGENT_SERVER_ALLOW_REMOTE_HTTP=true` is set for a trusted deployment.
- Telemetry output is opt-in so normal assistant text stays clean.
- Prompt-cache namespace lets backend deployments separate cache keys. The
  effective key is shared per namespace and model so sessions with the same
  static prompt prefix can reuse cached input.
- `RUST_AGENT_TOOL_MODE` defaults to `read-only`; set it to `workspace-write`
  to expose the `apply_patch` file editing tool.
- Set `RUST_AGENT_TOOL_MODE=workspace-exec` to expose `workspace-write` tools,
  plus the bounded `exec_command` shell tool. The shell tool currently permits
  any bash command string; command-level restrictions are intentionally
  deferred, so use this mode only for trusted local sessions.
- `RUST_AGENT_TOOL_SEARCH_CONCURRENCY` defaults to `1` and may be set up to
  `16` to allow more simultaneous FFF path/content searches across sessions.
- `RUST_AGENT_WORKSPACE` selects the canonical directory used by built-in
  workspace tools. When unset, rust-agent uses the process current directory.
  Startup fails if the configured path cannot be resolved to an existing
  directory.
- `RUST_AGENT_HOME` changes the directory that stores `auth.json` and the
  default `sessions.db`; when unset, rust-agent uses `~/.rust-agent/`.
- `RUST_AGENT_STATE_DB` overrides the SQLite session database path.

## Current Scope

Configuration is environment-only. ChatGPT auth tokens are stored separately in
`auth.json`; durable sessions are stored in SQLite at `sessions.db` by default;
`RUST_AGENT_SERVER_TOKEN` is the local server bearer token. HTTP/3 uses a
generated self-signed development certificate unless
`RUST_AGENT_SERVER_TLS_CERT` and `RUST_AGENT_SERVER_TLS_KEY` are set together.
Tool permissions and server route authentication are process-wide. File config,
dynamic reload, and per-request overrides should be added only when endpoint
behavior requires them.
