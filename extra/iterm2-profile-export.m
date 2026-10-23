// Export only appearance settings from the user's default iTerm2 profile.
// Build: clang -fobjc-arc extra/iterm2-profile-export.m -framework AppKit -o /tmp/iterm2-profile-export
// Colors without a color-space tag are calibrated RGB, not sRGB.
#import <AppKit/AppKit.h>

static NSString *hexColor(NSDictionary *value) {
    CGFloat r = [value[@"Red Component"] doubleValue];
    CGFloat g = [value[@"Green Component"] doubleValue];
    CGFloat b = [value[@"Blue Component"] doubleValue];
    NSString *space = value[@"Color Space"];
    NSColor *color;
    if ([space isEqualToString:@"sRGB"]) {
        color = [NSColor colorWithSRGBRed:r green:g blue:b alpha:1];
    } else if ([space isEqualToString:@"P3"]) {
        color = [NSColor colorWithDisplayP3Red:r green:g blue:b alpha:1];
    } else {
        color = [NSColor colorWithCalibratedRed:r green:g blue:b alpha:1];
    }
    color = [color colorUsingColorSpace:NSColorSpace.sRGBColorSpace];
    return [NSString stringWithFormat:@"#%02x%02x%02x",
            (int)lround(MAX(0, MIN(1, color.redComponent)) * 255),
            (int)lround(MAX(0, MIN(1, color.greenComponent)) * 255),
            (int)lround(MAX(0, MIN(1, color.blueComponent)) * 255)];
}

int main(void) {
    @autoreleasepool {
        NSString *path = [NSHomeDirectory() stringByAppendingPathComponent:
                          @"Library/Preferences/com.googlecode.iterm2.plist"];
        NSDictionary *preferences = [NSDictionary dictionaryWithContentsOfFile:path];
        NSDictionary *profile;
        for (NSDictionary *candidate in preferences[@"New Bookmarks"]) {
            if ([candidate[@"Guid"] isEqual:preferences[@"Default Bookmark Guid"]]) {
                profile = candidate;
                break;
            }
        }
        if (!profile) { fprintf(stderr, "Default iTerm2 profile not found\n"); return 1; }
        if ([profile[@"Use Separate Colors for Light and Dark Mode"] boolValue]) {
            fprintf(stderr, "Separate light/dark profiles require an explicit appearance\n");
            return 1;
        }
        NSMutableDictionary *colors = [NSMutableDictionary dictionary];
        NSMutableArray *keys = [@[@"Background Color", @"Foreground Color", @"Bold Color",
                                 @"Cursor Color", @"Cursor Text Color", @"Selection Color",
                                 @"Selected Text Color", @"Match Background Color"] mutableCopy];
        for (int i = 0; i < 16; i++) [keys addObject:[NSString stringWithFormat:@"Ansi %d Color", i]];
        for (NSString *key in keys) {
            if (profile[key]) colors[key] = hexColor(profile[key]);
        }
        NSString *fontString = profile[@"Normal Font"];
        NSRange split = [fontString rangeOfString:@" " options:NSBackwardsSearch];
        if (split.location == NSNotFound) return 1;
        CGFloat size = [[fontString substringFromIndex:split.location + 1] doubleValue];
        NSFont *font = [NSFont fontWithName:[fontString substringToIndex:split.location] size:size];
        if (!font) { fprintf(stderr, "Configured iTerm2 font unavailable\n"); return 1; }
        NSMutableDictionary *settings = [NSMutableDictionary dictionary];
        for (NSString *key in @[@"Vertical Spacing", @"Horizontal Spacing", @"Thin Strokes",
                               @"Use Bright Bold", @"Use Bold Font", @"Use Italic Font",
                               @"ASCII Ligatures", @"Use Non-ASCII Font", @"Transparency",
                               @"Blur", @"Blinking Cursor", @"Cursor Type"]) {
            if (profile[key]) settings[key] = profile[key];
        }
        NSDictionary *result = @{
            @"profile": profile[@"Name"], @"colors": colors, @"settings": settings,
            @"font": @{@"family": font.familyName, @"name": font.fontName, @"size": @(size),
                       @"ascent": @(font.ascender), @"descent": @(font.descender),
                       @"leading": @(font.leading)},
            @"padding": @{@"x": preferences[@"TerminalMargin"] ?: @5,
                          @"y": preferences[@"TerminalVMargin"] ?: @2}
        };
        NSData *json = [NSJSONSerialization dataWithJSONObject:result
                                                     options:NSJSONWritingPrettyPrinted | NSJSONWritingSortedKeys
                                                       error:nil];
        fwrite(json.bytes, 1, json.length, stdout);
        puts("");
    }
    return 0;
}
