namespace CefUnity.Viewer
{
    /// <summary>
    ///     CPU 上の BGRA フレームを表示用テクスチャへ上げる継ぎ目。
    ///     GPU 共有テクスチャ経路を持たない Linux (software paint) で使う。
    ///     アップロードには表示側の GL コンテキストが要るため、実装は表示バックエンドが持ち、
    ///     受信の分岐を封じた CefFrameSource がこれを呼ぶ。
    /// </summary>
    internal interface IBgraFrameUploader
    {
        /// <summary>BGRA (上から下の行順) を上げ、描画に渡すテクスチャを返す。</summary>
        IntPtr UploadBgra(ReadOnlySpan<byte> bgra, int width, int height);
    }
}
