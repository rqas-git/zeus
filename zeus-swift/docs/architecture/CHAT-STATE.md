# Chat State Architecture

`ChatViewModel` is the main UI state machine. It is `@MainActor` because it owns
SwiftUI-facing state and coordinates asynchronous backend work. It uses Swift's
granular Observation model (`@Observable`) so views only subscribe to the
properties they read; implementation-only dependencies, indexes, buffers, and
tasks are marked `@ObservationIgnored`.

## Flow

1. Startup creates the backend client, loads models/permissions/workspace, and
   creates a session.
2. User prompts are recorded in prompt history, appended to the transcript, and
   sent through the turn stream.
3. Assistant text deltas are display-buffered on a short cadence, preserving
   completed line chunks while only the active tail changes. The active
   assistant line renders as plain text while streaming, then commits parsed
   Markdown blocks with the final text at completion.
4. Tool-call start and completion events upsert tool transcript rows by call id.
   If the active assistant row is only a synthetic placeholder, the placeholder
   is removed before the first tool row is inserted so the transcript stays in
   chronological order during tool-only model rounds.
5. Token usage updates come from cache-health and turn-token-usage events.
   Cache-health events are also collected for the active assistant response and
   shown only when `/show-cache` is enabled.
6. Completion, cancellation, and error paths clear turn-local state and update
   readiness flags. Successful direct turns attach the backend-provided
   `duration_ms` to the final assistant transcript line when the turn displayed
   tool work, so the transcript can render a Codex-style `Worked for ...`
   separator without persisting that UI chrome.
7. `/restore <session id>` replaces transcript and prompt history from backend
   durable records.
8. The clear-context header action creates a fresh backend session, preserves
   the previous SQLite-backed session, and replaces the visible transcript with
   the previous session id needed for `/restore`.
9. The compact header action manually calls the backend compaction route for the
   active session, after applying the selected model if needed.
10. Transcript search refreshes are debounced and scan line snapshots off the
   main actor.
11. Path and `@` file-reference completion requests are debounced, served by
   `POST /workspace/paths:complete`, and dropped if the draft changes before
   results return.
12. Terminal passthrough applies the selected permission policy, sends commands
   to `terminal:run`, and appends bounded command output to the transcript.

## Responsibilities

- Own readiness, sending, login, branch-switch, terminal, and cancellation flags.
- Own selected model, reasoning effort, tool permission, workspace, and branch
  options.
- Apply selected model and permission before the next model turn, and apply the
  selected permission before terminal passthrough commands.
- Keep backend branch switches in the backend API path.
- Keep prompt-history and transcript-search behavior local to the frontend.
- Keep prompt path-completion parsing in `ZeusCore` and backend suggestion
  requests in the view model.
- Translate `AgentServerEvent` values into transcript lines.
- Keep view-facing display strings compact.

## Task Ownership

- `streamTask` owns the active model turn.
- `sessionEventTask` owns the passive session SSE stream.
- `loginTask` owns device-code login.
- `branchSwitchTask` owns backend branch switching.
- `terminalTask` owns user-initiated terminal commands.
- `contextClearTask` owns fresh-session creation for the clear-context action.
- `contextCompactTask` owns manual active-session context compaction.
- `pathCompletionTask` owns the active autocomplete request and is cancelled on
  draft changes, submission, mode switches, and newer completion requests.
- Assistant text display buffering is synchronous within the active model turn,
  with a short main-actor flush task for smoother partial-line rendering.
- `searchRefreshTask` debounces transcript search updates.

Tasks are cancelled on deinit and when a newer operation supersedes the old
one. Same-session turn ordering is enforced by the backend; the frontend avoids
starting overlapping user turns from one window.

## Current Scope

The view model is intentionally large because it is the boundary between
SwiftUI, backend process lifetime, and session state. Extract only pure,
testable logic into `ZeusCore`; do not move backend API assumptions into views.
