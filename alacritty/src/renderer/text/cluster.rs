//! CoreText shaping for multi-scalar emoji clusters on macOS.

use std::collections::HashMap;

use ahash::RandomState;
use core_foundation::attributed_string::CFMutableAttributedString;
use core_foundation::base::{CFRange, TCFType};
use core_foundation::boolean::CFBoolean;
use core_foundation::string::CFString;
use core_graphics::base::kCGImageAlphaPremultipliedLast;
use core_graphics::color_space::CGColorSpace;
use core_graphics::context::CGContext;
use core_graphics::geometry::{CGPoint, CGRect, CGSize};
use core_text::font::{CTFont, new_from_descriptor, new_from_name};
use core_text::font_collection::create_for_family;
use core_text::font_descriptor::{SymbolicTraitAccessors, kCTFontColorGlyphsTrait};
use core_text::line::CTLine;
use core_text::string_attributes::{
    kCTFontAttributeName, kCTForegroundColorFromContextAttributeName,
};
use crossfont::{BitmapBuffer, FontKey, RasterizedGlyph};
use objc2_foundation::{NSUserDefaults, ns_string};
use unicode_width::UnicodeWidthChar;

use super::glyph_cache::Glyph;
use crate::config::font::{Font, FontDescription};

pub fn needs_shaping(tail: &[char]) -> bool {
    tail.iter().any(|&c| {
        c.width() != Some(0) || matches!(c, '\u{200d}' | '\u{fe0f}' | '\u{fe0e}' | '\u{20e3}')
    })
}

pub struct ClusterCache {
    fonts: HashMap<FontKey, CTFont, RandomState>,
    glyphs: HashMap<FontKey, HashMap<String, Glyph, RandomState>, RandomState>,
    text: String,
}

impl ClusterCache {
    pub fn new(font: &Font, keys: [FontKey; 4]) -> Self {
        let descriptions = [font.normal().clone(), font.bold(), font.italic(), font.bold_italic()];
        let mut fonts = HashMap::default();
        for (index, (key, desc)) in keys.into_iter().zip(descriptions).enumerate() {
            let ct_font =
                load_font(&desc, font.size().as_pt() as f64, index & 1 != 0, index & 2 != 0);
            if let Some(font) = ct_font {
                fonts.entry(key).or_insert(font);
            }
        }
        Self { fonts, glyphs: HashMap::default(), text: String::new() }
    }

    pub fn clear(&mut self) {
        self.glyphs.clear();
    }

    pub fn prepare(&mut self, base: char, tail: &[char]) {
        self.text.clear();
        self.text.push(base);
        self.text.extend(tail);
    }

    pub fn get(&self, key: FontKey) -> Option<Glyph> {
        self.glyphs.get(&key)?.get(self.text.as_str()).copied()
    }

    pub fn insert(&mut self, key: FontKey, glyph: Glyph) {
        self.glyphs.entry(key).or_default().insert(self.text.clone(), glyph);
    }

    pub fn rasterize(&self, key: FontKey) -> Option<RasterizedGlyph> {
        rasterize(&self.text, self.fonts.get(&key)?)
    }
}

fn load_font(desc: &FontDescription, size: f64, bold: bool, italic: bool) -> Option<CTFont> {
    if let Some(collection) = create_for_family(&desc.family) {
        if let Some(descriptors) = collection.get_descriptors() {
            for descriptor in descriptors.iter() {
                let font = new_from_descriptor(&descriptor, size);
                let matches = match desc.style.as_ref() {
                    Some(style) => descriptor.style_name() == *style,
                    None => {
                        font.symbolic_traits().is_bold() == bold
                            && font.symbolic_traits().is_italic() == italic
                    },
                };
                if matches {
                    return Some(font);
                }
            }
        }
    }
    new_from_name("Menlo", size).ok()
}

fn bitmap(width: usize, height: usize) -> CGContext {
    CGContext::create_bitmap_context(
        None,
        width,
        height,
        8,
        width * 4,
        &CGColorSpace::create_device_rgb(),
        kCGImageAlphaPremultipliedLast,
    )
}

fn rasterize(text: &str, font: &CTFont) -> Option<RasterizedGlyph> {
    let mut attributed = CFMutableAttributedString::new();
    attributed.replace_str(&CFString::new(text), CFRange::init(0, 0));
    let range = CFRange::init(0, attributed.char_len());
    // SAFETY: These are the CoreText attribute keys and correctly typed CF values.
    unsafe {
        attributed.set_attribute(range, kCTFontAttributeName, font);
        attributed.set_attribute(
            range,
            kCTForegroundColorFromContextAttributeName,
            &CFBoolean::true_value(),
        );
    }
    let line = CTLine::new_with_attributed_string(attributed.as_concrete_TypeRef());
    let measurement = bitmap(1, 1);
    let bounds = line.get_image_bounds(&measurement);
    let left = bounds.origin.x.floor();
    let bottom = bounds.origin.y.floor();
    let top = (bounds.origin.y + bounds.size.height).ceil();
    let width = (bounds.origin.x + bounds.size.width).ceil() - left;
    let height = top - bottom;
    // Bound native allocation for malformed/unreasonably large clusters.
    if !width.is_finite()
        || !height.is_finite()
        || width <= 0.
        || height <= 0.
        || width > 4096.
        || height > 4096.
    {
        return None;
    }
    let colored = line.glyph_runs().iter().any(|run| {
        run.attributes()
            .and_then(|attrs| {
                attrs.find(CFString::new("NSFont")).and_then(|value| value.downcast::<CTFont>())
            })
            .is_some_and(|font| font.symbolic_traits() & kCTFontColorGlyphsTrait != 0)
    });
    let mut context = bitmap(width as usize, height as usize);
    context.set_rgb_fill_color(0., 0., 0., if colored { 0. } else { 1. });
    context.fill_rect(CGRect::new(&CGPoint::new(0., 0.), &CGSize::new(width, height)));
    // Match the scalar rasterizer's app-specific font smoothing preference.
    let defaults = NSUserDefaults::standardUserDefaults();
    let smooth = defaults.objectForKey(ns_string!("AppleFontSmoothing")).is_none()
        || defaults.integerForKey(ns_string!("AppleFontSmoothing")) != 0;
    context.set_allows_font_smoothing(true);
    context.set_should_smooth_fonts(smooth);
    context.set_allows_antialiasing(true);
    context.set_should_antialias(true);
    context.set_allows_font_subpixel_positioning(true);
    context.set_should_subpixel_position_fonts(true);
    context.set_rgb_fill_color(1., 1., 1., 1.);
    context.set_text_position(-left, -bottom);
    line.draw(&context);
    let bytes = context.data();
    let buffer = if colored {
        BitmapBuffer::Rgba(bytes.to_vec())
    } else {
        BitmapBuffer::Rgb(
            bytes.chunks_exact(4).flat_map(|pixel| pixel[..3].iter().copied()).collect(),
        )
    };
    Some(RasterizedGlyph {
        character: text.chars().next()?,
        left: left as i32,
        top: top as i32,
        width: width as i32,
        height: height as i32,
        advance: (0, 0),
        buffer,
    })
}
