#!/bin/bash
set -e

DEST="../cef-unity-unityproject/Assets/CefUnity/Interop/Plugins/osx-arm64"

# ヘルパーのbundle IDは親プロセス(CEF Server)と一致させる。
# CEFのMachPortRendezvousServerサービス名が BaseBundleID.MachPortRendezvousServer.PID
# の形式なので、親と子で同じbundle IDが必要。
BUNDLE_ID="com.cef-unity.server"

cargo build --release

# --- dylib (IPC client only, no CEF) ---
cp target/release/libcef_unity_rust.dylib "$DEST/"
codesign -s - --force "$DEST/libcef_unity_rust.dylib"

# --- CEF framework path ---
CEF_OUT=$(ls -d target/release/build/cef-dll-sys-*/out/cef_macos_* 2>/dev/null | head -1)
CEF_FW="$CEF_OUT/Chromium Embedded Framework.framework"
if [ ! -d "$CEF_FW" ]; then
    echo "ERROR: CEF framework not found at $CEF_FW"
    exit 1
fi

# --- server .app bundle ---
SERVER_APP="$DEST/cef-unity-server.app"
rm -rf "$SERVER_APP"
mkdir -p "$SERVER_APP/Contents/MacOS"
mkdir -p "$SERVER_APP/Contents/Frameworks"
mkdir -p "$SERVER_APP/Contents/Helpers"

# Server binary
cp target/release/cef-unity-server "$SERVER_APP/Contents/MacOS/"

# CEF framework (symlink to avoid doubling disk usage)
ln -sf "$(cd "$CEF_OUT" && pwd)/Chromium Embedded Framework.framework" \
    "$SERVER_APP/Contents/Frameworks/Chromium Embedded Framework.framework"

# Server Info.plist (LSBackgroundOnly = headless)
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
cp target/release/cef-unity-rust-helper "$HELPER_APP/Contents/MacOS/"
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

# Unity が .app 内に .meta ファイルを作るので codesign 前に削除
find "$SERVER_APP" -name '*.meta' -delete

# Codesign (helper first, then server)
codesign -s - --force --entitlements helper.entitlements "$HELPER_APP"
codesign -s - --force --entitlements server.entitlements "$SERVER_APP"

# 旧構成のファイルを削除
rm -rf "$DEST/cef-unity-rust-helper.app"
rm -f "$DEST/cef-unity-rust-helper"
rm -rf "$DEST/CefUnityBrowser.app"

echo "deployed"
