// CefAppProtocol injection & exception-safe message pump for embedding CEF
// in a macOS host that owns its own NSApplication (e.g. Unity Editor).
//
// Two problems are solved here:
//
// 1. CEF requires NSApplication to conform to CefAppProtocol (CrAppProtocol).
//    We inject this via Objective-C runtime swizzling since we cannot subclass
//    Unity's NSApplication.
//
// 2. CEF registers a CFRunLoop observer that calls ChromeWebAppShortcutCopierMain,
//    which tries to create an NSWindow. This fails (ObjC exception from
//    _CFBundleGetValueForInfoKey) because the host bundle lacks Chrome-specific
//    Info.plist keys. We wrap do_message_loop_work() in @try/@catch so the
//    background CEF thread survives the exception.

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

static IMP g_original_sendEvent = NULL;

static void Swizzled_sendEvent(id self, SEL _cmd, NSEvent *event) {
    BOOL wasHandling = [self isHandlingSendEvent];
    [self setHandlingSendEvent:YES];
    ((void (*)(id, SEL, NSEvent *))g_original_sendEvent)(self, _cmd, event);
    [self setHandlingSendEvent:wasHandling];
}

void cef_unity_inject_app_protocol(void) {
    Class cls = [NSApplication class];

    Method m = class_getInstanceMethod(cls, @selector(sendEvent:));
    if (m) {
        g_original_sendEvent = method_getImplementation(m);
        method_setImplementation(m, (IMP)Swizzled_sendEvent);
    }

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
}

// ---------------------------------------------------------------------------
// Exception-safe wrapper for do_message_loop_work
// ---------------------------------------------------------------------------

// Wraps a function call in @try/@catch so that the
// ChromeWebAppShortcutCopierMain → NSWindow → _CFBundleGetValueForInfoKey
// ObjC exception does not kill the thread.
// The caller passes a function pointer because the CEF library is dynamically
// loaded and its symbols are not available at link time.
void cef_unity_safe_pump(void (*pump_fn)(void)) {
    @try {
        pump_fn();
    } @catch (NSException *e) {
        // Swallow – ChromeWebAppShortcutCopier is not needed for OSR.
        NSLog(@"[cef-unity] caught ObjC exception in message pump: %@ – %@",
              e.name, e.reason);
    }
}
