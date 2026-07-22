# Rust CI (GitHub Actions) 第1弾 設計書

日付: 2026-07-22
対象: `.github/workflows/rust-build.yml` (新規)

## 背景

リポジトリを public 化した (Actions 標準ランナーが無料・無制限)。現状 CI が無く、
プラットフォーム別バイナリの鮮度がローカル作業に依存している — 実害として
`win-x64/cef_unity_rust.dll` が 5月14日ビルドのまま放置され、7月に追加した FFI
(音声4本 + scroll_monitor 4本) を含まない。CI で mac/win を毎回フルビルドし、
バイナリの鮮度とビルド可能性を機械的に保証する。

## スコープ

**やること (第1弾)**
- mac (arm64) / windows の Rust フルビルド + ユニットテスト + アーティファクト添付
- 常にフルビルド (ユーザー決定 2026-07-22。PR でも省略しない)

**やらないこと (後続)**
- Unity EditMode テスト (第2弾 — Unity ライセンス Secrets が必要)
- タグ→ GitHub Release 作成・Plugins 自動更新 (第3弾)
- target ディレクトリキャッシュ / sccache (遅さが実害になったら B 案として検討。
  GitHub キャッシュ 10GB/リポジトリ上限と mac+win の target 合計が競合するリスクがあるため v1 では見送り)

## ワークフロー設計

ファイル: `.github/workflows/rust-build.yml`

### trigger
- `push` (branches: main) / `pull_request` / `workflow_dispatch`
- `paths` フィルタ: `cef-unity-rust/**` と `.github/workflows/rust-build.yml` の変更時のみ
  (Unity 側のみの変更でフルビルド ×2 を回さない)

### ジョブ1: build-mac (runs-on: macos-14 = arm64)
1. checkout
2. Rust stable toolchain
3. `~/.cargo` (registry + git) キャッシュ (キー: OS + Cargo.lock ハッシュ)
4. `cargo test --workspace --lib --release` — **ユニットテストのみ**。
   `crates/server/tests/` の CEF 統合テスト (実プロセス起動・シングルトンロック問題)
   と `#[ignore]` の実機テスト (au_smoke 等) は `--lib` で自然に除外される
5. `./deploy.sh` を実行 (既存スクリプト再利用: `cargo build --release` + server.app
   バンドル + ad-hoc codesign + `cef-unity-unityproject/Assets/CefUnity/Interop/Plugins/osx-arm64/` へ配置)
6. `Plugins/osx-arm64` を zip → artifact **`cef-unity-macos-arm64`** (retention 90日)

### ジョブ2: build-win (runs-on: windows-latest)
mac と独立並列 (片方の失敗が他方をブロックしない)。
1. checkout
2. Rust stable toolchain
3. `~/.cargo` キャッシュ
4. `cargo test --workspace --lib --release`
5. `cargo build --release`
6. 診断ステップ: `target/release/` の exe/dll 一覧と `cef-dll-sys` の out ディレクトリ
   (cef_windows_*) の内容をログ出力する。**Windows のバンドル構成は 2026年5月以来
   未検証のため、初回実行はこの診断込みで仕様とする** (結果を見て収集対象を確定する)
7. 暫定の収集対象を zip → artifact **`cef-unity-windows-x64`**:
   - `target/release/cef_unity_rust.dll`
   - `target/release/cef-unity-server.exe` (存在すれば)
   - `target/release/cef-unity-rust-helper.exe` (存在すれば)
   - CEF 配布物 (`target/release/build/cef-dll-sys-*/out/cef_windows_*` の
     dll / .pak / resources / locales)
   - 存在しないものはスキップし、スキップした旨をログに残す (silent drop しない)

## エラーハンドリング / 制約

- CEF ダウンロード (~1GB/プラットフォーム) はネットワーク起因で失敗し得る →
  ダウンロードを引き起こす最初の cargo 実行 (= ユニットテストステップ) に 1 回のリトライを付ける
- GitHub ランナー環境はローカルで再現できないため、**実装後は push → 実行結果を
  gh CLI で監視 → 修正の反復**を前提とする (初回から green を保証しない)
- fork からの PR は Secrets を持たないが、本ワークフローは Secrets 不使用なので影響なし
- コミット済みバイナリ (Plugins/ 配下) との同期は第3弾のスコープ。第1弾では
  artifact として取得できることまでを保証する

## 成功基準

1. mac ジョブ green + artifact に `libcef_unity_rust.dylib` と `cef-unity-server.app`
   一式が含まれる (nm で `cef_scroll_monitor_start` 等の新 FFI を確認できる)
2. win ジョブがビルド green (バンドル内容は初回診断の結果を見て確定)
3. ユニットテストが両 OS で pass

## 実行結果 (2026-07-22, 実装後追記)

- 2 反復で両ジョブ green (mac は初回一発、win は SIGPIPE 対策のみ)
- **win バンドル確定レイアウト**: トップに `cef_unity_rust.dll` / `cef-unity-server.exe` /
  `cef-unity-rust-helper.exe`、`cef/` にランタイム一式 (libcef.dll, chrome_elf.dll,
  *.pak, icudtl.dat, v8_context_snapshot.bin, locales/ 220 ファイル。フラット構造・SDK 除外)
- **成果: Windows は server 含め全バイナリがビルド可能と機械的に確認** (2026-05 以来の未検証状態が解消)
