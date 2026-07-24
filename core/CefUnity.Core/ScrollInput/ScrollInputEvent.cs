namespace CefUnity.Runtime
{
    /// <summary>スクロールジェスチャの局面 (macOS NSEventPhase 相当の抽象)。</summary>
    public enum ScrollPhase : byte
    {
        None = 0,
        GestureBegan = 1,
        GestureChanged = 2,
        GestureEnded = 3,
        MomentumBegan = 4,
        MomentumChanged = 5,
        MomentumEnded = 6,
        Cancelled = 7,
    }

    /// <summary>量子化前の生スクロールイベント 1 件。</summary>
    public struct ScrollInputEvent
    {
        /// <summary>ソース固有クロックの発生時刻 (秒)。IScrollEventSource.Now と同一基準。</summary>
        public double Timestamp;

        /// <summary>
        ///     単位規約: Precise=true なら CSS px 相当の生 delta、false なら「ノッチ (ステップ) 数」。
        ///     ステップ→px 変換 (×WheelPixelsPerStep) と resolutionScale 乗算は
        ///     ScrollInputPipeline が行うので、ソース実装は変換せずそのまま渡すこと
        ///     (Windows: WM_MOUSEWHEEL の delta/120 をここに入れる)。
        /// </summary>
        public float DxPx;

        /// <inheritdoc cref="DxPx" />
        public float DyPx;

        /// <summary>true = ピクセル精度 (トラックパッド)、false = ライン単位 (ホイールノッチ)。</summary>
        public bool Precise;

        /// <summary>
        ///     Phase 非対応プラットフォームは常に None でよい — ジェスチャ終端は
        ///     リサンプラの GraceTimeout が担保する (正規サポートモード)。
        /// </summary>
        public ScrollPhase Phase;
    }

    /// <summary>
    ///     プラットフォーム別の生スクロールイベント供給源。
    ///     Windows (WndProc サブクラス化) / Linux (XInput2) も本インターフェースで追加する。
    ///     設計: docs/superpowers/specs/2026-07-20-raw-scroll-resampling-design.md
    /// </summary>
    public interface IScrollEventSource : System.IDisposable
    {
        /// <summary>取得を開始する。false = 使用不可 (呼び出し側はフォールバック)。</summary>
        bool Start();

        /// <summary>新着イベントを buffer に書き込み、件数を返す。毎フレーム呼ぶこと。</summary>
        int Poll(ScrollInputEvent[] buffer);

        /// <summary>イベントと同一クロックの現在時刻 (秒)。</summary>
        double Now { get; }
    }
}
