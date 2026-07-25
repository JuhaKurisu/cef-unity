using System;
using System.Collections.Generic;

namespace CefUnity.Runtime
{
    /// <summary>
    ///     フレーム毎スクロール排出量の「粗さ」指標。
    ///     roughness = Σ|d[i]−d[i−1]| / Σ|d[i]|
    ///     対象は隣接ペアのどちらかが非 0 の遷移 (分子) と非 0 フレーム (分母)。
    ///     0 = 完全均一。ホスト間比較 (Unity vs Viewer) は必ず本関数で両者を計算する
    ///     (過去のアドホック集計値 0.147/0.088 とは定義互換を保証しない)。
    /// </summary>
    public static class ScrollRoughnessAnalyzer
    {
        public static double ComputeRoughness(IReadOnlyList<int> sentDeltaY)
        {
            double transitionSum = 0;
            double magnitudeSum = 0;
            for (var index = 0; index < sentDeltaY.Count; index++)
            {
                magnitudeSum += Math.Abs(sentDeltaY[index]);
                if (index == 0) continue;
                var previous = sentDeltaY[index - 1];
                var current = sentDeltaY[index];
                if (previous != 0 || current != 0)
                    transitionSum += Math.Abs(current - previous);
            }
            return magnitudeSum == 0 ? 0.0 : transitionSum / magnitudeSum;
        }
    }
}
