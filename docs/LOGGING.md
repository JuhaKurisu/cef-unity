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
- レベルが None のときはどのファイルも作らない

## レベル

利用側が `CefRuntime.Initialize(logLevel: ...)` で選ぶ (既定は Essential。サンプルでは `_logLevel`)。
選んだレベル以下の行を書く。

| レベル | client / server | CEF 本体 |
|---|---|---|
| None | 何も書かない (ファイルも作らない) | 書かない |
| Essential | 起動・終了・障害・復旧・エラーと、プロセスごとのメモリの推移 | WARNING 以上 |
| Verbose | Essential に加えて毎フレーム級の診断・paint 統計 (`STATISTICS` 行) | VERBOSE |

Verbose の行は `CefRuntime.GetLogs()` でも取れる (Harness が統計を読むのに使う)。

## メモリの推移

Essential 以上のとき、server が 60 秒ごと (起動直後に 1 回目) に `server-*` へ 1 行書く。
対象は server 自身と、その子孫のプロセスすべて (CEF のレンダラー・GPU プロセス・ユーティリティなど)。
どのプロセスがいつから増えたかを、報告者のログだけで追うためのもの。全 OS 共通。

```
memory: server pid=33312 resident=115.9MiB virtual=79.9MiB | gpu-process pid=33580 resident=85.4MiB virtual=117.7MiB | renderer pid=8052 resident=49.8MiB virtual=24.2MiB | renderer pid=33440 resident=26.7MiB virtual=13.1MiB | utility pid=1008 resident=16.0MiB virtual=7.4MiB | utility pid=32244 resident=26.9MiB virtual=11.7MiB
```

- (上は Windows の実例)
- 種類はコマンドラインの `--type=` (無ければ `unknown`)。server 自身は `server`。
  起動直後の 1 回目は、起動途中のプロセスが `unknown` で出ることがある
- `resident`: 物理メモリ上の使用量 (Windows: working set、macOS / Linux: RSS)
- `virtual`: Windows では private bytes (リソースモニターの「プライベート」)、
  macOS / Linux では仮想アドレス空間の大きさ
- システム全体のプロセス一覧を走査するので、pump を止めないよう専用スレッドで取る

## 不具合報告を受けたとき

報告者に上の保存先のフォルダを zip で送ってもらう。見る順番:

1. `client-*`: `server lost` / `recovery:` の行で、server が落ちたか・復旧したか・諦めたか
2. `server-*`: `on_render_process_terminated` (status と error_code) で、レンダラーが落ちたか・
   再読み込みを止めたか。`compositor stopped responding` なら GPU プロセスの障害
3. `cef-*`: 同じ時刻の前後にある Chromium のエラー
4. メモリが増えたという報告なら `server-*` の `memory:` 行で、どのプロセスがいつから増えたか

`status` は `cef_termination_status_t` (0 = 正常終了、1 = 異常終了、2 = kill / クラッシュ、
3 = クラッシュ、4 = 起動失敗、5 = メモリ不足、6 = 整合性エラー)。
