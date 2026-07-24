// この開発リポジトリ専用ツール (CEF_UNITY_DEV_TOOLS)。パッケージ利用側では無効。
#if CEF_UNITY_DEV_TOOLS
using CefUnity.Runtime;
using UnityEditor;
using UnityEngine;

namespace CefUnity.Editor
{
    /// <summary>
    ///   開発用: cef_scroll_record 録画を ScrollReplayRunner(Core)でオフライン検証する。
    ///   batchmode: Unity -batchmode -quit -executeMethod CefUnity.Editor.ScrollReplay.Run
    ///   入力 $TMPDIR/cef_scroll_events.csv → 出力 $TMPDIR/cef_scroll_replay.csv。
    /// </summary>
    public static class ScrollReplay
    {
        public static void Run()
        {
            var temporaryDirectory = System.IO.Path.GetTempPath();
            var source = System.IO.Path.Combine(temporaryDirectory, "cef_scroll_events.csv");
            var destination = System.IO.Path.Combine(temporaryDirectory, "cef_scroll_replay.csv");
            if (!System.IO.File.Exists(source))
            {
                Debug.LogError($"[ScrollReplay] input not found: {source} — cef_scroll_record トグルで録画してから実行すること");
                if (Application.isBatchMode) EditorApplication.Exit(1);
                return;
            }
            var result = ScrollReplayRunner.Run(System.IO.File.ReadLines(source));
            if (!result.Ok)
            {
                Debug.LogError($"[ScrollReplay] {result.Error}");
                if (Application.isBatchMode) EditorApplication.Exit(1);
                return;
            }
            System.IO.File.WriteAllText(destination, string.Join("\n", result.OutputLines) + "\n");
            Debug.Log($"[ScrollReplay] events={result.Events} ticks={result.Ticks} fidelity: mismatches={result.Mismatches}/{result.Ticks} out={destination}");
        }
    }
}
#endif
