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
            public double Timestamp;
            public long Frames;
            public long AcceleratedFrameId;
            public long Applied;
        }

        private struct RatePoint
        {
            public double Timestamp;
            public float Unity;
            public float Cef;
            public float Applied;
        }

        private static readonly Color UnityColor = new Color(0.4f, 0.9f, 0.4f);
        private static readonly Color CefColor = new Color(0.35f, 0.75f, 1f);
        private static readonly Color AppliedColor = new Color(1f, 0.75f, 0.3f);

        private readonly List<CounterSample> _counters = new();
        private readonly List<RatePoint> _rates = new();
        private double _lastSampleTime;

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
            var time = EditorApplication.timeSinceStartup;
            if (time - _lastSampleTime < SampleInterval) return;
            _lastSampleTime = time;

            var browser = Application.isPlaying ? CefUnityBrowserSample.DiagnosticsInstance : null;
            if (browser == null)
            {
                Repaint(); // 直近のグラフは残したまま表示だけ更新
                return;
            }

            var sample = new CounterSample
            {
                Timestamp = time,
                Frames = Time.frameCount,
                AcceleratedFrameId = (long)browser.DiagnosticsAcceleratedFrameId,
                Applied = (long)browser.DiagnosticsTexturesApplied,
            };

            // Play し直し等でカウンタが巻き戻ったら履歴を作り直す
            if (_counters.Count > 0)
            {
                var last = _counters[_counters.Count - 1];
                if (sample.Frames < last.Frames || sample.AcceleratedFrameId < last.AcceleratedFrameId || sample.Applied < last.Applied)
                {
                    _counters.Clear();
                    _rates.Clear();
                }
            }

            _counters.Add(sample);
            while (_counters.Count > 0 && time - _counters[0].Timestamp > HistorySeconds + RateWindowSeconds)
                _counters.RemoveAt(0);

            // 1 秒移動窓のレート (窓の起点 = time-RateWindow 以前で最も新しいサンプル)
            var baseIndex = -1;
            for (var index = _counters.Count - 1; index >= 0; index--)
            {
                if (time - _counters[index].Timestamp >= RateWindowSeconds)
                {
                    baseIndex = index;
                    break;
                }
            }
            if (baseIndex >= 0)
            {
                var baseSample = _counters[baseIndex];
                var deltaTime = time - baseSample.Timestamp;
                if (deltaTime > 0.2)
                {
                    _rates.Add(new RatePoint
                    {
                        Timestamp = time,
                        Unity = (float)((sample.Frames - baseSample.Frames) / deltaTime),
                        Cef = (float)((sample.AcceleratedFrameId - baseSample.AcceleratedFrameId) / deltaTime),
                        Applied = (float)((sample.Applied - baseSample.Applied) / deltaTime),
                    });
                }
            }
            while (_rates.Count > 0 && time - _rates[0].Timestamp > HistorySeconds)
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
            foreach (var point in _rates)
                yMax = Mathf.Max(yMax, point.Unity, point.Cef, point.Applied);
            yMax *= 1.05f;

            // 30/60 fps のグリッド線
            foreach (var gridValue in new[] { 30f, 60f })
            {
                var y = rect.yMax - gridValue / yMax * rect.height;
                EditorGUI.DrawRect(new Rect(rect.x, y, rect.width, 1f), new Color(1f, 1f, 1f, 0.12f));
                GUI.Label(new Rect(rect.x + 2, y - 14, 40, 14), gridValue.ToString("F0"), EditorStyles.miniLabel);
            }

            if (_rates.Count < 2) return;

            var now = _rates[_rates.Count - 1].Timestamp;
            DrawSeries(rect, now, yMax, UnityColor, point => point.Unity);
            DrawSeries(rect, now, yMax, CefColor, point => point.Cef);
            DrawSeries(rect, now, yMax, AppliedColor, point => point.Applied);
        }

        private void DrawSeries(Rect rect, double now, float yMax, Color color, System.Func<RatePoint, float> select)
        {
            var points = new List<Vector3>(_rates.Count);
            foreach (var point in _rates)
            {
                var x = rect.xMax - (float)((now - point.Timestamp) / HistorySeconds) * rect.width;
                if (x < rect.x) continue;
                var y = rect.yMax - Mathf.Clamp01(select(point) / yMax) * rect.height;
                points.Add(new Vector3(x, y, 0));
            }
            if (points.Count < 2) return;
            Handles.color = color;
            Handles.DrawAAPolyLine(2f, points.ToArray());
        }
    }
}
#endif
