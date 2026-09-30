# ログ

リリース後の不具合 (ブラウザが止まった、落ちた) を報告者の手元のファイルから調べられるよう、
client・server・CEF 本体が常にファイルへログを書く。

## 保存先

保存先はライブラリではなく利用側が決める (アプリごとに分け、報告者が見つけられる場所にするため)。

| 利用側 | 既定の保存先 |
|---|---|
| Unity (Editor / プレイヤー) | `Application.persistentDataPath/CefUnity/Logs`<br>Windows: `%USERPROFILE%\AppData\LocalLow\<会社名>\<製品名>\CefUnity\Logs`<br>macOS: `~/Library/Application Support/<会社名>/<製品名>/CefUnity/Logs` |
| CefUnity.Viewer | Windows: `%LOCALAPPDATA%\CefUnity.Viewer\Logs` <br>macOS: `~/Library/Application Support/CefUnity.Viewer/Logs` |
| CefUnity.Harness | 実行ファイルの隣の `logs/` |

Unity では `CefLogDirectory` (Runtime アセンブリ) が起動時に `CefRuntime.DefaultLogDirectory` を
入れるので、利用側は何もしなくてよい。変えたいときは `CefRuntime.Initialize(logDirectory: ...)` で渡す。
実際に書いている場所は `CefRuntime.LogDirectory` で取れる。保存先が無い (Core を直接使い
`logDirectory` も `DefaultLogDirectory` も渡さない) ときはファイルに書かない。

## ファイル

起動ごとに別ファイルを作る。名前は `<種類>-<UTC 時刻>-<pid>.log` で、名前順 = 古い順。

| 種類 | 書くプロセス | 内容 |
|---|---|---|
| `client-*` | Unity (ネイティブプラグイン) | Initialize / Shutdown、server の起動・喪失・再起動、ブラウザの生成、FFI の panic |
| `server-*` | cef-unity-server | CEF の初期化、ブラウザの生成・遷移 (`load_url`)、レンダラーの終了理由、GPU プロセス障害からの作り直し、panic |
| `cef-*` | CEF 本体 (server・レンダラー・GPU プロセスが同じファイルへ追記) | Chromium のログ |

- server は復旧で起動し直すたびに新しいファイルを作る。落ちた server のログは消えない
- 種類ごとに新しい 10 個を残し、古いものは起動時に消す
- 1 ファイル 16 MiB を超えたら以降の行を捨てる (異常系でエラーが出続けてもディスクを食い潰さない)。
  CEF 本体のファイルは Chromium が書くのでこの上限は効かない
- 行は 1 行ずつ直接書き込む (バッファしない)。プロセスが落ちても直前の行まで残る
- Rust の panic は発生箇所とバックトレースを書く

## レベル

| レベル | 書く条件 | 内容 |
|---|---|---|
| Essential | 常に | 起動・終了・障害・復旧・エラー。CEF 本体は WARNING 以上 |
| Verbose | `Initialize(verboseLog: true)` (サンプルでは `_enableLog`) | 毎フレーム級の診断・paint 統計 (`STATISTICS` 行)。CEF 本体は VERBOSE |

Verbose の行は `CefRuntime.GetLogs()` でも取れる (Harness が統計を読むのに使う)。

## 不具合報告を受けたとき

報告者に上の保存先のフォルダを zip で送ってもらう。見る順番:

1. `client-*`: `server lost` / `recovery:` の行で、server が落ちたか・復旧したか・諦めたか
2. `server-*`: `on_render_process_terminated` (status と error_code) で、レンダラーが落ちたか・
   再読み込みを止めたか。`compositor stopped responding` なら GPU プロセスの障害
3. `cef-*`: 同じ時刻の前後にある Chromium のエラー

`status` は `cef_termination_status_t` (0 = 正常終了、1 = 異常終了、2 = kill / クラッシュ、
3 = クラッシュ、4 = 起動失敗、5 = メモリ不足、6 = 整合性エラー)。
