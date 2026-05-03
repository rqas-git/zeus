# Tooling Architecture

`ToolRegistry` exposes model-callable workspace tools. The default policy is
read-only. Write access is opt-in with `RUST_AGENT_TOOL_MODE=workspace-write`.

## Modes

- `read-only` exposes `read_file`, `list_dir`, `search_files`, and
  `search_text`.
- `workspace-write` exposes the read-only tools plus `apply_patch`.

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

## Safety

- Absolute paths and paths escaping the workspace are rejected.
- Add-file targets must not already exist.
- Update and delete targets must be existing UTF-8 files.
- A patch is parsed and all file changes are planned before any write starts, so
  validation failures do not leave earlier patch operations applied.
- Individual file writes use a temporary sibling file and rename into place.
- Patch input is capped at 256 KiB, and edited files are capped at 2 MiB.

## Current Scope

The registry does not expose shell commands, network tools, or arbitrary file
writes. Cross-file writes are planned before execution, but filesystem failures
during the final rename phase can still leave a partial multi-file patch.
