// この開発リポジトリ専用ツール (CEF_UNITY_DEV_TOOLS)。パッケージ利用側では無効。
#if CEF_UNITY_DEV_TOOLS
using System;
using System.Globalization;
using CefUnity.Runtime;
using UnityEditor;
using UnityEngine;

namespace CefUnity.Editor
{
    /// <summary>
    ///     開発用: cef_scroll_record で録画した生スクロールイベント列を、本物の
    ///     ScrollResampler にオフラインでリプレイする。実機のユーザー操作なしで
    ///     「低速時の飛び」等の入力→排出変換の不具合を再現・修正検証するための基盤。
    ///     batchmode: Unity -batchmode -quit -executeMethod CefUnity.Editor.ScrollReplay.Run
    ///     入力: $TMPDIR/cef_scroll_events.csv (S/E/T 行 — ScrollInputPipeline の録画形式。
    ///     S/over 列の無い旧録画も読める: scale=1・全イベント転送済みとみなす)
    ///     出力: $TMPDIR/cef_scroll_replay.csv — now,liveDx,liveDy,interpDx,interpDy,predDx,predDy
    ///     録画時と同モードの列を live (実排出) と突き合わせ、忠実度を自己報告する。
    /// </summary>
    public static class ScrollReplay
    {
        public static void Run()
        {
            var tmp = System.IO.Path.GetTempPath();
            var src = System.IO.Path.Combine(tmp, "cef_scroll_events.csv");
            var dst = System.IO.Path.Combine(tmp, "cef_scroll_replay.csv");
            if (!System.IO.File.Exists(src))
            {
                Debug.LogError($"[ScrollReplay] input not found: {src} — cef_scroll_record トグルで録画してから実行すること");
                Fail();
                return;
            }
            var interp = new ScrollResampler();
            var pred = new ScrollResampler { Predictive = true };
            var outLines = new System.Collections.Generic.List<string>();
            var scale = 1f; // S 行が無い旧録画は scale=1 扱い
            var events = 0;
            var ticks = 0;
            var mismatches = 0;
            var lineNo = 0;
            foreach (var line in System.IO.File.ReadLines(src))
            {
                lineNo++;
                if (line.Length == 0) continue;
                try
                {
                    var c = line.Split(',');
                    if (c.Length >= 2 && c[0] == "S")
                    {
                        scale = float.Parse(c[1], CultureInfo.InvariantCulture);
                    }
                    else if (c.Length >= 6 && c[0] == "E")
                    {
                        // over 列 (7列目) が 0 のイベントは live で転送されていない — 投入しない。
                        // 列が無い旧録画は「全て転送済み」とみなす。
                        if (c.Length >= 7 && c[6] != "1") continue;
                        var e = new ScrollInputEvent
                        {
                            Timestamp = double.Parse(c[1], CultureInfo.InvariantCulture),
                            // live のルーティングは scale 乗算後の値を入れる — 同じ変換を適用
                            DxPx = float.Parse(c[2], CultureInfo.InvariantCulture) * scale,
                            DyPx = float.Parse(c[3], CultureInfo.InvariantCulture) * scale,
                            Phase = (ScrollPhase)byte.Parse(c[4], CultureInfo.InvariantCulture),
                            Precise = c[5] == "1",
                        };
                        if (!e.Precise) continue; // 本番同様、リサンプラには precise のみ
                        interp.AddEvent(in e);
                        pred.AddEvent(in e);
                        events++;
                    }
                    else if (c.Length >= 5 && c[0] == "T")
                    {
                        var now = double.Parse(c[1], CultureInfo.InvariantCulture);
                        interp.Tick(now, out var idx, out var idy);
                        pred.Tick(now, out var pdx, out var pdy);
                        outLines.Add($"{c[1]},{c[2]},{c[3]},{idx},{idy},{pdx},{pdy}");
                        // 忠実度: 録画時と同モード (c[4]) の列が live 実排出 (c[2],c[3]) と一致するか
                        var wasPredictive = c[4] == "1";
                        var liveDx = int.Parse(c[2], CultureInfo.InvariantCulture);
                        var liveDy = int.Parse(c[3], CultureInfo.InvariantCulture);
                        if ((wasPredictive ? pdx : idx) != liveDx || (wasPredictive ? pdy : idy) != liveDy)
                            mismatches++;
                        ticks++;
                    }
                }
                catch (Exception e)
                {
                    Debug.LogError($"[ScrollReplay] parse error at line {lineNo}: \"{line}\" ({e.Message})");
                    Fail();
                    return;
                }
            }
            if (ticks == 0)
            {
                Debug.LogError($"[ScrollReplay] no T lines in {src} — 録画が空 (cef_scroll_record 中にスクロールしたか確認)");
                Fail();
                return;
            }
            System.IO.File.WriteAllText(dst, string.Join("\n", outLines) + "\n");
            Debug.Log($"[ScrollReplay] events={events} ticks={ticks} fidelity: mismatches={mismatches}/{ticks} out={dst}");
        }

        // batchmode では非 0 終了コードで失敗を伝える (偽成功防止)。
        private static void Fail()
        {
            if (Application.isBatchMode) EditorApplication.Exit(1);
        }
    }
}
#endif
