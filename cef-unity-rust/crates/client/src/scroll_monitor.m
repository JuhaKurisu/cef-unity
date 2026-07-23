// NSEvent ローカルモニタでスクロールイベントを収集し、Unity 側の毎フレーム poll に渡す。
// スレッドモデル: モニタハンドラは AppKit メインスレッドで発火し、Unity スクリプト
// (poll 呼び出し側) も同じメインスレッドで動く。イベント配送はランループ上 (スクリプト
// 実行中には起きない) ため、リングはロック無しの単純配列で安全。
// 権限不要 (自アプリ宛イベントのみ)。イベントは素通し (return event) し通常配送を妨げない。
#import <AppKit/AppKit.h>
#import <string.h>

typedef struct {
    double timestamp;   // NSEvent.timestamp (起動からの秒)
    float dx, dy;       // scrollingDeltaX/Y (precise ならピクセル精度)
    uint8_t phase;      // 下の phase_of() 参照 (CefScrollEvent.phase と同一値)
    uint8_t precise;    // 1 = hasPreciseScrollingDeltas
} scroll_event_t;

#define RING_CAP 256
static scroll_event_t g_ring[RING_CAP];
static int g_count = 0;
static id g_monitor = nil;

static uint8_t phase_of(NSEvent *e) {
    NSEventPhase m = e.momentumPhase;
    if (m == NSEventPhaseBegan) return 4;
    if (m == NSEventPhaseChanged) return 5;
    if (m == NSEventPhaseEnded) return 6;
    if (m == NSEventPhaseCancelled) return 7;
    NSEventPhase p = e.phase;
    if (p == NSEventPhaseBegan) return 1;
    if (p == NSEventPhaseChanged) return 2;
    if (p == NSEventPhaseEnded) return 3;
    if (p == NSEventPhaseCancelled) return 7;
    return 0;
}

int cef_scroll_monitor_start_impl(void) {
    // 前回セッションの残骸を掃除する: dylib は Editor に常駐するため、異常終了で
    // stop 未到達だと古い timestamp のイベントが残り、次回開始直後の初回 poll で
    // GraceTimeout 超の蓄積分が一括排出されて「飛び」になる。
    g_count = 0;
    if (g_monitor != nil) return 1;
    if (NSApp == nil) return 0; // ヘッドレス (batchmode 等) → フォールバックさせる
    g_monitor = [NSEvent addLocalMonitorForEventsMatchingMask:NSEventMaskScrollWheel
                                                      handler:^NSEvent *(NSEvent *e) {
        if (g_count == RING_CAP) {
            // 飽和 (poll は毎フレームなので実質発生しない): 最古を捨てる
            memmove(g_ring, g_ring + 1, (RING_CAP - 1) * sizeof(scroll_event_t));
            g_count--;
        }
        scroll_event_t *s = &g_ring[g_count++];
        s->timestamp = e.timestamp;
        s->dx = (float)e.scrollingDeltaX;
        s->dy = (float)e.scrollingDeltaY;
        s->phase = phase_of(e);
        s->precise = e.hasPreciseScrollingDeltas ? 1 : 0;
        return e; // 素通し
    }];
    return g_monitor != nil ? 1 : 0;
}

void cef_scroll_monitor_stop_impl(void) {
    if (g_monitor != nil) {
        [NSEvent removeMonitor:g_monitor];
        g_monitor = nil;
    }
    g_count = 0;
}

int cef_scroll_monitor_poll_impl(scroll_event_t *out, int max) {
    int n = g_count < max ? g_count : max;
    memcpy(out, g_ring, (size_t)n * sizeof(scroll_event_t));
    if (n < g_count)
        memmove(g_ring, g_ring + n, (size_t)(g_count - n) * sizeof(scroll_event_t));
    g_count -= n;
    return n;
}

double cef_scroll_monitor_now_impl(void) {
    // NSEvent.timestamp と同一基準 (起動からの秒)
    return [[NSProcessInfo processInfo] systemUptime];
}
