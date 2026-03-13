using System.IO;
using UnityEditor;
using UnityEditor.Build;
using UnityEditor.Build.Reporting;
using UnityEngine;

namespace CefUnity.Editor
{
    public class CefBuildPostProcessor : IPostprocessBuildWithReport
    {
        // dylib より後に実行（Unity のプラグインコピー完了後）
        public int callbackOrder => 100;

        public void OnPostprocessBuild(BuildReport report)
        {
            if (report.summary.platform != BuildTarget.StandaloneOSX)
                return;

            var appPath = report.summary.outputPath;

            // Unity のビルド出力は .app ディレクトリ
            if (!appPath.EndsWith(".app"))
            {
                Debug.LogWarning("[CefUnity] Build output is not a .app bundle, skipping post-process.");
                return;
            }

            var pluginsDir = Path.Combine(appPath, "Contents", "Plugins");
            if (!Directory.Exists(pluginsDir))
            {
                Debug.LogError("[CefUnity] Plugins directory not found: " + pluginsDir);
                return;
            }

            // ソース: Editor 内の cef-unity-server.app
            var src = Path.Combine(
                Application.dataPath,
                "CefUnity", "Interop", "Plugins", "osx-arm64", "cef-unity-server.app");

            if (!Directory.Exists(src))
            {
                Debug.LogError("[CefUnity] cef-unity-server.app not found at: " + src);
                return;
            }

            var dst = Path.Combine(pluginsDir, "cef-unity-server.app");

            // 既にあれば削除してからコピー
            if (Directory.Exists(dst))
                Directory.Delete(dst, true);

            Debug.Log("[CefUnity] Copying cef-unity-server.app to build...");
            CopyDirectory(src, dst);
            Debug.Log("[CefUnity] Done. Copied to: " + dst);
        }

        private static void CopyDirectory(string src, string dst)
        {
            Directory.CreateDirectory(dst);

            foreach (var file in Directory.GetFiles(src))
            {
                // Unity が .app 内に作る .meta ファイルはスキップ
                if (file.EndsWith(".meta"))
                    continue;

                var destFile = Path.Combine(dst, Path.GetFileName(file));
                File.Copy(file, destFile, true);
            }

            foreach (var dir in Directory.GetDirectories(src))
            {
                // .DS_Store フォルダ等をスキップ
                var dirName = Path.GetFileName(dir);
                if (dirName.StartsWith("."))
                    continue;

                CopyDirectory(dir, Path.Combine(dst, dirName));
            }
        }
    }
}
