# Context Window Architecture

The context window bounds how much conversation history is sent to the model and
how much completed history is retained in memory. It is intentionally simple and
fast for the current early-stage service.

## Flow

1. `AgentLoop` stores recent completed user messages, assistant messages,
   function calls, and function-call outputs in memory.
2. Before a model call, `InMemorySessionStore` prunes retained messages to the
   configured history budget.
3. Before a model call, `InMemorySessionStore` walks retention units from newest
   to oldest. A normal message is one unit; a consecutive run of function-call
   and function-output items is one tool transcript unit.
4. Units are retained for the prompt until the configured message or byte budget
   is reached.
5. The retained prompt messages are reversed back into chronological order.
6. The prompt window borrows message text instead of cloning it.
7. After a completed or failed turn, stored history is pruned again.

## Responsibilities

- `ContextWindowConfig` defines prompt and history message and byte budgets.
- `InMemorySessionStore` applies prompt-window and retained-history bounds.
- Function-call and function-output transcript items are retained or pruned
  atomically to keep Responses follow-up requests well-formed.
- `ConversationMessage` provides the borrowed model-facing view, including
  structured Responses tool-call items.
- `ChatGptClient` serializes only the retained prompt window.

## Performance Notes

- Windowing prevents unbounded prompt payload growth.
- History retention prevents unbounded session memory growth.
- Borrowed prompt views avoid copying message bodies, raw tool arguments, and
  tool outputs per turn.
- The latest message or tool transcript unit is always retained, even if it
  exceeds the byte budget.

## Current Scope

This is recency-based truncation, not semantic compaction. Summary generation,
semantic tool-output pruning, and token-accurate budgeting are future work.
History retention is approximate byte-based retention over message text and
structured tool payloads.
