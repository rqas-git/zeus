# Context Window Architecture

The context window bounds how much conversation history is sent to the model and
how much completed history is retained in memory. It is intentionally simple and
fast for the current early-stage service.

## Flow

1. `AgentLoop` stores completed user messages, assistant messages, function
   calls, function-call outputs, and compaction entries in memory.
2. Before a model call, `InMemorySessionStore` prunes retained messages to the
   configured history budget. When compaction is enabled or a compaction entry
   exists, pruning keeps the latest compaction boundary and newer messages.
3. Before a model call, `InMemorySessionStore` walks retention units from newest
   to oldest. A normal message is one unit; a consecutive run of function-call
   and function-output items is one tool transcript unit.
4. Units are retained for the prompt until the configured message or byte budget
   is reached. After compaction, the prompt uses the latest summary plus the
   retained recent tail instead of recency-only truncation.
5. The retained prompt messages are reversed back into chronological order.
6. The prompt window borrows message text instead of cloning it.
7. After a completed or failed turn, stored history is pruned again.

## Responsibilities

- `ContextWindowConfig` defines prompt and history message and byte budgets.
- `CompactionConfig` defines the token window, reserve, retained recent tail,
  and whether automatic compaction is enabled.
- `InMemorySessionStore` applies prompt-window and retained-history bounds.
- Function-call and function-output transcript items are retained or pruned
  atomically to keep Responses follow-up requests well-formed.
- `ConversationMessage` provides the borrowed model-facing view, including
  structured Responses tool-call items.
- Compaction entries are represented to the model as user summary messages.
- `ChatGptClient` serializes only the retained prompt window.

## Performance Notes

- Windowing prevents unbounded prompt payload growth.
- History retention prevents unbounded session memory growth.
- Semantic compaction prevents long-running sessions from relying only on
  recency truncation once estimated context crosses the configured threshold.
- Borrowed prompt views avoid copying message bodies, raw tool arguments, and
  tool outputs per turn.
- The latest message or tool transcript unit is always retained, even if it
  exceeds the byte budget.

## Current Scope

Recency-based truncation remains the fallback when compaction is disabled and no
compaction entry exists. Semantic compaction uses approximate chars-per-token
budgeting, pi-style summary prompts, and truncated tool results for summary
generation. Token-accurate budgeting and provider-managed compaction are out of
scope.
