#!/usr/bin/env bash
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
DEST="$HERE/../cef-unity-unityproject/Assets/CefUnity/Plugins"
dotnet build "$HERE/CefUnity.Core/CefUnity.Core.csproj" -c Release -v quiet
SRC="$HERE/CefUnity.Core/bin/Release/netstandard2.1/CefUnity.Core.dll"
cp "$SRC" "$DEST/CefUnity.Core.dll"
echo "copied CefUnity.Core.dll -> $DEST"
