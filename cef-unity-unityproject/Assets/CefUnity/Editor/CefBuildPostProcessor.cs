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
            switch (report.summary.platform)
            {
                case BuildTarget.StandaloneOSX:
                    PostProcessOSX(report.summary.outputPath);
                    break;
                case BuildTarget.StandaloneWindows64:
                    PostProcessWindows(report.summary.outputPath);
                    break;
            }
        }

        // -------------------------------------------------------------------
        // macOS: cef-unity-server.app を <App>.app/Contents/Plugins/ にコピー
        // -------------------------------------------------------------------
        private static void PostProcessOSX(string appPath)
        {
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

            var src = Path.Combine(
                Application.dataPath,
                "CefUnity", "Interop", "Plugins", "osx-arm64", "cef-unity-server.app");

            if (!Directory.Exists(src))
            {
                Debug.LogError("[CefUnity] cef-unity-server.app not found at: " + src);
                return;
            }

            var dst = Path.Combine(pluginsDir, "cef-unity-server.app");

            if (Directory.Exists(dst))
                Directory.Delete(dst, true);

            Debug.Log("[CefUnity] Copying cef-unity-server.app to build...");
            CopyDirectory(src, dst);
            Debug.Log("[CefUnity] Done. Copied to: " + dst);
        }

        // -------------------------------------------------------------------
        // Windows: cef-unity-server.exe + libcef.dll + リソース一式を
        //          <App>_Data/Plugins/x86_64/ にコピー (cef_unity_rust.dll の隣)。
        //          Unity は cef_unity_rust.dll のみ自動コピーするので、それ以外を
        //          手動配置する。
        // -------------------------------------------------------------------
        private static void PostProcessWindows(string exePath)
        {
            if (!exePath.EndsWith(".exe"))
            {
                Debug.LogWarning("[CefUnity] Build output is not an .exe, skipping post-process.");
                return;
            }

            var dataDir = Path.Combine(
                Path.GetDirectoryName(exePath) ?? string.Empty,
                Path.GetFileNameWithoutExtension(exePath) + "_Data");

            var pluginsDir = Path.Combine(dataDir, "Plugins", "x86_64");
            if (!Directory.Exists(pluginsDir))
            {
                Debug.LogError("[CefUnity] Plugins/x86_64 directory not found: " + pluginsDir);
                return;
            }

            var src = Path.Combine(
                Application.dataPath,
                "CefUnity", "Interop", "Plugins", "win-x64");

            if (!Directory.Exists(src))
            {
                Debug.LogError("[CefUnity] win-x64 plugin folder not found at: " + src);
                return;
            }

            Debug.Log("[CefUnity] Copying CEF runtime to build...");
            CopyDirectoryFlat(src, pluginsDir, skipExtensions: new[] { ".meta" });
            Debug.Log("[CefUnity] Done. Copied to: " + pluginsDir);
        }

        // -------------------------------------------------------------------
        // ヘルパー
        // -------------------------------------------------------------------
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
                var dirName = Path.GetFileName(dir);
                if (dirName.StartsWith("."))
                    continue;

                CopyDirectory(dir, Path.Combine(dst, dirName));
            }
        }

        /// <summary>
        /// src の中身を dst に再帰的にコピーするが、cef_unity_rust.dll は除外する
        /// (Unity が自動でコピー済みのため)。
        /// </summary>
        private static void CopyDirectoryFlat(string src, string dst, string[] skipExtensions)
        {
            Directory.CreateDirectory(dst);

            foreach (var file in Directory.GetFiles(src))
            {
                var ext = Path.GetExtension(file);
                if (System.Array.IndexOf(skipExtensions, ext) >= 0)
                    continue;

                var fileName = Path.GetFileName(file);
                // cef_unity_rust.dll は Unity がプラグインとして自動コピーする
                if (fileName.Equals("cef_unity_rust.dll", System.StringComparison.OrdinalIgnoreCase))
                    continue;

                File.Copy(file, Path.Combine(dst, fileName), true);
            }

            foreach (var dir in Directory.GetDirectories(src))
            {
                var dirName = Path.GetFileName(dir);
                if (dirName.StartsWith("."))
                    continue;

                CopyDirectoryFlat(dir, Path.Combine(dst, dirName), skipExtensions);
            }
        }
    }
}
