using UnityEngine;

namespace CefUnity.Runtime
{
    /// <summary>
    ///     CEF-Unity 全体のログ出力を一括制御するマスタースイッチ。
    ///     <para>
    ///     <see cref="Enabled" /> が <c>false</c> の間は <see cref="Log" /> /
    ///     <see cref="LogWarning" /> が抑制される。これがプロジェクト全体の診断ログの
    ///     単一の真実の源であり、Unity 側 (CefUnityBrowserSample / CefAudioOutput) と
    ///     Rust 側 (client/server のファイルログ) の両方が同じフラグに従う
    ///     (Rust 側へは <see cref="CefRuntime.Init" /> の <c>enableLog</c> 引数で伝搬する)。
    ///     </para>
    ///     <para>
    ///     <see cref="LogError" /> は障害の握りつぶしを防ぐため既定で常に出力する。
    ///     エラーも含めて完全に無音化したい場合は <see cref="SuppressErrors" /> を立てる。
    ///     </para>
    /// </summary>
    public static class CefLog
    {
        /// <summary>情報・診断ログ (Log / LogWarning) を出力するか。</summary>
        public static bool Enabled { get; set; }

        /// <summary>true にするとエラーログも抑制する (既定 false = エラーは常に出す)。</summary>
        public static bool SuppressErrors { get; set; }

        public static void Log(string message)
        {
            if (Enabled) Debug.Log(message);
        }

        public static void LogWarning(string message)
        {
            if (Enabled) Debug.LogWarning(message);
        }

        public static void LogError(string message)
        {
            if (!SuppressErrors) Debug.LogError(message);
        }
    }
}
