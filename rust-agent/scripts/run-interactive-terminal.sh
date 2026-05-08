#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "This script opens macOS Terminal.app; run it on macOS." >&2
  exit 1
fi

osascript - "$repo_root" <<'APPLESCRIPT'
on run argv
  set repoPath to item 1 of argv
  tell application "Terminal"
    activate
    do script "cd " & quoted form of repoPath & " && cargo run"
  end tell
end run
APPLESCRIPT
