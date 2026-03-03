#!/bin/bash
# Build cef-unity-server.app bundle for Sandbox testing.
# Usage: build-server-sandbox.sh <output_dir>
set -e

OUTPUT_DIR="$1"
if [ -z "$OUTPUT_DIR" ]; then
    echo "Usage: $0 <output_dir>"
    exit 1
fi

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
BUNDLE_ID="com.cef-unity.server"

# CEF framework path
CEF_OUT=$(ls -d "$SCRIPT_DIR/target/debug/build/cef-dll-sys-"*/out/cef_macos_* 2>/dev/null | head -1)
if [ -z "$CEF_OUT" ]; then
    echo "ERROR: CEF build output not found. Run 'cargo build' first."
    exit 1
fi
CEF_FW="$CEF_OUT/Chromium Embedded Framework.framework"

# --- server .app bundle ---
SERVER_APP="$OUTPUT_DIR/cef-unity-server.app"
rm -rf "$SERVER_APP"
mkdir -p "$SERVER_APP/Contents/MacOS"
mkdir -p "$SERVER_APP/Contents/Frameworks"
mkdir -p "$SERVER_APP/Contents/Helpers"

# Server binary
cp "$SCRIPT_DIR/target/debug/cef-unity-server" "$SERVER_APP/Contents/MacOS/"

# CEF framework (symlink)
ln -sf "$CEF_FW" "$SERVER_APP/Contents/Frameworks/Chromium Embedded Framework.framework"

# Server Info.plist
cat > "$SERVER_APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleIdentifier</key>
    <string>${BUNDLE_ID}</string>
    <key>CFBundleExecutable</key>
    <string>cef-unity-server</string>
    <key>CFBundleName</key>
    <string>cef-unity-server</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleVersion</key>
    <string>1.0</string>
    <key>LSBackgroundOnly</key>
    <true/>
</dict>
</plist>
PLIST

# --- helper .app bundle (inside server) ---
HELPER_APP="$SERVER_APP/Contents/Helpers/cef-unity-rust-helper.app"
mkdir -p "$HELPER_APP/Contents/MacOS"
cp "$SCRIPT_DIR/target/debug/cef-unity-rust-helper" "$HELPER_APP/Contents/MacOS/"
cat > "$HELPER_APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleIdentifier</key>
    <string>${BUNDLE_ID}</string>
    <key>CFBundleExecutable</key>
    <string>cef-unity-rust-helper</string>
    <key>CFBundleName</key>
    <string>cef-unity-rust-helper</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleVersion</key>
    <string>1.0</string>
</dict>
</plist>
PLIST

# Codesign
codesign -s - --force --entitlements "$SCRIPT_DIR/helper.entitlements" "$HELPER_APP"
codesign -s - --force --entitlements "$SCRIPT_DIR/server.entitlements" "$SERVER_APP"

echo "server .app built at $SERVER_APP"
