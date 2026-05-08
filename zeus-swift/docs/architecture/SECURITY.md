# Security Notes

Zeus is a local frontend for a local `rust-agent` process. It assumes the user
controls the machine and selected workspace.

## Local Server

Zeus generates one bearer token per backend launch and passes it through
`RUST_AGENT_SERVER_TOKEN`. The backend binds HTTP to `127.0.0.1:0`, and Zeus
uses the readiness line to learn the selected port. Bearer-token holders are
fully authorized for that local server.

## Workspace

Zeus passes the selected workspace as `RUST_AGENT_WORKSPACE` and rejects startup
if readiness or `GET /` reports a different canonical path. Branch switches go
through the backend workspace API so backend search state and UI state stay
aligned.

## Auth

`RustAgentAuth` runs backend login commands and inherits the process
environment. `RUST_AGENT_HOME` changes where the backend stores `auth.json` and
the default session database.

## Terminal Passthrough

Terminal passthrough is user-initiated and uses `POST /terminal:run`. It records
the command and bounded output in the session transcript, but it does not grant
future model turns `workspace-exec` permissions.

## Packaging

Local builds are ad-hoc signed by default. Distributable builds should use a
Developer ID signing identity and notarization profile.
