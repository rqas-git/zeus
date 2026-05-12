# Testing

Run frontend checks from the repository root:

```bash
cd /Users/ajc/zeus
swift run zeus-checks
```

`swift run zeus-checks` runs the reusable check suite and prints pass/fail
lines. `swift test` builds the Swift Testing wrapper around the same registered
checks, but the command-line tools runner may not emit per-check output; use
`zeus-checks` when validating behavior locally.

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
