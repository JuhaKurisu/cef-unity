# CLAUDE.md

## ビルド・テスト・デプロイ

Rust 側のコードに変更を加えた場合、以下を必ず実行すること:

### 1. ビルドとテスト

```bash
cargo build
cargo test -p cef-unity-ipc
```

### 2. C# 側の同期更新

FFI 関数の追加・変更時は以下の **両方** を更新する:

- **cef-unity-csharp** (Sandbox 用): `cef-unity-csharp/Interop/NativeMethods.g.cs` + `CefUnity.cs`
- **cef-unity-unityproject** (Unity 用): `cef-unity-unityproject/Assets/CefUnity/Interop/NativeMethods.g.cs` + `CefUnity.cs`

両方のファイルは同じ内容を維持すること (namespace の違いに注意: csharp 側は `Interop`、Unity 側は `CefUnity.Interop`)。

### 3. Unity プロジェクトへのデプロイ

ビルド成果物を Unity プロジェクトにコピーする:

```bash
cp target/debug/libcef_unity_rust.dylib ../cef-unity-unityproject/Assets/CefUnity/Interop/Plugins/osx-arm64/
cp target/debug/cef-unity-server ../cef-unity-unityproject/Assets/CefUnity/Interop/Plugins/osx-arm64/cef-unity-server.app/Contents/MacOS/
```
