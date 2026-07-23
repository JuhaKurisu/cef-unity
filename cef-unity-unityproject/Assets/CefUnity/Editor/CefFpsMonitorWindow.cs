// この開発リポジトリ専用ツール (CEF_UNITY_DEV_TOOLS)。パッケージ利用側では無効。
#if CEF_UNITY_DEV_TOOLS
using System.Collections.Generic;
using CefUnity.Runtime;
using UnityEditor;
using UnityEngine;

namespace CefUnity.Editor
{
    /// <summary>
    ///     開発用リアルタイム fps モニタ。3 系列を 1 秒移動窓のレートでグラフ表示する:
    ///       Unity  = Unity のフレームレート (Time.frameCount 増分)
    ///       CEF    = CEF 内部の paint レート (accel_frame_id 増分 — server が実際に描いた数)
    ///       適用   = Unity テクスチャへ実際に適用された paint レート (画面に映った数)
    ///     メニュー: CefUnity/FPS Monitor。Play 中のみ計測。
    ///
    ///     読み方: CEF は damage 駆動で paint するため、静止ページで CEF/適用 が ~0 に
    ///     落ちるのは正常 (更新が無いフレームは描かない)。スクロール中や動画再生中に
    ///     CEF が Unity より大きく低ければ、それが「内部レートの不足」。
    ///     CEF ≈ 60 なのに 適用 が低ければ、受け渡し (recv/0F 待ち) 側の取りこぼし。
    /// </summary>
    public class CefFpsMonitorWindow : EditorWindow
    {
        private const double HistorySeconds = 60.0; // グラフの横幅 (秒)
        private const double RateWindowSeconds = 1.0; // レート算出の移動窓
        private const double SampleInterval = 0.1; // サンプリング周期 (秒)

        private struct CounterSample
        {
            public double T;
            public long Frames;
            public long Afi;
            public long Applied;
        }

        private struct RatePoint
        {
            public double T;
            public float Unity;
            public float Cef;
            public float Applied;
        }

        private static readonly Color UnityColor = new Color(0.4f, 0.9f, 0.4f);
        private static readonly Color CefColor = new Color(0.35f, 0.75f, 1f);
        private static readonly Color AppliedColor = new Color(1f, 0.75f, 0.3f);

        private readonly List<CounterSample> _counters = new();
        private readonly List<RatePoint> _rates = new();
        private double _lastSampleT;

        [MenuItem("CefUnity/FPS Monitor")]
        public static void Open()
        {
            GetWindow<CefFpsMonitorWindow>("CEF FPS");
        }

        // 自動化用: $TMPDIR/cef_fps_monitor が存在すればドメインリロード時に自動で開く
        // (メニュー実行はリモート操作からブロックされるため、既存の temp トグル方式に合わせる)。
        [InitializeOnLoadMethod]
        private static void AutoOpenViaToggle()
        {
            if (System.IO.File.Exists(
                    System.IO.Path.Combine(System.IO.Path.GetTempPath(), "cef_fps_monitor")))
                EditorApplication.delayCall += Open;
        }

        private void OnEnable()
        {
            EditorApplication.update += Tick;
        }

        private void OnDisable()
        {
            EditorApplication.update -= Tick;
        }

        private void Tick()
        {
            var t = EditorApplication.timeSinceStartup;
            if (t - _lastSampleT < SampleInterval) return;
            _lastSampleT = t;

            var browser = Application.isPlaying ? CefUnityBrowserSample.DiagnosticsInstance : null;
            if (browser == null)
            {
                Repaint(); // 直近のグラフは残したまま表示だけ更新
                return;
            }

            var sample = new CounterSample
            {
                T = t,
                Frames = Time.frameCount,
                Afi = (long)browser.DiagAccelFrameId,
                Applied = (long)browser.DiagTexturesApplied,
            };

            // Play し直し等でカウンタが巻き戻ったら履歴を作り直す
            if (_counters.Count > 0)
            {
                var last = _counters[_counters.Count - 1];
                if (sample.Frames < last.Frames || sample.Afi < last.Afi || sample.Applied < last.Applied)
                {
                    _counters.Clear();
                    _rates.Clear();
                }
            }

            _counters.Add(sample);
            while (_counters.Count > 0 && t - _counters[0].T > HistorySeconds + RateWindowSeconds)
                _counters.RemoveAt(0);

            // 1 秒移動窓のレート (窓の起点 = t-RateWindow 以前で最も新しいサンプル)
            var baseIdx = -1;
            for (var i = _counters.Count - 1; i >= 0; i--)
            {
                if (t - _counters[i].T >= RateWindowSeconds)
                {
                    baseIdx = i;
                    break;
                }
            }
            if (baseIdx >= 0)
            {
                var b = _counters[baseIdx];
                var dt = t - b.T;
                if (dt > 0.2)
                {
                    _rates.Add(new RatePoint
                    {
                        T = t,
                        Unity = (float)((sample.Frames - b.Frames) / dt),
                        Cef = (float)((sample.Afi - b.Afi) / dt),
                        Applied = (float)((sample.Applied - b.Applied) / dt),
                    });
                }
            }
            while (_rates.Count > 0 && t - _rates[0].T > HistorySeconds)
                _rates.RemoveAt(0);

            Repaint();
        }

        private void OnGUI()
        {
            var latest = _rates.Count > 0 ? _rates[_rates.Count - 1] : default;

            using (new EditorGUILayout.HorizontalScope())
            {
                DrawLegend("Unity", latest.Unity, UnityColor);
                DrawLegend("CEF paint", latest.Cef, CefColor);
                DrawLegend("適用", latest.Applied, AppliedColor);
            }

            if (!Application.isPlaying)
                EditorGUILayout.HelpBox("Play 中に計測します (静止ページで CEF/適用 ≈ 0 は正常 — damage 駆動)", MessageType.Info);

            var rect = GUILayoutUtility.GetRect(120f, 4000f, 100f, 4000f,
                GUILayout.ExpandWidth(true), GUILayout.ExpandHeight(true));
            DrawGraph(rect);
        }

        private static void DrawLegend(string label, float value, Color color)
        {
            var style = new GUIStyle(EditorStyles.boldLabel) { normal = { textColor = color } };
            GUILayout.Label($"{label}: {value,5:F1} fps", style, GUILayout.Width(150f));
        }

        private void DrawGraph(Rect rect)
        {
            EditorGUI.DrawRect(rect, new Color(0.12f, 0.12f, 0.12f));

            // Y スケール: 最低 70fps ぶんを確保し、観測最大に追従
            var yMax = 70f;
            foreach (var p in _rates)
                yMax = Mathf.Max(yMax, p.Unity, p.Cef, p.Applied);
            yMax *= 1.05f;

            // 30/60 fps のグリッド線
            foreach (var gy in new[] { 30f, 60f })
            {
                var y = rect.yMax - gy / yMax * rect.height;
                EditorGUI.DrawRect(new Rect(rect.x, y, rect.width, 1f), new Color(1f, 1f, 1f, 0.12f));
                GUI.Label(new Rect(rect.x + 2, y - 14, 40, 14), gy.ToString("F0"), EditorStyles.miniLabel);
            }

            if (_rates.Count < 2) return;

            var now = _rates[_rates.Count - 1].T;
            DrawSeries(rect, now, yMax, UnityColor, p => p.Unity);
            DrawSeries(rect, now, yMax, CefColor, p => p.Cef);
            DrawSeries(rect, now, yMax, AppliedColor, p => p.Applied);
        }

        private void DrawSeries(Rect rect, double now, float yMax, Color color, System.Func<RatePoint, float> select)
        {
            var points = new List<Vector3>(_rates.Count);
            foreach (var p in _rates)
            {
                var x = rect.xMax - (float)((now - p.T) / HistorySeconds) * rect.width;
                if (x < rect.x) continue;
                var y = rect.yMax - Mathf.Clamp01(select(p) / yMax) * rect.height;
                points.Add(new Vector3(x, y, 0));
            }
            if (points.Count < 2) return;
            Handles.color = color;
            Handles.DrawAAPolyLine(2f, points.ToArray());
        }
    }
}
#endif
