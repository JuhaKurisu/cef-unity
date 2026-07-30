#!/bin/bash
# Rust 成果物と CEF ランタイムを指定ディレクトリへフラット配置する (Linux)。
# deploy-linux.sh (Unity 配置) と build-server-sandbox.sh (Harness 出力) の双方から呼ばれる。
# copy-windows-runtime.ps1 の Linux 版。
#
# cef-unity-server は libcef_unity_rust.so と同じディレクトリ直下から起動されるため
# (crates/client/src/lib.rs の server_binary_path)、両者を同じ場所に置く必要がある。
#
# ソースが無い場合はエラーにせず警告のみ (Rust 未ビルド環境で dotnet build を壊さないため)。
# 成果物の欠落を検出したい呼び出し元は、呼ぶ前後で自前に検査すること。
#
# 使い方: copy-linux-runtime.sh <destination_directory> [debug|release]
set -u

DESTINATION_DIRECTORY="${1:-}"
BUILD_PROFILE="${2:-release}"
if [ -z "$DESTINATION_DIRECTORY" ]; then
    echo "使い方: $0 <destination_directory> [debug|release]" >&2
    exit 2
fi
if [ "$BUILD_PROFILE" != "debug" ] && [ "$BUILD_PROFILE" != "release" ]; then
    echo "ERROR: ビルド構成は debug か release: $BUILD_PROFILE" >&2
    exit 2
fi

SCRIPT_DIRECTORY="$(cd "$(dirname "$0")" && pwd)"
# CARGO_TARGET_DIR が設定されていればそちらを使う (ローカル開発で target/ を
# 別ファイルシステムへ逃がしている場合に対応する)。
TARGET_DIRECTORY="${CARGO_TARGET_DIR:-$SCRIPT_DIRECTORY/target}/$BUILD_PROFILE"

if [ ! -d "$TARGET_DIRECTORY" ]; then
    echo "[copy-linux-runtime] $TARGET_DIRECTORY が無いのでスキップ" >&2
    exit 0
fi

mkdir -p "$DESTINATION_DIRECTORY"

# ---- Rust 成果物 ----
for artifact in cef-unity-server cef-unity-rust-helper libcef_unity_rust.so; do
    if [ -f "$TARGET_DIRECTORY/$artifact" ]; then
        cp "$TARGET_DIRECTORY/$artifact" "$DESTINATION_DIRECTORY/"
    else
        echo "[copy-linux-runtime] missing artifact (skipped): $artifact" >&2
    fi
done

# ---- CEF ランタイムを cef-dll-sys のビルド出力から拾う ----
# アーキテクチャ名はワイルドカードで受ける (x86_64 / aarch64 の双方で通る)。
CEF_DIRECTORY=$(ls -d "$TARGET_DIRECTORY/build/cef-dll-sys-"*/out/cef_linux_* 2>/dev/null | head -1)
if [ -z "$CEF_DIRECTORY" ]; then
    echo "[copy-linux-runtime] CEF ランタイムが見つからないのでスキップ" >&2
    exit 0
fi

# 共有ライブラリ (Chromium / Angle / SwiftShader / Vulkan)。
# chrome-sandbox は settings.no_sandbox = 1 のため配置しない。
for library in libcef.so libEGL.so libGLESv2.so libvk_swiftshader.so libvulkan.so.1; do
    if [ -f "$CEF_DIRECTORY/$library" ]; then
        cp "$CEF_DIRECTORY/$library" "$DESTINATION_DIRECTORY/"
    else
        echo "[copy-linux-runtime] missing runtime library (skipped): $library" >&2
    fi
done

# リソース (V8 snapshot / ICU / pak / SwiftShader manifest)。
# Windows にある snapshot_blob.bin は Linux 配布物には存在しない。
for resource in icudtl.dat v8_context_snapshot.bin resources.pak \
                chrome_100_percent.pak chrome_200_percent.pak vk_swiftshader_icd.json; do
    if [ -f "$CEF_DIRECTORY/$resource" ]; then
        cp "$CEF_DIRECTORY/$resource" "$DESTINATION_DIRECTORY/"
    fi
done

# locales/ は呼び出し元が .meta を保持したい場合、呼ぶ前後で自前に退避・復元すること。
if [ -d "$CEF_DIRECTORY/locales" ]; then
    rm -rf "$DESTINATION_DIRECTORY/locales"
    cp -r "$CEF_DIRECTORY/locales" "$DESTINATION_DIRECTORY/locales"
fi

echo "[copy-linux-runtime] done -> $DESTINATION_DIRECTORY"
