# Packaging

Build a local installable DMG that embeds the `rust-agent` release binary inside
`Zeus.app`:

```bash
cd /Users/ajc/zeus-swift
scripts/package-release.sh
```

The default output is `dist/Zeus.dmg`. By default the app is signed ad-hoc for
local testing.

Useful environment variables:

```bash
RUST_AGENT_ROOT=/path/to/rust-agent
APP_NAME=Zeus
BUNDLE_ID=dev.ajc.zeus
VERSION=0.1.0
BUILD_VERSION=123
DIST_DIR=/tmp/zeus-dist
SIGN_IDENTITY="Developer ID Application: Your Name (TEAMID)"
NOTARY_PROFILE=apple-notary-profile
```

Set `SIGN_IDENTITY` to a Developer ID Application identity and
`NOTARY_PROFILE` to a `notarytool` keychain profile when creating a distributable
DMG for other users.
