#!/usr/bin/env bash
# Mac の作業ツリーを WSL のビルド用コピーへ同期する。
# 真実源は Mac 側。WSL 側は使い捨てのビルドサンドボックスで git 管理しない。
set -euo pipefail
SOURCE_DIRECTORY="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/"
DESTINATION="wsl:~/cef-unity-build/"
rsync -az --delete \
  --exclude '.git/' \
  --exclude 'target/' \
  --exclude 'obj/' \
  --exclude 'bin/' \
  --exclude 'build-mac/' \
  --exclude 'cef-unity-unityproject/Assets/CefUnity/Plugins/' \
  --exclude 'cef-unity-unityproject/Library/' \
  "$SOURCE_DIRECTORY" "$DESTINATION"
echo "[sync] done -> $DESTINATION"
