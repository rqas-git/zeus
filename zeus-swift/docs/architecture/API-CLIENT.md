# API Client Architecture

`AgentAPIClient` is the Swift HTTP client for the `rust-agent` local server. It
uses typed `ZeusCore` request and response models.

## Flow

1. `RustAgentServer` creates `AgentAPIClient` with the readiness HTTP base URL
   and bearer token.
2. Public health and compatibility routes are called without authorization.
3. Session, workspace, model, permission, terminal, and restore routes include
   `Authorization: Bearer <token>`.
4. JSON requests and responses use `AgentAPIContract.swift` types.
5. Turn and session event streams are read as raw SSE bytes, preserving blank
   event-separator lines before decoding payloads into `AgentServerEvent`.
6. HTTP non-2xx responses become `AgentClientError.httpStatus`.
7. Empty or unparsable streams include a short preview in the thrown error.

## Routes Used

- `GET /healthz`
- `GET /`
- `GET /capabilities`
- `GET /models`
- `GET /permissions`
- `GET /workspace`
- `POST /workspace/branch`
- `POST /sessions`
- `POST /sessions:restore`
- `PUT /sessions/{session_id}/model`
- `PUT /sessions/{session_id}/permissions`
- `POST /sessions/{session_id}/turns:stream`
- `GET /sessions/{session_id}/events`
- `POST /sessions/{session_id}/turns:cancel`
- `POST /sessions/{session_id}/terminal:run`

## Streams

The direct turn stream is the canonical source for events from a submitted user
message. The passive session event stream keeps the UI aware of session events
outside the direct turn path, including terminal command and compaction events.

## Current Scope

The client has no retry policy beyond caller-driven startup polling. It does not
use WebSockets or HTTP/3. Contract compatibility is enforced by decoding the
checked-in Rust fixture in `ZeusCheckSuite`.
