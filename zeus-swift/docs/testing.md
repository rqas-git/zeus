# Testing

Run frontend checks from the repository root:

```bash
cd /Users/ajc/zeus
swift test
swift run zeus-checks
```

`swift test` runs the Swift Testing wrapper. `swift run zeus-checks` runs the
same reusable check suite as a command-line executable and prints pass/fail
lines.

## What Is Covered

- `TerminalMarkdownParser`
- `ToolMetadata`
- `AgentServerEvent`
- response cache stat formatting
- Rust API contract fixture decoding
- `PathDisplay`
- `PromptHistory`

## When To Run More

Run backend checks too when a change crosses the Rust/Swift contract boundary.
Run the app manually with `swift run zeus` when changing process startup,
keyboard behavior, transcript rendering, terminal passthrough, login, packaging,
or other UI workflows that pure checks cannot exercise.
