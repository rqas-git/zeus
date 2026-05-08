# Packaging

Build a local installable DMG that embeds a release `rust-agent` binary inside
`Zeus.app`:

```bash
cd /Users/ajc/zeus
zeus-swift/scripts/package-release.sh
```

The default output is `zeus-swift/dist/Zeus.dmg`. The default signing mode is
ad-hoc and intended for local testing.

Useful environment variables:

```bash
RUST_AGENT_ROOT=/path/to/rust-agent
ZEUS_WORKSPACE=/path/to/project
APP_NAME=Zeus
BUNDLE_ID=dev.ajc.zeus
VERSION=0.1.0
BUILD_VERSION=123
DIST_DIR=/tmp/zeus-dist
SIGN_IDENTITY="Developer ID Application: Your Name (TEAMID)"
NOTARY_PROFILE=apple-notary-profile
```

At runtime, `ZEUS_WORKSPACE` selects the project shown by the UI. Zeus passes
that directory to the embedded backend as `RUST_AGENT_WORKSPACE` and verifies
the server reports the same canonical workspace before creating a session. Zeus
launches the embedded backend as an owned child process with a per-launch bearer
token and loopback port `0`; the backend's structured readiness line supplies
the actual bound HTTP and HTTP/3 addresses.

Set `SIGN_IDENTITY` to a Developer ID Application identity and
`NOTARY_PROFILE` to a `notarytool` keychain profile when creating a distributable
DMG for other users.

## Runtime Lookup

Packaged apps prefer the bundled `rust-agent` executable next to the Zeus
binary. Development runs fall back to a fresh `rust-agent/target/debug` binary
or `cargo run` through `RustAgentLocator`. Set `RUST_AGENT_ROOT` when testing
against a nonstandard backend checkout.

## Validation

Before packaging, run the relevant frontend checks and build the backend release
binary:

```bash
cd /Users/ajc/zeus/rust-agent
cargo build --release
cd /Users/ajc/zeus
swift run zeus-checks
```
