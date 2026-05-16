# Contract Architecture

The `rust-agent` server API is the contract boundary. Rust owns the canonical
contract generator and checked-in fixture. Swift owns typed decoding and client
usage of that contract.

## Update Flow

When a route, request, response, readiness field, capability, permission,
transcript record, or SSE event changes:

1. Update Rust server types and contract generation.
2. Regenerate `rust-agent/docs/contracts/zeus-api-contract.json`.
3. Update `Sources/ZeusCore/AgentAPIContract.swift`.
4. Update `Sources/ZeusCore/AgentServerEvent.swift` for SSE changes.
5. Update `Sources/Zeus/AgentAPIClient.swift` when routes or request behavior
   change.
6. Update `Sources/Zeus/ChatViewModel.swift` and views when UI behavior changes.
7. Update `Tests/ZeusCheckSuite/APIContractChecks.swift`.
8. Run backend and frontend checks.

## Compatibility Rules

- The frontend must not infer undocumented server behavior.
- Required startup features are validated in `RustAgentServer`.
- `path_completion` and `session_compaction` are required features because the
  UI depends on backend routes for prompt autocomplete and manual context
  compaction.
- Unknown SSE events decode as `.unknown` so newer servers do not crash older
  clients.
- New UI-visible capabilities should be explicit backend feature flags or route
  data.

## Current Scope

Swift checks decode the fixture but do not regenerate it. Run the Rust contract
command from `rust-agent/` when the server contract changes.
