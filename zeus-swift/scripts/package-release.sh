#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ZEUS_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
RUST_AGENT_ROOT="${RUST_AGENT_ROOT:-$ZEUS_ROOT/../rust-agent}"

APP_NAME="${APP_NAME:-Zeus}"
BUNDLE_ID="${BUNDLE_ID:-dev.ajc.zeus}"
VERSION="${VERSION:-0.1.0}"
BUILD_VERSION="${BUILD_VERSION:-$(git -C "$ZEUS_ROOT" rev-list --count HEAD 2>/dev/null || date +%Y%m%d%H%M%S)}"
DIST_DIR="${DIST_DIR:-$ZEUS_ROOT/dist}"
SIGN_IDENTITY="${SIGN_IDENTITY:--}"
NOTARY_PROFILE="${NOTARY_PROFILE:-}"

ZEUS_BINARY="$ZEUS_ROOT/.build/release/zeus"
DMG_PATH="$DIST_DIR/$APP_NAME.dmg"
STAGING_DIR=""

cleanup() {
    if [[ -n "$STAGING_DIR" ]]; then
        rm -rf "$STAGING_DIR"
    fi
}
trap cleanup EXIT

require_file() {
    if [[ ! -f "$1" ]]; then
        echo "missing required file: $1" >&2
        exit 1
    fi
}

if [[ ! -d "$RUST_AGENT_ROOT" ]]; then
    echo "RUST_AGENT_ROOT does not exist: $RUST_AGENT_ROOT" >&2
    exit 1
fi
RUST_AGENT_ROOT="$(cd "$RUST_AGENT_ROOT" && pwd)"
RUST_AGENT_BINARY="$RUST_AGENT_ROOT/target/release/rust-agent"

if [[ ! -f "$RUST_AGENT_ROOT/Cargo.toml" ]]; then
    echo "RUST_AGENT_ROOT does not point at rust-agent: $RUST_AGENT_ROOT" >&2
    exit 1
fi

mkdir -p "$DIST_DIR"
STAGING_DIR="$(mktemp -d "$DIST_DIR/.package.XXXXXX")"
APP_DIR="$STAGING_DIR/$APP_NAME.app"
CONTENTS_DIR="$APP_DIR/Contents"
MACOS_DIR="$CONTENTS_DIR/MacOS"

echo "Building rust-agent release..."
cargo build --release --manifest-path "$RUST_AGENT_ROOT/Cargo.toml"

echo "Building Zeus release..."
(
    cd "$ZEUS_ROOT"
    swift build -c release --product zeus
)

require_file "$ZEUS_BINARY"
require_file "$RUST_AGENT_BINARY"

echo "Creating $APP_NAME.app..."
mkdir -p "$MACOS_DIR"
cp "$ZEUS_BINARY" "$MACOS_DIR/$APP_NAME"
cp "$RUST_AGENT_BINARY" "$MACOS_DIR/rust-agent"
chmod 755 "$MACOS_DIR/$APP_NAME" "$MACOS_DIR/rust-agent"

cat > "$CONTENTS_DIR/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleDevelopmentRegion</key>
  <string>en</string>
  <key>CFBundleExecutable</key>
  <string>$APP_NAME</string>
  <key>CFBundleIdentifier</key>
  <string>$BUNDLE_ID</string>
  <key>CFBundleInfoDictionaryVersion</key>
  <string>6.0</string>
  <key>CFBundleName</key>
  <string>$APP_NAME</string>
  <key>CFBundlePackageType</key>
  <string>APPL</string>
  <key>CFBundleShortVersionString</key>
  <string>$VERSION</string>
  <key>CFBundleVersion</key>
  <string>$BUILD_VERSION</string>
  <key>LSMinimumSystemVersion</key>
  <string>13.0</string>
  <key>NSHighResolutionCapable</key>
  <true/>
</dict>
</plist>
PLIST

echo "Signing app bundle..."
if [[ "$SIGN_IDENTITY" == "-" ]]; then
    codesign --force --sign - "$MACOS_DIR/rust-agent"
    codesign --force --sign - "$APP_DIR"
else
    codesign --force --timestamp --options runtime --sign "$SIGN_IDENTITY" "$MACOS_DIR/rust-agent"
    codesign --force --timestamp --options runtime --sign "$SIGN_IDENTITY" "$APP_DIR"
fi
codesign --verify --deep --strict --verbose=2 "$APP_DIR"

echo "Creating DMG..."
hdiutil create -volname "$APP_NAME" -srcfolder "$APP_DIR" -ov -format UDZO "$DMG_PATH"

if [[ -n "$NOTARY_PROFILE" ]]; then
    if [[ "$SIGN_IDENTITY" == "-" ]]; then
        echo "NOTARY_PROFILE is set, but SIGN_IDENTITY is ad-hoc; skipping notarization." >&2
    else
        echo "Submitting DMG for notarization..."
        xcrun notarytool submit "$DMG_PATH" --keychain-profile "$NOTARY_PROFILE" --wait
        xcrun stapler staple "$DMG_PATH"
    fi
fi

echo "Packaged: $DMG_PATH"
