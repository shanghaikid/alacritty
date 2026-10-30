// Loaded only into isolated benchmark children by compare-terminal-input.py.
// Synthetic keys go directly to the child's window; no global events are sent.
#import <AppKit/AppKit.h>
#import <QuartzCore/QuartzCore.h>
#import <IOSurface/IOSurface.h>
#import <objc/runtime.h>
#include <OpenGL/OpenGL.h>
#include <OpenGL/gl3.h>
#include <stdio.h>
#include <stdlib.h>
#include <time.h>

static int sent, seen;
static double sentAt;
static FILE *output;

static double seconds(void) {
    struct timespec t;
    clock_gettime(CLOCK_MONOTONIC, &t);
    return t.tv_sec + t.tv_nsec / 1e9;
}

static int phase(void) {
    FILE *f = fopen(getenv("PARITY_PHASE"), "r");
    if (!f) return 0;
    int c = fgetc(f);
    fclose(f);
    return c;
}

static void check(const unsigned char *pixels, int components, double cellWidth) {
    if (phase() != 'i' || sent == seen) return;
    int value = 0;
    for (int i = 0; i < 8; i++) {
        const unsigned char *p = pixels + ((int)((i + .5) * cellWidth)) * components;
        if (p[0] > 230 && p[1] > 230 && p[2] > 230) value |= 1 << i;
        else if (!(p[0] < 20 && p[1] < 20 && p[2] < 20)) return;
    }
    if (value != sent) return;
    seen = sent;
    if (!output) output = fopen(getenv("PARITY_SAMPLES"), "w");
    if (output) {
        fprintf(output, "%.6f\n", (seconds() - sentAt) * 1000);
        fflush(output);
    }
}

static CGLError flush(CGLContextObj context) {
    if (phase() == 'i' && sent > seen && CGLGetCurrentContext() == context) {
        GLint v[4], alignment, buffer;
        glGetIntegerv(GL_VIEWPORT, v);
        glGetIntegerv(GL_PACK_ALIGNMENT, &alignment);
        glGetIntegerv(GL_READ_BUFFER, &buffer);
        double cellWidth = v[2] / 140.0;
        int width = (int)(8 * cellWidth) + 1;
        unsigned char *pixels = malloc(width * 3);
        if (pixels) {
            glPixelStorei(GL_PACK_ALIGNMENT, 1);
            glReadBuffer(GL_BACK);
            glReadPixels(v[0], v[1] + v[3] - 4, width, 1, GL_RGB, GL_UNSIGNED_BYTE, pixels);
            glReadBuffer(buffer);
            glPixelStorei(GL_PACK_ALIGNMENT, alignment);
            check(pixels, 3, cellWidth);
            free(pixels);
        }
    }
    return CGLFlushDrawable(context);
}

__attribute__((used)) static struct { const void *replacement; const void *original; }
interpose __attribute__((section("__DATA,__interpose"))) = {
    (const void *)flush, (const void *)CGLFlushDrawable
};

static void (*originalContents)(id, SEL, id);
static void setContents(CALayer *layer, SEL selector, id contents) {
    if (contents && phase() == 'i' && sent > seen &&
        [NSStringFromClass(layer.class) isEqualToString:@"IOSurfaceLayer"]) {
        // Ghostty 1.4 submits a completed BGRA IOSurface to this layer.
        // Black/white marker decoding works for both BGRA and RGB readback.
        IOSurfaceRef surface = (__bridge IOSurfaceRef)contents;
        if (IOSurfaceLock(surface, kIOSurfaceLockReadOnly, NULL) == kIOReturnSuccess) {
            const unsigned char *pixels = IOSurfaceGetBaseAddress(surface);
            if (pixels && IOSurfaceGetHeight(surface) > 4) {
                check(pixels + 4 * IOSurfaceGetBytesPerRow(surface), 4,
                      IOSurfaceGetWidth(surface) / 140.0);
            }
            IOSurfaceUnlock(surface, kIOSurfaceLockReadOnly, NULL);
        }
    }
    originalContents(layer, selector, contents);
}

// Prevent test windows from taking keyboard focus. Events are still delivered
// directly to their window, including its normal input-method handling.
static void ignoreActivate(id o, SEL s) {}
static void ignoreActivateFlag(id o, SEL s, BOOL flag) {}
static void orderBack(NSWindow *w, SEL s, id sender) { [w orderBack:sender]; }
static void replace(Class c, SEL s, IMP implementation) {
    Method method = class_getInstanceMethod(c, s);
    if (method) method_setImplementation(method, implementation);
}

__attribute__((constructor)) static void install(void) {
    fprintf(stderr, "input probe installed\n");
    replace([NSApplication class], @selector(activateIgnoringOtherApps:), (IMP)ignoreActivateFlag);
    replace([NSApplication class], NSSelectorFromString(@"activate"), (IMP)ignoreActivate);
    replace([NSWindow class], @selector(makeKeyAndOrderFront:), (IMP)orderBack);
    replace([NSWindow class], @selector(makeKeyWindow), (IMP)ignoreActivate);
    replace([NSWindow class], @selector(makeMainWindow), (IMP)ignoreActivate);
    originalContents = (void *)method_setImplementation(
        class_getInstanceMethod([CALayer class], @selector(setContents:)), (IMP)setContents);
    dispatch_async(dispatch_get_main_queue(), ^{
        [NSTimer scheduledTimerWithTimeInterval:.05 repeats:YES block:^(NSTimer *timer) {
            if (phase() != 'i' || sent != seen) return;
            NSWindow *window = NSApp.keyWindow ?: NSApp.windows.firstObject;
            if (!window) return;
            NSString *text = sent == 100 ? @"q" : @"x";
            if (sent == 100) [timer invalidate];
            else { ++sent; sentAt = seconds(); }
            NSEvent *event = [NSEvent keyEventWithType:NSEventTypeKeyDown
                location:NSZeroPoint modifierFlags:0 timestamp:NSProcessInfo.processInfo.systemUptime
                windowNumber:window.windowNumber context:nil characters:text
                charactersIgnoringModifiers:text isARepeat:NO keyCode:7];
            [window sendEvent:event];
        }];
    });
}
