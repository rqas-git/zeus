# Chat State Architecture

`ChatViewModel` is the main UI state machine. It is `@MainActor` because it owns
published SwiftUI state and coordinates asynchronous backend work.

## Flow

1. Startup creates the backend client, loads models/permissions/workspace, and
   creates a session.
2. User prompts are recorded in prompt history, appended to the transcript, and
   sent through the turn stream.
3. Assistant text deltas are buffered briefly before updating the transcript to
   avoid excessive UI churn. The active assistant line renders as plain text
   while streaming, then stores parsed Markdown blocks after completion.
4. Tool-call start and completion events upsert tool transcript rows by call id.
5. Token usage updates come from cache-health and turn-token-usage events.
   Cache-health events are also collected for the active assistant response and
   shown only when `/show-cache` is enabled.
6. Completion, cancellation, and error paths clear turn-local state and update
   readiness flags.
7. `/restore <session id>` replaces transcript and prompt history from backend
   durable records.
8. Terminal passthrough sends commands to `terminal:run` and appends bounded
   command output to the transcript.

## Responsibilities

- Own readiness, sending, login, branch-switch, terminal, and cancellation flags.
- Own selected model, reasoning effort, tool permission, workspace, and branch
  options.
- Apply selected model and permission before the next model turn.
- Keep backend branch switches in the backend API path.
- Keep prompt-history and transcript-search behavior local to the frontend.
- Translate `AgentServerEvent` values into transcript lines.
- Keep view-facing display strings compact.

## Task Ownership

- `streamTask` owns the active model turn.
- `sessionEventTask` owns the passive session SSE stream.
- `loginTask` owns device-code login.
- `branchSwitchTask` owns backend branch switching.
- `terminalTask` owns user-initiated terminal commands.
- `assistantDeltaFlushTask` batches assistant text updates.

Tasks are cancelled on deinit and when a newer operation supersedes the old
one. Same-session turn ordering is enforced by the backend; the frontend avoids
starting overlapping user turns from one window.

## Current Scope

The view model is intentionally large because it is the boundary between
SwiftUI, backend process lifetime, and session state. Extract only pure,
testable logic into `ZeusCore`; do not move backend API assumptions into views.
