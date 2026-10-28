// Test-process-only launch shim. Keep native benchmark windows from stealing
// keyboard focus; both compared applications must use the same shim.
// Build: clang -O2 -fobjc-arc -dynamiclib -framework AppKit FILE -o SHIM.dylib
// Then codesign --force --sign - SHIM.dylib and set
// TERMINAL_BENCH_BACKGROUND_LIBRARY to its absolute path for the Python probes.
#import <AppKit/AppKit.h>
#import <objc/runtime.h>

// Only benchmark processes load this library. Prevent real keyboard/mouse
// input from contaminating output-only PTY measurements if macOS nevertheless
// activates a test window. Do not use this shim for input-latency acceptance.
static void (*originalSendEvent)(id, SEL, NSEvent *);
static void sendBenchmarkEvent(id object, SEL selector, NSEvent *event) {
    switch (event.type) {
        case NSEventTypeKeyDown:
        case NSEventTypeKeyUp:
        case NSEventTypeFlagsChanged:
        case NSEventTypeLeftMouseDown:
        case NSEventTypeLeftMouseUp:
        case NSEventTypeRightMouseDown:
        case NSEventTypeRightMouseUp:
        case NSEventTypeOtherMouseDown:
        case NSEventTypeOtherMouseUp:
        case NSEventTypeLeftMouseDragged:
        case NSEventTypeRightMouseDragged:
        case NSEventTypeOtherMouseDragged:
        case NSEventTypeScrollWheel:
            return;
        default:
            originalSendEvent(object, selector, event);
    }
}

static void ignoreActivate(id object, SEL selector) {}
static void ignoreActivateFlag(id object, SEL selector, BOOL flag) {}
static void ignoreArrange(id object, SEL selector, id sender) {}
static void showInBackground(NSWindow *window, SEL selector, id sender) {
    [window orderBack:sender];
}
static void unhideInBackground(NSApplication *application, SEL selector, id sender) {
    [application unhideWithoutActivation];
}
static void replace(Class cls, SEL selector, IMP implementation) {
    Method method = class_getInstanceMethod(cls, selector);
    if (method) method_setImplementation(method, implementation);
}
__attribute__((constructor)) static void backgroundBenchmark(void) {
    Method send = class_getInstanceMethod([NSApplication class], @selector(sendEvent:));
    originalSendEvent = (void *)method_setImplementation(send, (IMP)sendBenchmarkEvent);
    replace([NSApplication class], @selector(activateIgnoringOtherApps:), (IMP)ignoreActivateFlag);
    replace([NSApplication class], NSSelectorFromString(@"activate"), (IMP)ignoreActivate);
    replace([NSApplication class], @selector(arrangeInFront:), (IMP)ignoreArrange);
    replace([NSApplication class], @selector(unhide:), (IMP)unhideInBackground);
    replace([NSWindow class], @selector(makeKeyAndOrderFront:), (IMP)showInBackground);
    replace([NSWindow class], @selector(makeKeyWindow), (IMP)ignoreActivate);
    replace([NSWindow class], @selector(makeMainWindow), (IMP)ignoreActivate);
}
