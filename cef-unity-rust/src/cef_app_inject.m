// CefAppProtocol injection for embedding CEF in a macOS process.
//
// CEF requires NSApplication to conform to CefAppProtocol (CrAppProtocol).
// We inject this via Objective-C runtime swizzling at startup.
//
// Additionally, NSWindow init is swizzled to catch ObjC exceptions thrown by
// Chromium's ChromeWebAppShortcutCopierMain on background threads, preventing
// them from propagating through C++ frames (which would trigger std::terminate).

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

