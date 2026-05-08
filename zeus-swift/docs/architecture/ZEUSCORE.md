# ZeusCore Architecture

`ZeusCore` contains reusable, UI-independent Swift logic shared by the app and
checks.

## Modules

- `AgentAPIContract.swift` defines typed request and response models for the
  backend contract.
- `AgentServerEvent.swift` decodes SSE JSON payloads into stable Swift enum
  cases.
- `ToolMetadata.swift` maps backend tool names and JSON arguments into compact
  UI metadata.
- `TerminalMarkdownParser.swift` parses the lightweight markdown subset used by
  terminal transcript rendering.
- `PromptHistory.swift` implements shell-like submitted-message navigation.
- `PathDisplay.swift` formats local paths for display.

## Responsibilities

- Stay pure Swift and independent of SwiftUI, AppKit, `Process`, and
  `URLSession`.
- Preserve forward compatibility where practical, such as unknown SSE event
  handling.
- Keep backend field names explicit with `CodingKeys`.
- Keep behavior covered by `ZeusCheckSuite`.

## Current Scope

`ZeusCore` is not a general SDK. It contains only logic needed by the Zeus app
and its checks. Backend behavior that affects wire compatibility belongs in the
Rust contract fixture first.
