# Tooling Architecture

`ToolRegistry` exposes model-callable workspace tools. The default policy is
read-only. Write access is opt-in with `RUST_AGENT_TOOL_MODE=workspace-write`.

## Modes

- `read-only` exposes `read_file`, `read_file_range`, `list_dir`,
  `search_files`, and `search_text`.
- `workspace-write` exposes the read-only tools plus `apply_patch`.
- `workspace-exec` exposes the workspace-write tools plus `exec_command` and
  dedicated git wrappers: `git_status`, `git_diff`, `git_log`, `git_query`, and
  `git_commit`.

## Patch Tool

`apply_patch` accepts one JSON argument, `patch`, using this patch shape:

```text
*** Begin Patch
*** Update File: src/lib.rs
@@
 context line
-old line
+new line
*** Add File: src/new.rs
+new file contents
*** Delete File: src/old.rs
*** End Patch
```

Update hunks are exact replacements. Each hunk must include context or removed
lines so the tool can find a unique target in the current file. Add and delete
operations are path-based.

## Read and List Tools

`read_file` accepts a required workspace-relative `path`. When `offset` or
`limit` is provided, it reads line-oriented pages with 1-indexed line numbers,
matching the large-file continuation style used by OpenCode and Pi. Paginated
reads default to 2,000 lines, cap individual returned lines at 2,000 bytes, and
cap the whole returned page at 64 KiB.

`list_dir` accepts a required workspace-relative `path` plus optional `offset`,
`limit`, and `depth`. Offsets are 1-indexed. Depth defaults to 1 and is capped at
4. The default list remains capped at 200 entries for compatibility, while
explicit `limit` values may request up to 500 entries.

## Safety

- Absolute paths and paths escaping the workspace are rejected.
- Add-file targets must not already exist.
- Update and delete targets must be existing UTF-8 files.
- A patch is parsed and all file changes are planned before any write starts, so
  validation failures do not leave earlier patch operations applied.
- Individual file writes use a temporary sibling file and rename into place.
- Patch input is capped at 256 KiB, and edited files are capped at 2 MiB.
- `exec_command` runs commands through `bash -lc` from a workspace-confined
  current directory, captures stdout and stderr separately, caps retained output,
  keeps the tail of truncated streams, writes full truncated streams under
  `target/rust-agent-tool-output/`, enforces a timeout, rejects oversized command
  inputs, and kills the process group when total output exceeds the hard output
  ceiling.
- `exec_command` rejects command lines that mention a direct `git` executable
  token. Repository operations must use the dedicated git wrappers.
- `git_query` allows read-only inspection commands: `status`, `diff`, `log`,
  `show`, `blame`, `grep`, `ls-files`, `branch --show-current`, `rev-parse`,
  `merge-base`, `describe`, `worktree list`, and `submodule status`.
- `git_commit` requires explicit workspace-relative paths and commits only those
  pathspecs. Git path lists and commit messages are bounded before execution.
- `list_dir` keeps only the lexicographically first capped result set in memory
  while scanning large directories.
- `search_text` output is capped before it is returned to the model.
- FFF `search_files` and `search_text` calls run on blocking workers behind a
  process-local concurrency limiter.

## Current Scope

The registry does not expose network tools or arbitrary file writes.
Cross-file writes are planned before execution, but filesystem failures during
the final rename phase can still leave a partial multi-file patch.
`workspace-exec` is intended only for trusted local sessions; shell commands are
not filesystem-sandboxed beyond their starting directory.
