using System.IO;
using CefUnity.Interop;
using UnityEngine;

namespace CefUnity.Runtime
{
    /// <summary>
    ///     Unity で動かすときのログの保存先を決める。
    ///     <para>
    ///     CefUnity.Core はアプリを知らないため、保存先は利用側が渡す。Unity ではアプリごとに
    ///     分かれていて、プレイヤーにも書き込み権限がある <c>Application.persistentDataPath</c> の
    ///     下を既定にする (Windows: <c>%USERPROFILE%\AppData\LocalLow\&lt;会社名&gt;\&lt;製品名&gt;\CefUnity\Logs</c>、
    ///     macOS: <c>~/Library/Application Support/&lt;会社名&gt;/&lt;製品名&gt;/CefUnity/Logs</c>)。
    ///     <see cref="CefRuntime.Initialize" /> に <c>logDirectory</c> を渡せばそちらが優先される。
    ///     </para>
    /// </summary>
    public static class CefLogDirectory
    {
        public static string Default => Path.Combine(Application.persistentDataPath, "CefUnity", "Logs");

        // persistentDataPath はメインスレッドからしか読めないので、シーン読み込み前に確定させる。
        [RuntimeInitializeOnLoadMethod(RuntimeInitializeLoadType.SubsystemRegistration)]
        private static void ApplyDefault()
        {
            CefRuntime.DefaultLogDirectory ??= Default;
        }
    }
}
