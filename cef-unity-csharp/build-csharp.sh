#!/usr/bin/env bash
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
DEST="$HERE/../cef-unity-unityproject/Assets/CefUnity/Plugins"
# Platform=x64 が環境変数で設定されたマシン (Visual Studio 環境) では出力が
# bin/x64/Release/ になるため、AnyCPU を明示して出力先を固定する。
dotnet build "$HERE/CefUnity.Core/CefUnity.Core.csproj" -c Release -p:Platform=AnyCPU -v quiet
SRC="$HERE/CefUnity.Core/bin/Release/netstandard2.1/CefUnity.Core.dll"
cp "$SRC" "$DEST/CefUnity.Core.dll"
echo "copied CefUnity.Core.dll -> $DEST"
