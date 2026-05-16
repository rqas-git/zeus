# Terminal UI Architecture

`ChatWindow` renders a terminal-style SwiftUI interface and maps keyboard input
and header actions to `ChatViewModel` operations.

## Layout

- `HeaderBar` shows app chrome, manual compaction, clear-context, and settings
  controls.
- `TranscriptView` renders transcript lines, the marker-free session id label,
  assistant markdown, tool rows, and search highlights.
- `InputPrompt` renders the prompt marker, text field, startup placeholder
  progress, and path-completion dropdown.
- `FooterBar` exposes branch, model, effort, permission, and shortcut controls.
- Dropdowns are rendered by footer menu components with keyboard-highlighted
  options.

## Keyboard Model

`LocalEventMonitor` intercepts key events while the window is active. Global
shortcuts open footer menus, toggle terminal passthrough, search the transcript,
or cancel the active turn. Arrow keys either navigate prompt history, move
footer focus, or move the active dropdown highlight depending on current UI
state.

Typing `@` at a token boundary in chat mode opens backend-served file-reference
completion. `Tab` opens path completion or accepts the highlighted suggestion.
When the completion dropdown is visible, Up and Down move its highlight, Return
accepts the highlighted suggestion before submitting, and Esc closes it.
Terminal passthrough uses Tab path completion but does not auto-open
file-reference completion for `@`.

Keep `docs/shortcuts.md` synchronized with any user-visible key behavior.

## Transcript Rendering

Assistant text is parsed by `TerminalMarkdownParser` and rendered by
`TerminalMarkdownView`. Tool calls use `ToolMetadata` to show stable icons,
actions, names, and compact argument targets. Cache stats render below assistant
messages only when enabled.

Transcript block separators are render-only UI chrome. `TranscriptView` draws a
Codex-style dim horizontal rule between tool/work rows and the following
assistant response, after the initial session status block, and after the final
assistant response for completed turns that displayed tool work. The final
separator includes the backend-provided `Worked for ...` duration. These
separators are not persisted, restored, searched, or exposed as transcript
records. Cancelled and errored turns do not render response timing yet. Login
output is also not grouped into its own block yet.

## Current Scope

The UI is optimized for a single active transcript per window. There is no
sidebar session browser yet, even though the backend exposes session metadata.
