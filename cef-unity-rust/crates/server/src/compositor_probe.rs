// ブラウザの合成経路 (compositor) の生存確認。
//
// GPU プロセスが落ちると Chromium は GPU プロセスを起動し直すが、既存ブラウザの
// OSR の合成経路は繋ぎ直されず、以後 paint が一切来なくなる (実測: BeginFrame を
// 送り続けても、invalidate・resize・再読み込みをしても 0。外部 BeginFrame を使わない
// 構成でも同じ。新しく作ったブラウザは正常に描画する)。CEF はこれを知らせる
// コールバックを持たないので、症状から検出する: しばらく paint が無いブラウザに
// invalidate を送り、それでも paint が返らなければ合成経路が死んでいるとみなす。
// 健全なら invalidate は必ず次の BeginFrame で paint を生む (リサイズ後の再描画と同じ)。
//
// 誤検出するとページが読み込み直しになるため、確かめられる状況でだけ判定する:
// 一度は描画できていたこと (初回読み込みが遅いだけのページを除く)、レンダラーが
// 生きていること (クラッシュ後で描くものが無い状態を除く)、BeginFrame が流れて
// いること (利用側が止まっている間は描画されなくて当然)。

use std::time::{Duration, Instant};

/// paint がこの時間無ければ invalidate で確かめる。
const IDLE_BEFORE_PROBE: Duration = Duration::from_secs(3);
/// invalidate の後、この時間内に paint が来なければ確認失敗。
const PROBE_TIMEOUT: Duration = Duration::from_secs(1);
/// 何回続けて失敗したら合成経路が死んだとみなすか。
const FAILED_PROBES_BEFORE_RECREATE: u32 = 2;

#[derive(Debug, PartialEq, Eq)]
pub enum ProbeAction {
    None,
    /// invalidate を送って paint が返るか確かめる。
    Invalidate,
    /// 合成経路が死んでいる。ブラウザを作り直す。
    Recreate,
}

pub struct CompositorProbe {
    observed_paint_count: u64,
    last_paint_at: Option<Instant>,
    probe_started_at: Option<Instant>,
    failed_probes: u32,
}

impl CompositorProbe {
    pub fn new() -> Self {
        CompositorProbe {
            observed_paint_count: 0,
            last_paint_at: None,
            probe_started_at: None,
            failed_probes: 0,
        }
    }

    /// 定期的に呼ぶ。`paint_count`: このブラウザの paint の累計。
    /// `renderer_alive`: レンダラーが生きているか。`begin_frames_flowing`: 直近に
    /// BeginFrame を受け取っているか。
    pub fn update(
        &mut self,
        now: Instant,
        paint_count: u64,
        renderer_alive: bool,
        begin_frames_flowing: bool,
    ) -> ProbeAction {
        if paint_count != self.observed_paint_count {
            self.observed_paint_count = paint_count;
            self.last_paint_at = Some(now);
            self.probe_started_at = None;
            self.failed_probes = 0;
            return ProbeAction::None;
        }
        let Some(last_paint_at) = self.last_paint_at else {
            return ProbeAction::None; // まだ一度も描画していない
        };
        if !renderer_alive || !begin_frames_flowing {
            // 確かめられない状況。進行中の確認は数えずに取り消す。
            self.probe_started_at = None;
            return ProbeAction::None;
        }
        match self.probe_started_at {
            Some(started_at) if now.saturating_duration_since(started_at) >= PROBE_TIMEOUT => {
                self.failed_probes += 1;
                if self.failed_probes >= FAILED_PROBES_BEFORE_RECREATE {
                    // 作り直したブラウザが一度描画するまで次の判定はしない。
                    *self = CompositorProbe::new();
                    self.observed_paint_count = paint_count;
                    ProbeAction::Recreate
                } else {
                    self.probe_started_at = Some(now);
                    ProbeAction::Invalidate
                }
            }
            Some(_) => ProbeAction::None,
            None if now.saturating_duration_since(last_paint_at) >= IDLE_BEFORE_PROBE => {
                self.probe_started_at = Some(now);
                ProbeAction::Invalidate
            }
            None => ProbeAction::None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(start: Instant, milliseconds: u64) -> Instant {
        start + Duration::from_millis(milliseconds)
    }

    #[test]
    fn healthy_idle_page_is_probed_and_answers() {
        let start = Instant::now();
        let mut probe = CompositorProbe::new();
        assert_eq!(probe.update(start, 1, true, true), ProbeAction::None);
        assert_eq!(probe.update(at(start, 2_000), 1, true, true), ProbeAction::None);
        assert_eq!(probe.update(at(start, 3_000), 1, true, true), ProbeAction::Invalidate);
        // invalidate で paint が返る = 健全。
        assert_eq!(probe.update(at(start, 3_050), 2, true, true), ProbeAction::None);
        assert_eq!(probe.update(at(start, 5_000), 2, true, true), ProbeAction::None);
        assert_eq!(probe.update(at(start, 6_050), 2, true, true), ProbeAction::Invalidate);
    }

    #[test]
    fn dead_compositor_is_recreated_after_two_failed_probes() {
        let start = Instant::now();
        let mut probe = CompositorProbe::new();
        probe.update(start, 1, true, true);
        assert_eq!(probe.update(at(start, 3_000), 1, true, true), ProbeAction::Invalidate);
        assert_eq!(probe.update(at(start, 3_500), 1, true, true), ProbeAction::None);
        assert_eq!(probe.update(at(start, 4_000), 1, true, true), ProbeAction::Invalidate, "1 回目の失敗で確かめ直す");
        assert_eq!(probe.update(at(start, 5_000), 1, true, true), ProbeAction::Recreate);
        // 作り直した後は、新しいブラウザが描画するまで判定しない。
        assert_eq!(probe.update(at(start, 20_000), 1, true, true), ProbeAction::None);
        assert_eq!(probe.update(at(start, 20_100), 2, true, true), ProbeAction::None);
        assert_eq!(probe.update(at(start, 23_100), 2, true, true), ProbeAction::Invalidate);
    }

    #[test]
    fn never_painted_browser_is_not_probed() {
        let start = Instant::now();
        let mut probe = CompositorProbe::new();
        assert_eq!(probe.update(start, 0, true, true), ProbeAction::None);
        assert_eq!(probe.update(at(start, 60_000), 0, true, true), ProbeAction::None);
    }

    #[test]
    fn not_probed_while_the_renderer_is_dead_or_begin_frames_stop() {
        let start = Instant::now();
        let mut probe = CompositorProbe::new();
        probe.update(start, 1, true, true);
        assert_eq!(probe.update(at(start, 10_000), 1, false, true), ProbeAction::None);
        assert_eq!(probe.update(at(start, 10_000), 1, true, false), ProbeAction::None);
    }

    /// 確認中に BeginFrame が止まったら、その確認は失敗として数えない。
    #[test]
    fn interrupted_probe_is_not_counted_as_a_failure() {
        let start = Instant::now();
        let mut probe = CompositorProbe::new();
        probe.update(start, 1, true, true);
        assert_eq!(probe.update(at(start, 3_000), 1, true, true), ProbeAction::Invalidate);
        assert_eq!(probe.update(at(start, 3_500), 1, true, false), ProbeAction::None);
        assert_eq!(probe.update(at(start, 4_000), 1, true, true), ProbeAction::Invalidate, "確認をやり直す");
        assert_eq!(probe.update(at(start, 5_000), 1, true, true), ProbeAction::Invalidate, "まだ 1 回目の失敗");
        assert_eq!(probe.update(at(start, 6_000), 1, true, true), ProbeAction::Recreate);
    }
}
