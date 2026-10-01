# Windows で起動時に位置情報の許可ダイアログが出る件 (2026-10-01)

## 報告

Windows 11 Pro で位置情報サービスをオフにしている利用者が、moorestech の「ローカルでプレイ」を押したときに
「Windows とアプリに位置情報へのアクセスを許可しますか? cef-unity-server では、GPS や Wi-Fi などの信号を
使用するためのアクセス許可が必要です。」というダイアログを見た。どちらを押しても動作は続く。

## 原因

Windows 11 24H2 以降は、Wi-Fi の BSSID などを返す WLAN API (`WlanQueryInterface` 等) の呼び出しを位置情報への
アクセスとして扱い、位置情報がオフならダイアログを出す
([Changes to API behavior for Wi-Fi access and location](https://learn.microsoft.com/en-us/windows/win32/nativewifi/wi-fi-access-location-changes))。

cef-unity 自身は位置情報を求めていない。Chromium のキャスト機能 (MediaRouter) の `DiscoveryNetworkMonitor` が
起動直後に `WlanQueryInterface` で接続中の SSID を取りに行くのが引き金
(Chromium 側の対策コミット [659748cd](https://chromium.googlesource.com/chromium/src/+/659748cd74cab6a1ef64ecdab818997f60af3fac)、
CefSharp でも同じ報告 [discussion #5064](https://github.com/cefsharp/CefSharp/discussions/5064))。

## 実機での確認 (windows-desktop, Windows 11 25H2, Wi-Fi 接続, 位置情報は許可)

位置情報へのアクセスは `HKCU\SOFTWARE\Microsoft\Windows\CurrentVersion\CapabilityAccessManager\ConsentStore\location\NonPackaged`
に実行ファイルのパスごとに記録される (`LastUsedTimeStart` / `LastUsedTimeStop`)。許可している環境ではダイアログは
出ないが、記録の有無でアクセスしたかが分かる。

- これまで起動した全ての `cef-unity-server.exe` (moorestech、Steam 版、harness、viewer) に記録があった。
  いずれも起動の約 1 秒後に 1 ms だけ。example.com を開くだけの harness でも付くので、ページ内容とは無関係
- harness の `smoke` を実行パスを変えて比較 (キャッシュの競合を避けるため `TEMP`/`TMP` を分けた):

| 構成 | 位置情報アクセスの記録 |
| --- | --- |
| v0.6.4 の server そのまま | あり |
| server を起動し直すだけのラッパー経由 (フラグなし) | あり |
| ラッパー経由で `--disable-features=MediaRouter` を付ける | なし (3 回) |
| 修正後の server (7b63218) | なし (3 回、`SMOKE_OK`) |

## 修正

`ServerApp::on_before_command_line_processing` で `--disable-features=MediaRouter` を付ける (全 OS)。
キャストは使っていない。macOS でも `smoke` は `SMOKE_OK`。
