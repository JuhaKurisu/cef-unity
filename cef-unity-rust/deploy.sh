#!/bin/bash
set -e
DEST="../cef-unity-unityproject/Assets/CefUnity/Interop/Plugins/osx-arm64"
# ヘルパーのbundle IDは親プロセス(Unity Editor)と一致させる。
# CEFのMachPortRendezvousServerサービス名が BaseBundleID.MachPortRendezvousServer.PID
# の形式なので、親と子で同じbundle IDが必要。
BUNDLE_ID="com.unity3d.UnityEditor5.x"

cargo build

# --- dylib ---
cp target/debug/libcef_unity_rust.dylib "$DEST/"
codesign -s - --force "$DEST/libcef_unity_rust.dylib"

# --- helper .app bundle ---
HELPER_APP="$DEST/cef-unity-rust-helper.app"
mkdir -p "$HELPER_APP/Contents/MacOS"
cp target/debug/cef-unity-rust-helper "$HELPER_APP/Contents/MacOS/"
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
codesign -s - --force "$HELPER_APP"

# メインバンドル不要 (main_bundle_pathは設定しない)
rm -rf "$DEST/CefUnityBrowser.app"
# 古い素のバイナリがあれば削除
rm -f "$DEST/cef-unity-rust-helper"

echo "deployed"
