// この開発リポジトリ専用ツール (CEF_UNITY_DEV_TOOLS)。パッケージ利用側では無効。
#if CEF_UNITY_DEV_TOOLS
using UnityEditor;
using UnityEditor.Build.Reporting;
using UnityEngine;

namespace CefUnity.Editor
{
    /// <summary>
    ///     計測用 (一時): スタンドアロン macOS プレイヤーを非対話でビルドするメニュー項目。
    ///     フレームレートの真値を Editor 外 (ビルド) で計測するために使う。
    /// </summary>
    public static class CefQuickBuild
    {
        private const string OutPath =
            "/Users/juha/Documents/GitHub/cef-unity/build-mac/CefUnity.app";

        [MenuItem("CefUnity/Build Mac Player (measure)")]
        public static void BuildMac()
        {
            var opts = new BuildPlayerOptions
            {
                scenes = new[] { "Assets/Scenes/SampleScene.unity" },
                locationPathName = OutPath,
                target = BuildTarget.StandaloneOSX,
                // Development ビルド: temp ファイルの開発トグル群
                // (cef_perf_probe / cef_scroll_* / cef_novsync 等、DEVELOPMENT_BUILD で
                // 条件コンパイル) を計測ビルドで有効にする。リリースでは完全に消える。
                options = BuildOptions.Development,
            };

            var report = BuildPipeline.BuildPlayer(opts);
            var s = report.summary;
            Debug.Log($"[CefQuickBuild] result={s.result} sizeBytes={s.totalSize} " +
                      $"errors={s.totalErrors} out={OutPath}");
        }
    }
}
#endif
