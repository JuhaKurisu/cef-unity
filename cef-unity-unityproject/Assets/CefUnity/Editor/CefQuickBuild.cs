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
        private const string OutputPath =
            "/Users/juha/Documents/GitHub/cef-unity/build-mac/CefUnity.app";

        [MenuItem("CefUnity/Build Mac Player (measure)")]
        public static void BuildMac()
        {
            var buildPlayerOptions = new BuildPlayerOptions
            {
                scenes = new[] { "Assets/Scenes/SampleScene.unity" },
                locationPathName = OutputPath,
                target = BuildTarget.StandaloneOSX,
                // Development ビルド: temp ファイルの開発トグル群
                // (cef_perf_probe / cef_scroll_* / cef_novsync 等、DEVELOPMENT_BUILD で
                // 条件コンパイル) を計測ビルドで有効にする。リリースでは完全に消える。
                options = BuildOptions.Development,
            };

            var report = BuildPipeline.BuildPlayer(buildPlayerOptions);
            var summary = report.summary;
            Debug.Log($"[CefQuickBuild] result={summary.result} sizeBytes={summary.totalSize} " +
                      $"errors={summary.totalErrors} out={OutputPath}");
        }

        /// <summary>
        ///     Windows プレイヤーの出力先。BuildMac と違い絶対パスを埋め込まず、
        ///     プロジェクト直下の Build/Windows/ を使う (worktree でもそのまま動くように)。
        /// </summary>
        private static string WindowsOutputPath =>
            System.IO.Path.Combine(
                System.IO.Path.GetDirectoryName(Application.dataPath) ?? string.Empty,
                "Build", "Windows", "CefUnity.exe");

        [MenuItem("CefUnity/Build Windows Player (measure)")]
        public static void BuildWindows()
        {
            var outputPath = WindowsOutputPath;
            var buildPlayerOptions = new BuildPlayerOptions
            {
                scenes = new[] { "Assets/Scenes/SampleScene.unity" },
                locationPathName = outputPath,
                target = BuildTarget.StandaloneWindows64,
                // BuildMac と同じ理由で Development ビルド (開発トグル群を有効にする)。
                options = BuildOptions.Development,
            };

            var report = BuildPipeline.BuildPlayer(buildPlayerOptions);
            var summary = report.summary;
            Debug.Log($"[CefQuickBuild] result={summary.result} sizeBytes={summary.totalSize} " +
                      $"errors={summary.totalErrors} out={outputPath}");
        }
    }
}
#endif
