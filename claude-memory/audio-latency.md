---
name: audio-latency
description: CEF→Unity 音声パイプラインの遅延 実測ログ(2026-06-20)。管理リング滞留が支配的(平均273ms)
metadata: 
  node_type: memory
  type: project
  originSessionId: f8fbce93-c10c-49c6-b179-0b883fcae794
---

# CEF→Unity 音声遅延 実測ログ (2026-06-20 計測)

CEF の音声を Unity 側で再生する機能(`AudioHandler` → 共有メモリリング → `CefAudioOutput`)の
遅延を実測した記録。計装は `CefAudioOutput.LogDiagnostics`(`[CefAudio-LAT]` ログ, 1秒ごと)。
関連: [[cef-external-begin-frame]](映像側の遅延対策)。

## 計測条件
- テスト信号: 440Hz サイン波(WebAudio, gain=0.2)を data: URI で読み込み
- Unity targetFrameRate=60、出力 44.1kHz、CEF ストリーム 48kHz x2ch
- 58サンプル(定常状態)

## 実測結果(段階別)
| 段階 | 項目 | 実測値 |
|------|------|--------|
| ① CEF キャプチャ | frames_per_buffer=1024 @48kHz | 21.3ms(固定) |
| ④ メインスレッド ポーリング | フレーム間隔 60fps | 16.6〜16.7ms |
| ⑤ **管理リング滞留量** | `_ringWrite-_ringRead` | **平均273 / 中央値277 / 最小75 / 最大484ms**(p10=107, p90=441) |
| ⑧ Unity DSP ミキサ | GetDSPBufferSize = **1024×4段** @44.1kHz | 92.9ms(固定) |
| | **内部合計(①+④+⑤+⑧)** | **平均≈404ms / 範囲205〜563ms** |

⑥⑦ Unity ストリーミングAudioClip 先読み と ⑨ OS/HW 出力は未計測(内部合計に**含まず**、さらに上乗せ)。

## 重要な発見
- **支配的遅延源は ⑤ 管理リング**(平均273ms)。事前推定(~8〜33ms)の約10倍で、推定は誤りだった。
- リングが容量上限(0.5秒)に張り付きかけ(最大484ms)→ **オーバーフローで最古サンプル破棄=音飛び発生中**の可能性大。
- 滞留量が 75〜484ms で激しく振動(sawtooth)→ producer(CEF実時間48kHz)は steady、consumer(Unity の PCMReaderCallback)がバースト排出 → 遅延+深刻なジッタ。
- ⑧ DSP は numBuffers=**4** と判明、固定92.9ms。
- 根本原因: `CefAudioOutput` の管理リングに**目標滞留量の制御が無く、上限まで溜めてから破棄する**設計。

## A 実施結果(2026-06-29 修正+再計測)— 完了
`CefAudioOutput` に **目標滞留量(`_targetLatencySeconds=0.04`)+ 上限超過で最古を捨てて目標へ snap back する drift 補正(`_maxLatencySeconds=0.08`)** を追加。
従来は容量上限(0.5秒)到達まで破棄しない設計だったのを、能動的に目標へ戻すよう変更。

| 段階 | 修正前 | **修正後** |
|------|--------|-----------|
| ⑤ 管理リング滞留 | 平均273 / 最大484ms | **平均50 / 中央43 / 最小21 / 最大64 / p90 61ms** (n=40) |
| 内部合計(①+④+⑤+⑧) | 平均≈404 / 範囲205〜563 | **平均≈182 / 範囲152〜222ms** (n=47) |

- ⑤ を **273→50ms(-82%)**、内部合計を **404→182ms(-55%)** へ削減。
- **最大が 484→64ms** に下がり、容量上限への張り付き(=オーバーフロー音飛び)が解消。音声経路は健全(rms=0.141/peak=0.200=0.2 gain サイン一致、spec 431Hz)。
- 残る支配項は **⑧ DSP 92.9ms(1024×4段, 固定)**。次に削るならここ(選択肢 C)。

## ぶつ切り(choppiness)修正 — 完了 (2026-06-29)
A の target=80ms 化でぶつ切りが発生。計測で**真因を特定**:
- producer(CEF→SHM→ReadAudio)は ~1024 フレーム量子で sum≈1024ms/s と概ね健全。
- **真因は consumer**: ストリーミング AudioClip の `PCMReaderCallback` が先読みバッファ管理の都合で
  消費レートを **~2秒周期で 800ms/s ↔ 1200ms/s に振動**(計装 `[CefAudio-CONS]` で実測)。
  1200ms/s 局面で供給を上回りリングが谷で枯渇 → アンダーラン → ぶつ切り。
  元の 0.5s バッファはこの波を吸収できていただけ(谷でも min75ms 残っていた)。

**根本修正**(`CefAudioOutput.cs` + 新規 `CefAudioRing.cs`):
1. consumer を **`OnAudioFilterRead`**(DSP ブロックごとに一定ペースで呼ばれる)へ変更。
   先読みの波が消滅 → 実測 calls/s=43, **998〜1022ms/s 一定**, maxBlock=1024 固定。
2. リングを**非同期サンプルレート変換**(線形補間 + 滞留量誤差で消費レートを ±2% steering)化。
   クリックもアンダーランも出さず滞留量を目標へ収束。
3. 出力検証は `AudioListener.GetSpectrumData` が OnAudioFilterRead 加算分を拾えない(タップ順序/Mute)
   ため、**ミックスへ加算した音声の RMS を直接計測**(`outRms`)に変更 → outRms=0.28 で出力経路を確認。

**修正後 実測**: 定常で **ringOcc 目標80ms 近傍(70〜110ms)安定 / underrun=0 / overflow=0 を約18秒連続**。
内部合計 sum≈230〜270ms(元404ms)。残存: 大きなフレームスパイク(GC 等, >targetの滞留を一括ドレイン)時のみ
一時アンダーラン → 即回復。耐性を上げるなら `_targetLatencySeconds` を上げる(遅延とトレードオフ)。

**テスト**: `CefAudioRing` を UnityEngine 非依存の純 C# に分離し、`Runtime.Tests` に NUnit EditMode テスト
(`CefAudioRingTests.cs`)を整備。uloop の run-tests/test-runner はセキュリティ設定でブロックのため、
同一ソースを plain .NET の NUnit プロジェクトにリンクして `dotnet test` で実行 → **7/7 合格**
(連続性=クリック検出, アンダーラン, オーバーフロー, レートdrift収束, 補間正当性)。

## C 実施結果 — DSP バッファ削減 (2026-06-29) 完了
⑧ Unity DSP ミキサを **1024×4(92.9ms)→ 256×4(23.2ms)** へ(Best latency 相当)。
- 設定: `ProjectSettings/AudioManager.asset` の `m_DSPBufferSize`/`m_RequestedDSPBufferSize` を 256 に。
  **ただしエディタ実行中はプロジェクト設定が起動時しか反映されず**、ファイル編集は通常終了で
  メモリ上の旧値に上書きされる恐れもある。そこで `CefUnityBrowserSample.ApplyAudioDspBufferSize()` で
  **実行時に `AudioSettings.Reset(dspBufferSize=256)`** を音声シンク生成前に呼んで確実に適用
  (`_audioDspBufferSize` フィールドで可変, 0 でプロジェクト設定のまま)。ログ `DSP buffer 1024 -> 256` で確認。
- 実測: **内部合計 250→約160ms(151〜174ms で安定)**, underrun/overflow=0 維持, outRms=0.141。

### 内部遅延の現状内訳(C 後・定常)
| 段階 | 値 |
|------|----|
| ① CEF キャプチャ(frames_per_buffer=1024@48k) | 21.3ms |
| ④ メインスレッド ポーリング(60fps) | ~17ms |
| ⑤ リング滞留(target=80ms) | ~90〜110ms ← **現状の最大要因** |
| ⑧ DSP(256×4@44.1k) | 23.2ms |
| **内部合計** | **約160ms**(元404ms) |

### バインド曖昧さの修正(重要)
OnAudioFilterRead を AudioListener を持つ GameObject(Main Camera)上に置くと、AudioSource と
どちらにバインドされるか**非決定的**になり Unity が警告 + 実行ごとに音が出たり出なかったり不安定化。
→ consumer を **専用子 GameObject `CefAudioSink`(AudioListener 無し・AudioSource のみ)** に分離して根絶。
`CefAudioOutput` は producer(ReadAudio ポーリング + リング書込)とリング所有のみ、`CefAudioSink` が
OnAudioFilterRead で consumer。

## さらなる低遅延化 調査(2026-07-02 再計測)— 未実装・計画確定
ベースライン再計測: 内部合計 148〜190ms(平均~160ms)で 6/29 から変化なし。

### 新知見(削減の鍵)
- ⑤ リング滞留が target(80ms)を常時 6〜47ms 超過して ~100ms で浮動。原因は producer の量子性:
  CEF パケット 1024 フレーム(21.3ms)単位 + メインスレッドヒッチで **1回最大 3072 フレーム(64ms)バースト**
  (`[CefAudio-PROD] max=3072` 実測)。steering ±2% は 20ms/s しか排出できず超過が滞留。
- **target=80ms でもアンダーランするスパイクが実在**(実測中 1 回: underrun/s=1847, occ 27ms へ陥没→回復)。
  → **現構造のまま target を下げるのは逆効果(行き止まり)**。
- consumer(OnAudioFilterRead)は完全安定(calls/s≈175, maxBlock=256 固定)。
  **不安定要素は「producer がメインスレッドにいること」だけ**。

### 削減プラン(推奨順: A→再計測→B→C, 各段で計測)
- **A. producer をオーディオスレッドへ移動(-70〜90ms, 構造的・最大効果)**:
  `CefAudioSink.OnAudioFilterRead` 内で `Browser.ReadAudio`(SHM 読み)を直接呼ぶ。
  Rust 側 `AudioShmReader.read` は atomics+memcpy のみ(ロック/アロケなし, µs)で audio-thread safe。
  ④ 消滅(-17ms)+ ⑤ target がスパイク非依存化 80→~30ms + ring が同一スレッド化。
  **リスク**: `Browser.Dispose` とコールバックの UAF 競合 → detach フラグ+コールバック実行中ガードで
  Dispose 待機のライフサイクル制御が必須。期待: 160→**75〜90ms**。
- **B. `frames_per_buffer` 1024→512(-10ms + A と相乗)**: ① 21.3→10.7ms、パケット量子半減で
  A 併用時 target ~20ms 可。Rust 再ビルド+deploy+Editor 再起動。期待(A+B): **55〜65ms**。
- **C. DSP 256→128(-11.6ms)**: `_audioDspBufferSize=128`。macOS クラックル要実測。
  期待(A+B+C): **45〜55ms**。
- **D. 音響 end-to-end 計測**: 外側(Chromium audio service 内部 推定10〜20ms + CoreAudio HW ~10〜30ms)
  は未計測。内部 50ms 台まで詰めるなら定量化の価値大。

**結論: 内部 160ms → 約50〜60ms(-65%)が現実的到達点。鍵は target 調整ではなく A の構造変更。**

## A の実装設計(2026-07-02 確定・未実装)

### 変更の骨子
producer(SHM→ring の取り込み)をメインスレッド Update から **`CefAudioSink.OnAudioFilterRead`(オーディオスレッド)の先頭**へ移す。
コールバックは「①SHM pull→ring.Write → ②ring.Read(SRC)→ミックス加算」の順で両方を行う(同一スレッド化)。

### CefAudioSink.cs の変更
1. `Configure(...)` に `Browser browser` と pull 用スクラッチサイズを追加。`_pullScratch`(`MaxPullFrames * 8` の float[])を**事前確保**(オーディオスレッドでのアロケ禁止)。
2. `OnAudioFilterRead` 先頭に pull を追加:
   ```csharp
   Interlocked.Increment(ref _pullActive);
   try {
       if (!_detached) {
           int got = _browser.ReadAudio(_pullScratch, MaxPullFrames, out int ch);
           if (got > 0) {
               if (ch != _srcChannels && ch > 0) { _chChanged = true; }  // フラグのみ。Unity API 禁止
               else _ring.Write(_pullScratch, 0, got);
           }
       }
   } finally { Interlocked.Decrement(ref _pullActive); }
   ```
   その後に既存の `ring.Read` → ミックス加算。
3. **Dispose UAF ガード**(最重要):
   - `volatile bool _detached` + `int _pullActive`(Interlocked カウンタ)。
   - `public void DetachAndWait()`(メインスレッドから呼ぶ):
     `_detached = true;` → `while (Interlocked.CompareExchange(ref _pullActive, 0, 0) != 0) Thread.SpinWait(64);`
   - 順序の正しさ: Interlocked はフルフェンス。detached=true 後に active==0 を観測できたら、
     以降のコールバックは FFI 前に必ず `_detached=true` を見る → `Browser.Dispose` 後の FFI は起きない。
     コールバックは µs オーダーなので spin は実質即時。
4. 計装: PROD 統計(pulls/s, framesGot min/max/sum, rms/peak)を sink 側へ移し、既存 `_statsLock`+`SnapshotStats` パターンに統合。per-sample RMS 計算は `CefLog.Enabled` 時のみ(consumer 側と同様)。

### CefAudioOutput.cs の変更
1. `PullFromBrowser()` を削除(audio スレッドへ移管)。`Update()` に残すのは:
   - ストリーム開始前の `TryInitStream()`(`TryGetAudioFormat` はヘッダ読みのみで read カーソルを触らない → メインスレッドから安全)
   - `LogDiagnostics()`(sink の SnapshotStats を読む)
   - `_chChanged` フラグ監視 → 立っていたら `DetachAndWait()` → ring/sink 再構築(Unity API はメインスレッドで)
2. `TryInitStream()` で sink に Browser も渡す。**ストリーム開始後は SHM の read カーソルを触るのは audio スレッドのみ**
   (AudioShmReader.read はカーソルを持つため呼び出し元は常に1スレッドに限定。2箇所から呼ぶ実装は禁止)。
3. `OnDisable()` / Browser setter(null 代入時)で `_sink.DetachAndWait()` を呼ぶ。
4. `_targetLatencySeconds` 0.08 → **0.03**(A 後)。B 実施後は 0.02。

### CefUnityBrowserSample.cs の変更
teardown(現在 `:395-398` の `_audioOutput.Browser = null; enabled = false;`)を
**`_audioOutput.DetachAndWaitBeforeDispose()`(内部で sink の DetachAndWait)→ その後 `_browser.Dispose()`** の順に。
現状の順序でも Browser=null 代入だけでは audio スレッドが古い参照を保持し得るため、**待機が必須**。

### FFI 安全性の根拠(調査済み)
- `cef_unity_read_audio` → `AudioShmReader.read`: atomics+memcpy のみ、ロック/アロケ/ブロッキングなし → audio-thread safe。
- `handle_to_ref` は `&mut ClientBrowserInstance` を作るが、audio スレッドは `audio_shm` フィールドのみ、
  メインスレッドの他 FFI は `shm`/`browser_id` のみでフィールド非交差。既存コードと同等の実用上安全。
- `cef_unity_get_audio_format` はヘッダ Acquire 読みのみ → メインスレッド並行呼び出し可。

### B の実装(A 後)
`server.rs` の `p.frames_per_buffer = 1024` → **512**(1行)。`bash deploy.sh` → **Editor 再起動必須**。
target を 0.02 へ。コールバック頻度 2 倍だが処理 µs で影響軽微。

### C の実装(B 後)
`CefUnityBrowserSample._audioDspBufferSize` のデフォルト 256 → **128**(シーンには未シリアライズなので
スクリプトのデフォルト変更で効く)。クラックル(まれな DSP 割り込み遅れ)が出ないか実聴+underrun/s で確認。

### 検証手順(各段共通)
1. `uloop compile` → Play → `$TMPDIR/cef_load_url` に 440Hz トーン data:URI を書いて遷移。
2. `[CefAudio-LAT]` で ringOcc/sum を 30 秒以上、`underrun/s=0 overflow/s=0` 維持を確認。
3. スクロール等の操作負荷をかけてスパイク耐性確認(A 後はメインスレッドスパイクの影響が消えているはず)。
4. `CefAudioRing` の NUnit(dotnet test, 7 テスト)は ring API 不変なのでそのまま回帰に使う。

## 関連ファイル
- `cef-unity-unityproject/Assets/CefUnity/Runtime/CefAudioOutput.cs`(管理リング・計装)
- `cef-unity-rust/crates/server/src/server.rs`(AudioHandler, frames_per_buffer=1024)
- `cef-unity-rust/crates/ipc/src/lib.rs`(AudioShm リング, AUDIO_RING_FRAMES=48000=1秒)
