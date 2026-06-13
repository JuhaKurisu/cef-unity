# IOSurface GPU コピー: 設計判断と教訓

## アーキテクチャ概要

```
CEF GPU compositor → IOSurface(src)
  → Metal blit → IOSurface(pool, dst)  [サーバープロセス]
    → Mach IPC (port 転送)
      → IOSurfaceLookupFromMachPort → sRGB texture view  [クライアント/Unity プロセス]
        → Unity レンダリング
```

全パスが GPU メモリ上で完結。CPU バッファコピーなし。

## 関連ファイル

| ファイル | 役割 |
|---------|------|
| `crates/server/src/iosurface_pool.m` | Metal GPU blit + IOSurface プール管理 |
| `crates/server/src/mach_iosurface.c` | Mach IPC (サーバー→クライアント IOSurface 転送) |
| `crates/server/src/server.rs` | `on_accelerated_paint` コールバック |
| `crates/client/src/metal_texture.m` | クライアント側 IOSurface 受信 + sRGB テクスチャ作成 |
| `crates/server/build.rs` | Obj-C/C コンパイル設定 |

## 最終設計: 同期 blit + waitUntilCompleted

```objc
[cmdBuf commit];
[cmdBuf waitUntilCompleted];  // ← 必須
return (void*)dst;            // 完了済み surface を直接返却
```

**コスト**: ~0.5ms/frame (60fps の 3%、実用上問題なし)

## 試行錯誤の記録と教訓

### 試行1: waitUntilCompleted (初期実装)
- **結果**: 正しく動作、~1.5ms (キャッシュなし時)
- **判断**: コスト削減のため最適化を試みた

### 試行2: waitUntilCompleted 完全削除
- **結果**: 0.03ms に高速化。しかし高解像度でフレームロールバック発生
- **原因**: GPU blit 未完了の IOSurface をクライアントが読んだ

### 試行3: waitUntilScheduled
- **結果**: ~1.5ms — waitUntilCompleted と同じコスト
- **原因**: IOSurface-backed テクスチャは "scheduled" 状態でも完全な GPU 同期が必要
- **教訓**: IOSurface 経由の場合、scheduled と completed のコスト差はない

### 試行4: パイプラインパターン (前フレームを返す)
- **設計**: 現フレームは async blit、前フレーム (16ms 前に submit 済み) を返却
- **結果**: ロールバック発生。CEF 起動時のバーストフレームで前フレームの blit が未完了
- **教訓**: 60fps の定常状態では安全だが、バースト時の安全性を保証できない

### 試行5: パイプライン + g_prev_cmd 安全チェック
- **設計**: 前フレームの commandBuffer の status を確認、未完了なら wait
- **結果**: **画面下半分にティアリング**。上半分は新フレーム、下半分は旧フレーム
- **原因**: クロスプロセス IOSurface 読み取りでは `waitUntilCompleted` が唯一の正しい同期手段。status チェックだけでは IOSurface のメモリ可視性が保証されない
- **教訓**: Metal commandBuffer の status プロパティは内部状態であり、クロスプロセスのメモリ可視性とは無関係

### 試行6: 同期 blit に回帰 (最終)
- **結果**: ティアリング解消、安定動作
- **コスト**: ~0.5ms (テクスチャキャッシュにより初期の 1.5ms から改善)

## 確定した設計原則

### 1. クロスプロセス IOSurface は `waitUntilCompleted` 必須
Apple Silicon unified memory でも、Metal の GPU blit 完了を別プロセスに可視化するには `waitUntilCompleted` が唯一の信頼できる手段。`waitUntilScheduled`、status チェック、タイミング仮定はすべて不十分。

### 2. 正確性を先に確保、最適化は後
- まず `waitUntilCompleted` で正しく動作させる
- 実測してボトルネックか判断 (0.5ms は 60fps の 3%)
- 本当に問題なら、安全性を**証明**してから最適化

### 3. POOL_SIZE = 5
- CEF トリプルバッファリング (3 surfaces) + クライアント読み取り猶予
- POOL_SIZE=3 ではクライアントが読み取り中の surface をサーバーが上書きする
- 5 なら 83ms (60fps で 5 フレーム分) の猶予があり安全

### 4. テクスチャキャッシュで高速化
- src 側: CEF が 2-3 個の IOSurface をローテーション → 4 エントリキャッシュ
- dst 側: プールインデックスごとにキャッシュ
- `newTextureWithDescriptor:iosurface:` の呼び出しを最小化 → 1.5ms → 0.5ms

### 5. プロファイリングコードは本番では削除
- `on_accelerated_paint` 内のファイル I/O (fprintf, NSLog) は定期スパイクの原因
- 計測が終わったらプロファイリングコードは完全に削除する
- 残す場合は頻度を極端に下げる (3000 フレーム以上)

### 6. Rust → Obj-C では @autoreleasepool 必須
- Rust スレッドには autorelease pool がない
- Metal オブジェクト (commandBuffer, blitCommandEncoder) は autorelease で返される
- `@autoreleasepool` がないと蓄積 → 定期バッチ解放 → フレームスパイク
