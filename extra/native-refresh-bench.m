// xcrun clang -O2 -fobjc-arc -Wno-deprecated-declarations \
//   extra/native-refresh-bench.m -framework AppKit -framework CoreVideo -o /tmp/refresh-bench
// Rejected experiment; not used by production frame scheduling. Requires macOS 12+.
// Run outside a restricted sandbox so AppKit can enumerate displays.
#import <AppKit/AppKit.h>
#import <CoreVideo/CoreVideo.h>
#include <mach/mach_time.h>
#include <stdio.h>

int main(void) {
    @autoreleasepool {
        [NSApplication sharedApplication];
        mach_timebase_info_data_t timebase;
        mach_timebase_info(&timebase);
        for (NSScreen *screen in NSScreen.screens) {
            NSWindow *window = [[NSWindow alloc] initWithContentRect:screen.frame
                styleMask:NSWindowStyleMaskBorderless backing:NSBackingStoreBuffered defer:NO screen:screen];
            window.releasedWhenClosed = NO;
            NSView *view = [[NSView alloc] initWithFrame:NSMakeRect(0, 0, 100, 100)];
            window.contentView = view;
            const unsigned iterations = 10000;
            for (unsigned native = 0; native <= 1; native++) {
                uint64_t start = mach_absolute_time();
                double hz = 0;
                for (unsigned i = 0; i < iterations; i++) {
                    NSScreen *current = view.window.screen ?: screen;
                    if (native) {
                        hz = current.maximumFramesPerSecond;
                    } else {
                        CGDirectDisplayID display = [current.deviceDescription[@"NSScreenNumber"] unsignedIntValue];
                        CFUUIDRef uuid = CGDisplayCreateUUIDFromDisplayID(display);
                        if (!uuid) return 2;
                        display = CGDisplayGetDisplayIDFromUUID(uuid);
                        CFRelease(uuid);
                        CGDisplayModeRef mode = CGDisplayCopyDisplayMode(display);
                        if (!mode) return 3;
                        hz = CGDisplayModeGetRefreshRate(mode);
                        CGDisplayModeRelease(mode);
                        if (hz <= 0) {
                            CVDisplayLinkRef link = NULL;
                            if (CVDisplayLinkCreateWithCGDisplay(display, &link) == kCVReturnSuccess) {
                                CVTime period = CVDisplayLinkGetNominalOutputVideoRefreshPeriod(link);
                                CVDisplayLinkRelease(link);
                                if (period.timeValue) hz = (double)period.timeScale / period.timeValue;
                            }
                        }
                    }
                }
                double us = (mach_absolute_time() - start) * (double)timebase.numer / timebase.denom / 1000. / iterations;
                printf("screen=%s method=%s hz=%.3f query=%.3fus\n", screen.localizedName.UTF8String,
                    native ? "AppKit" : "CoreGraphics", hz, us);
                fflush(stdout);
            }
            [window close];
        }
    }
}
