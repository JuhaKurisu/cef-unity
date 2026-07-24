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

            var pluginsDirectory = Path.Combine(appPath, "Contents", "Plugins");
            if (!Directory.Exists(pluginsDirectory))
            {
                Debug.LogError("[CefUnity] Plugins directory not found: " + pluginsDirectory);
                return;
            }

            var source = Path.Combine(
                Application.dataPath,
                "CefUnity", "Plugins", "osx-arm64", "cef-unity-server.app");

            if (!Directory.Exists(source))
            {
                Debug.LogError("[CefUnity] cef-unity-server.app not found at: " + source);
                return;
            }

            var destination = Path.Combine(pluginsDirectory, "cef-unity-server.app");

            if (Directory.Exists(destination))
                Directory.Delete(destination, true);

            Debug.Log("[CefUnity] Copying cef-unity-server.app to build...");
            CopyDirectory(source, destination);
            Debug.Log("[CefUnity] Done. Copied to: " + destination);
        }

        // -------------------------------------------------------------------
        // Windows: cef-unity-server.exe + libcef.dll + リソース一式を
        //          <App>_Data/Plugins/x86_64/ にコピー (cef_unity_rust.dll の隣)。
        //          Unity は cef_unity_rust.dll のみ自動コピーするので、それ以外を
        //          手動配置する。
        // -------------------------------------------------------------------
        private static void PostProcessWindows(string executablePath)
        {
            if (!executablePath.EndsWith(".exe"))
            {
                Debug.LogWarning("[CefUnity] Build output is not an .exe, skipping post-process.");
                return;
            }

            var dataDirectory = Path.Combine(
                Path.GetDirectoryName(executablePath) ?? string.Empty,
                Path.GetFileNameWithoutExtension(executablePath) + "_Data");

            var pluginsDirectory = Path.Combine(dataDirectory, "Plugins", "x86_64");
            if (!Directory.Exists(pluginsDirectory))
            {
                Debug.LogError("[CefUnity] Plugins/x86_64 directory not found: " + pluginsDirectory);
                return;
            }

            var source = Path.Combine(
                Application.dataPath,
                "CefUnity", "Plugins", "win-x64");

            if (!Directory.Exists(source))
            {
                Debug.LogError("[CefUnity] win-x64 plugin folder not found at: " + source);
                return;
            }

            Debug.Log("[CefUnity] Copying CEF runtime to build...");
            CopyDirectoryFlat(source, pluginsDirectory, skipExtensions: new[] { ".meta" });
            Debug.Log("[CefUnity] Done. Copied to: " + pluginsDirectory);
        }

        // -------------------------------------------------------------------
        // ヘルパー
        // -------------------------------------------------------------------
        private static void CopyDirectory(string source, string destination)
        {
            Directory.CreateDirectory(destination);

            foreach (var file in Directory.GetFiles(source))
            {
                // Unity が .app 内に作る .meta ファイルはスキップ
                if (file.EndsWith(".meta"))
                    continue;

                var destinationFile = Path.Combine(destination, Path.GetFileName(file));
                File.Copy(file, destinationFile, true);
            }

            foreach (var directory in Directory.GetDirectories(source))
            {
                var directoryName = Path.GetFileName(directory);
                if (directoryName.StartsWith("."))
                    continue;

                CopyDirectory(directory, Path.Combine(destination, directoryName));
            }
        }

        /// <summary>
        /// source の中身を destination に再帰的にコピーするが、cef_unity_rust.dll は除外する
        /// (Unity が自動でコピー済みのため)。
        /// </summary>
        private static void CopyDirectoryFlat(string source, string destination, string[] skipExtensions)
        {
            Directory.CreateDirectory(destination);

            foreach (var file in Directory.GetFiles(source))
            {
                var extension = Path.GetExtension(file);
                if (System.Array.IndexOf(skipExtensions, extension) >= 0)
                    continue;

                var fileName = Path.GetFileName(file);
                // cef_unity_rust.dll は Unity がプラグインとして自動コピーする
                if (fileName.Equals("cef_unity_rust.dll", System.StringComparison.OrdinalIgnoreCase))
                    continue;

                File.Copy(file, Path.Combine(destination, fileName), true);
            }

            foreach (var directory in Directory.GetDirectories(source))
            {
                var directoryName = Path.GetFileName(directory);
                if (directoryName.StartsWith("."))
                    continue;

                CopyDirectoryFlat(directory, Path.Combine(destination, directoryName), skipExtensions);
            }
        }
    }
}
