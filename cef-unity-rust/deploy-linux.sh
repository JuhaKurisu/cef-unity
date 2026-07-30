#!/bin/bash
# Linux x86_64 用のビルド + Unity プラグインへのコピー。
# libcef_unity_rust.so (cdylib), cef-unity-server, cef-unity-rust-helper,
# および CEF ランタイム (libcef.so, *.pak, *.dat, *.bin, locales/ 等) を一括配置する。
#
# ホストアーキテクチャ向けにしかビルドしない。Unity にはデスクトップ Linux arm64 の
# ターゲットが無いため、arm64 の成果物は CI がリリース zip として出す (Assets には置かない)。
set -e

SCRIPT_DIRECTORY="$(cd "$(dirname "$0")" && pwd)"

HOST_ARCHITECTURE="$(uname -m)"
if [ "$HOST_ARCHITECTURE" != "x86_64" ]; then
    echo "ERROR: deploy-linux.sh は x86_64 ホスト専用です (検出: $HOST_ARCHITECTURE)。" >&2
    echo "       Plugins/linux-x64/ に誤ったアーキテクチャの成果物を置かないため中止します。" >&2
    exit 1
fi

DESTINATION_DIRECTORY="$SCRIPT_DIRECTORY/../cef-unity-unityproject/Assets/CefUnity/Plugins/linux-x64"

echo "[deploy-linux] cargo build --release"
cd "$SCRIPT_DIRECTORY"
cargo build --release

mkdir -p "$DESTINATION_DIRECTORY"

# ---- locales/ の Unity .meta を退避 ----
# 共有スクリプトは .meta を関知しないため、退避・復元は Unity 配置固有の処理として
# ここで行う。旧ファイルの残留を避けるため共有スクリプトが locales をディレクトリごと
# 作り直すので、その前後で .meta を保存する。
LOCALES_DESTINATION="$DESTINATION_DIRECTORY/locales"
META_TEMPORARY=""
if [ -d "$LOCALES_DESTINATION" ]; then
    META_TEMPORARY="$(mktemp -d)"
    find "$LOCALES_DESTINATION" -maxdepth 1 -name '*.meta' -exec cp {} "$META_TEMPORARY/" \; 2>/dev/null || true
fi

bash "$SCRIPT_DIRECTORY/copy-linux-runtime.sh" "$DESTINATION_DIRECTORY" release

# ---- 退避した .meta を復元 ----
if [ -n "$META_TEMPORARY" ] && [ -d "$META_TEMPORARY" ]; then
    find "$META_TEMPORARY" -maxdepth 1 -name '*.meta' -exec cp {} "$LOCALES_DESTINATION/" \; 2>/dev/null || true
    rm -rf "$META_TEMPORARY"
fi

# ---- 成果物の欠落を検査する ----
# 共有スクリプトは欠落を警告のみで通すため、Unity 配置では厳格に検査する。
for required in libcef_unity_rust.so cef-unity-server cef-unity-rust-helper libcef.so; do
    if [ ! -f "$DESTINATION_DIRECTORY/$required" ]; then
        echo "ERROR: deploy failed: $required が $DESTINATION_DIRECTORY にコピーされていません" >&2
        exit 1
    fi
done

echo "[deploy-linux] done -> $DESTINATION_DIRECTORY"
