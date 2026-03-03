// CefAppProtocol injection & exception-safe message pump for embedding CEF
// in a macOS host that owns its own NSApplication (e.g. Unity Editor).
//
// Problems solved here:
//
// 1. CEF requires NSApplication to conform to CefAppProtocol (CrAppProtocol).
//    We inject this via Objective-C runtime swizzling since we cannot subclass
//    Unity's NSApplication.
//
// 2. Chromium's ChromeWebAppShortcutCopierMain creates an NSWindow on a
//    background thread, which throws an ObjC exception from
//    _CFBundleGetValueForInfoKey. The exception propagates through C++ frames
//    → std::terminate → abort. We swizzle NSWindow init to catch the exception
//    at the ObjC level before it reaches C++ code.

#import <AppKit/AppKit.h>
#import <objc/runtime.h>

// ---------------------------------------------------------------------------
// CefAppProtocol hierarchy (avoid CEF header dependency)
// ---------------------------------------------------------------------------

@protocol CrAppProtocol
- (BOOL)isHandlingSendEvent;
@end

@protocol CrAppControlProtocol <CrAppProtocol>
- (void)setHandlingSendEvent:(BOOL)handlingSendEvent;
@end

@protocol CefAppProtocol <CrAppControlProtocol>
@end

// ---------------------------------------------------------------------------
// Inject CefAppProtocol into NSApplication
// ---------------------------------------------------------------------------

static char kHandlingSendEventKey;

@interface NSApplication (CefUnityAppProtocol) <CefAppProtocol>
@end

@implementation NSApplication (CefUnityAppProtocol)

- (BOOL)isHandlingSendEvent {
    NSNumber *value = objc_getAssociatedObject(self, &kHandlingSendEventKey);
    return value ? [value boolValue] : NO;
}

- (void)setHandlingSendEvent:(BOOL)handlingSendEvent {
    objc_setAssociatedObject(self, &kHandlingSendEventKey,
                             @(handlingSendEvent),
                             OBJC_ASSOCIATION_RETAIN_NONATOMIC);
}

@end

// ---------------------------------------------------------------------------
// sendEvent: swizzle – track isHandlingSendEvent for CefAppProtocol
// ---------------------------------------------------------------------------

static IMP g_original_sendEvent = NULL;

static void Swizzled_sendEvent(id self, SEL _cmd, NSEvent *event) {
    BOOL wasHandling = [self isHandlingSendEvent];
    [self setHandlingSendEvent:YES];
    ((void (*)(id, SEL, NSEvent *))g_original_sendEvent)(self, _cmd, event);
    [self setHandlingSendEvent:wasHandling];
}

// ---------------------------------------------------------------------------
// NSWindow init swizzle – catch exceptions at the ObjC level before they
// propagate through C++ frames (which would trigger std::terminate).
// ---------------------------------------------------------------------------

static IMP g_original_nswindow_init = NULL;

static id Swizzled_NSWindow_init(id self, SEL _cmd,
                                  NSRect contentRect,
                                  NSWindowStyleMask style,
                                  NSBackingStoreType backing,
                                  BOOL defer) {
    @try {
        return ((id (*)(id, SEL, NSRect, NSWindowStyleMask,
                        NSBackingStoreType, BOOL))g_original_nswindow_init)
            (self, _cmd, contentRect, style, backing, defer);
    } @catch (NSException *e) {
        if (![NSThread isMainThread]) {
            NSLog(@"[cef-unity] suppressed NSWindow init on bg thread: %@ – %@",
                  e.name, e.reason);
            return nil;
        }
        @throw;
    }
}

// ---------------------------------------------------------------------------
// Entry point – called once from Rust before CEF initialization
// ---------------------------------------------------------------------------

void cef_unity_inject_app_protocol(void) {
    Class cls = [NSApplication class];

    // sendEvent: swizzle for CefAppProtocol
    Method m = class_getInstanceMethod(cls, @selector(sendEvent:));
    if (m) {
        g_original_sendEvent = method_getImplementation(m);
        method_setImplementation(m, (IMP)Swizzled_sendEvent);
    }

    // Protocol injection
    Protocol *proto = objc_getProtocol("CefAppProtocol");
    if (!proto) {
        proto = objc_allocateProtocol("CefAppProtocol");
        if (proto) {
            Protocol *ctrl = objc_getProtocol("CrAppControlProtocol");
            if (!ctrl) {
                ctrl = objc_allocateProtocol("CrAppControlProtocol");
                if (ctrl) {
                    Protocol *cr = objc_getProtocol("CrAppProtocol");
                    if (!cr) {
                        cr = objc_allocateProtocol("CrAppProtocol");
                        if (cr) {
                            protocol_addMethodDescription(cr,
                                @selector(isHandlingSendEvent), "B@:", YES, YES);
                            objc_registerProtocol(cr);
                        }
                    }
                    if (cr) protocol_addProtocol(ctrl, cr);
                    protocol_addMethodDescription(ctrl,
                        @selector(setHandlingSendEvent:), "v@:B", YES, YES);
                    objc_registerProtocol(ctrl);
                }
            }
            if (ctrl) protocol_addProtocol(proto, ctrl);
            objc_registerProtocol(proto);
        }
    }
    if (proto) {
        class_addProtocol(cls, proto);
    }

    // NSWindow init swizzle – catch ObjC exceptions at the source
    Method winInit = class_getInstanceMethod([NSWindow class],
        @selector(initWithContentRect:styleMask:backing:defer:));
    if (winInit) {
        g_original_nswindow_init = method_getImplementation(winInit);
        method_setImplementation(winInit, (IMP)Swizzled_NSWindow_init);
    }
}

// ---------------------------------------------------------------------------
// Exception-safe wrapper for do_message_loop_work
// ---------------------------------------------------------------------------

void cef_unity_safe_pump(void (*pump_fn)(void)) {
    @try {
        pump_fn();
    } @catch (NSException *e) {
        NSLog(@"[cef-unity] caught ObjC exception in message pump: %@ – %@",
              e.name, e.reason);
    }
}

// ---------------------------------------------------------------------------
// NSApplication-based run loop for the CEF server process
// ---------------------------------------------------------------------------

typedef void (*cef_unity_timer_fn)(void *context);

static cef_unity_timer_fn g_timer_fn = NULL;
static void *g_timer_context = NULL;

@interface CefUnityTimerTarget : NSObject
- (void)timerFired:(NSTimer *)timer;
@end

@implementation CefUnityTimerTarget
- (void)timerFired:(NSTimer *)timer {
    if (g_timer_fn) {
        g_timer_fn(g_timer_context);
    }
}
@end

static CefUnityTimerTarget *g_timer_target = nil;

/// Initialize NSApplication and run its event loop with a periodic timer callback.
/// This provides proper macOS bootstrap port registration needed for Chromium's
/// Mach port rendezvous system (required by renderer subprocesses).
void cef_unity_nsapp_run(double interval_seconds,
                         cef_unity_timer_fn callback,
                         void *context) {
    g_timer_fn = callback;
    g_timer_context = context;

    [NSApplication sharedApplication];
    [NSApp setActivationPolicy:NSApplicationActivationPolicyProhibited];

    g_timer_target = [[CefUnityTimerTarget alloc] init];
    NSTimer *timer = [NSTimer timerWithTimeInterval:interval_seconds
                                             target:g_timer_target
                                           selector:@selector(timerFired:)
                                           userInfo:nil
                                            repeats:YES];
    [[NSRunLoop mainRunLoop] addTimer:timer forMode:NSDefaultRunLoopMode];

    [NSApp run];
}

/// Stop the NSApplication run loop.
void cef_unity_nsapp_stop(void) {
    dispatch_async(dispatch_get_main_queue(), ^{
        [NSApp stop:nil];
        // Post a dummy event to unblock [NSApp run] from its event wait
        NSEvent *event = [NSEvent otherEventWithType:NSEventTypeApplicationDefined
                                            location:NSMakePoint(0, 0)
                                       modifierFlags:0
                                           timestamp:0
                                        windowNumber:0
                                             context:nil
                                             subtype:0
                                               data1:0
                                               data2:0];
        [NSApp postEvent:event atStart:YES];
    });
}
