# Context Window Architecture

The context window bounds how much in-memory conversation history is sent to the
model. It is intentionally simple and fast for the current early-stage service.

## Flow

1. `AgentLoop` stores every completed user and assistant message in memory.
2. Before a model call, `InMemorySessionStore` walks messages from newest to
   oldest.
3. Messages are retained until the configured message or byte budget is reached.
4. The retained messages are reversed back into chronological order.
5. The prompt window borrows message text instead of cloning it.

## Responsibilities

- `ContextWindowConfig` defines message and byte budgets.
- `InMemorySessionStore` applies the bounds.
- `ConversationMessage` provides the borrowed model-facing view.
- `ChatGptClient` serializes only the retained prompt window.

## Performance Notes

- Windowing prevents unbounded prompt payload growth.
- Borrowed prompt views avoid copying message bodies per turn.
- The latest message is always retained, even if it exceeds the byte budget.

## Current Scope

This is recency-based truncation, not semantic compaction. Summary generation,
tool-output pruning, and token-accurate budgeting are future work.
