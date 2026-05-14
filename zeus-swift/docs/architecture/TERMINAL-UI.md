# Terminal UI Architecture

`ChatWindow` renders a terminal-style SwiftUI interface and maps keyboard input
to `ChatViewModel` actions.

## Layout

- `HeaderBar` shows app chrome, clear-context, and settings controls.
- `TranscriptView` renders transcript lines, assistant markdown, tool rows, and
  search highlights.
- `InputPrompt` renders the prompt marker and text field.
- `FooterBar` exposes branch, model, effort, permission, and shortcut controls.
- Dropdowns are rendered by footer menu components with keyboard-highlighted
  options.

## Keyboard Model

`LocalEventMonitor` intercepts key events while the window is active. Global
shortcuts open footer menus, toggle terminal passthrough, search the transcript,
or cancel the active turn. Arrow keys either navigate prompt history, move
footer focus, or move the active dropdown highlight depending on current UI
state.

Keep `docs/shortcuts.md` synchronized with any user-visible key behavior.

## Transcript Rendering

Assistant text is parsed by `TerminalMarkdownParser` and rendered by
`TerminalMarkdownView`. Tool calls use `ToolMetadata` to show stable icons,
actions, names, and compact argument targets. Cache stats render below assistant
messages only when enabled.

## Current Scope

The UI is optimized for a single active transcript per window. There is no
sidebar session browser yet, even though the backend exposes session metadata.
