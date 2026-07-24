// NSEvent ローカルモニタでスクロールイベントを収集し、Unity 側の毎フレーム poll に渡す。
// スレッドモデル: モニタハンドラは AppKit メインスレッドで発火し、Unity スクリプト
// (poll 呼び出し側) も同じメインスレッドで動く。イベント配送はランループ上 (スクリプト
// 実行中には起きない) ため、リングはロック無しの単純配列で安全。
// 権限不要 (自アプリ宛イベントのみ)。イベントは素通し (return event) し通常配送を妨げない。
#import <AppKit/AppKit.h>
#import <string.h>

typedef struct {
    double timestamp;   // NSEvent.timestamp (起動からの秒)
    float delta_x, delta_y;       // scrollingDeltaX/Y (precise ならピクセル精度)
    uint8_t phase;      // 下の phase_of() 参照 (CefScrollEvent.phase と同一値)
    uint8_t precise;    // 1 = hasPreciseScrollingDeltas
} scroll_event_t;

#define RING_CAPACITY 256
static scroll_event_t g_ring[RING_CAPACITY];
static int g_count = 0;
static id g_monitor = nil;

static uint8_t phase_of(NSEvent *event) {
    NSEventPhase momentum_phase = event.momentumPhase;
    if (momentum_phase == NSEventPhaseBegan) return 4;
    if (momentum_phase == NSEventPhaseChanged) return 5;
    if (momentum_phase == NSEventPhaseEnded) return 6;
    if (momentum_phase == NSEventPhaseCancelled) return 7;
    NSEventPhase phase = event.phase;
    if (phase == NSEventPhaseBegan) return 1;
    if (phase == NSEventPhaseChanged) return 2;
    if (phase == NSEventPhaseEnded) return 3;
    if (phase == NSEventPhaseCancelled) return 7;
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
                                                      handler:^NSEvent *(NSEvent *event) {
        if (g_count == RING_CAPACITY) {
            // 飽和 (poll は毎フレームなので実質発生しない): 最古を捨てる
            memmove(g_ring, g_ring + 1, (RING_CAPACITY - 1) * sizeof(scroll_event_t));
            g_count--;
        }
        scroll_event_t *entry = &g_ring[g_count++];
        entry->timestamp = event.timestamp;
        entry->delta_x = (float)event.scrollingDeltaX;
        entry->delta_y = (float)event.scrollingDeltaY;
        entry->phase = phase_of(event);
        entry->precise = event.hasPreciseScrollingDeltas ? 1 : 0;
        return event; // 素通し
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
    int copy_count = g_count < max ? g_count : max;
    memcpy(out, g_ring, (size_t)copy_count * sizeof(scroll_event_t));
    if (copy_count < g_count)
        memmove(g_ring, g_ring + copy_count, (size_t)(g_count - copy_count) * sizeof(scroll_event_t));
    g_count -= copy_count;
    return copy_count;
}

double cef_scroll_monitor_now_impl(void) {
    // NSEvent.timestamp と同一基準 (起動からの秒)
    return [[NSProcessInfo processInfo] systemUptime];
}
