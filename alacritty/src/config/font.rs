use std::fmt;

use crossfont::Size as FontSize;
use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use alacritty_config_derive::{ConfigDeserialize, SerdeReplace};

use crate::config::ui_config::Delta;

/// Font config.
///
/// Defaults are provided at the level of this struct per platform, but not per
/// field in this struct. It might be nice in the future to have defaults for
/// each value independently. Alternatively, maybe erroring when the user
/// doesn't provide complete config is Ok.
#[derive(ConfigDeserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct Font {
    /// Extra spacing per character.
    pub offset: Delta<i8>,

    /// Glyph offset within character cell.
    pub glyph_offset: Delta<i8>,

    #[config(removed = "set the AppleFontSmoothing user default instead")]
    pub use_thin_strokes: bool,

    /// Normal font face.
    normal: FontDescription,

    /// Bold font face.
    bold: SecondaryFontDescription,

    /// Italic font face.
    italic: SecondaryFontDescription,

    /// Bold italic font face.
    bold_italic: SecondaryFontDescription,

    /// Font size in points.
    size: Size,

    /// Explicit line height in points, scaled together with the font size.
    line_height: Option<Size>,

    /// Whether to use the built-in font for box drawing characters.
    pub builtin_box_drawing: bool,
}

impl Font {
    /// Get a font clone with a size modification.
    pub fn with_size(self, size: FontSize) -> Font {
        let scale = size.as_pt() / self.size.0.as_pt();
        let line_height = self.line_height.map(|height| Size(height.0.scale(scale)));
        Font { size: Size(size), line_height, ..self }
    }

    #[inline]
    pub fn size(&self) -> FontSize {
        self.size.0
    }

    /// Explicit line height in rasterizer units, if configured.
    pub fn line_height(&self) -> Option<f64> {
        self.line_height.as_ref().map(|height| {
            // CoreText uses macOS points directly; other rasterizers use 96 DPI.
            #[cfg(target_os = "macos")]
            let height = height.0.as_pt();
            #[cfg(not(target_os = "macos"))]
            let height = height.0.as_px();
            f64::from(height)
        })
    }

    /// Get normal font description.
    pub fn normal(&self) -> &FontDescription {
        &self.normal
    }

    /// Get bold font description.
    pub fn bold(&self) -> FontDescription {
        self.bold.desc(&self.normal)
    }

    /// Get italic font description.
    pub fn italic(&self) -> FontDescription {
        self.italic.desc(&self.normal)
    }

    /// Get bold italic font description.
    pub fn bold_italic(&self) -> FontDescription {
        self.bold_italic.desc(&self.normal)
    }
}

impl Default for Font {
    fn default() -> Font {
        Self {
            builtin_box_drawing: true,
            glyph_offset: Default::default(),
            use_thin_strokes: Default::default(),
            bold_italic: Default::default(),
            italic: Default::default(),
            offset: Default::default(),
            normal: Default::default(),
            bold: Default::default(),
            size: Default::default(),
            line_height: None,
        }
    }
}

/// Description of the normal font.
#[derive(ConfigDeserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct FontDescription {
    pub family: String,
    pub style: Option<String>,
}

impl Default for FontDescription {
    fn default() -> FontDescription {
        FontDescription {
            #[cfg(not(any(target_os = "macos", windows)))]
            family: "monospace".into(),
            #[cfg(target_os = "macos")]
            family: "Menlo".into(),
            #[cfg(windows)]
            family: "Consolas".into(),
            style: None,
        }
    }
}

/// Description of the italic and bold font.
#[derive(ConfigDeserialize, Serialize, Debug, Default, Clone, PartialEq, Eq)]
pub struct SecondaryFontDescription {
    family: Option<String>,
    style: Option<String>,
}

impl SecondaryFontDescription {
    pub fn desc(&self, fallback: &FontDescription) -> FontDescription {
        FontDescription {
            family: self.family.clone().unwrap_or_else(|| fallback.family.clone()),
            style: self.style.clone(),
        }
    }
}

#[derive(SerdeReplace, Debug, Clone, PartialEq, Eq)]
struct Size(FontSize);

impl Default for Size {
    fn default() -> Self {
        Self(FontSize::new(11.25))
    }
}

impl<'de> Deserialize<'de> for Size {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct NumVisitor;
        impl Visitor<'_> for NumVisitor {
            type Value = Size;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("f64 or i64")
            }

            fn visit_f64<E: de::Error>(self, value: f64) -> Result<Self::Value, E> {
                Ok(Size(FontSize::new(value as f32)))
            }

            fn visit_i64<E: de::Error>(self, value: i64) -> Result<Self::Value, E> {
                Ok(Size(FontSize::new(value as f32)))
            }
        }

        deserializer.deserialize_any(NumVisitor)
    }
}

impl Serialize for Size {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_f32(self.0.as_pt())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_line_height_tracks_dpi_and_zoom() {
        let font: Font = toml::from_str("size = 12.0\nline_height = 14.0").unwrap();
        let height = font.line_height().unwrap();
        let retina = font.clone().with_size(FontSize::new(24.));
        assert_eq!(retina.line_height(), Some(height * 2.));
        let zoomed = font.with_size(FontSize::new(18.));
        assert_eq!(zoomed.line_height(), Some(height * 1.5));
        let restored = retina.with_size(FontSize::new(12.));
        assert_eq!(restored.line_height(), Some(height));
    }

    #[test]
    fn natural_line_height_remains_the_default() {
        let font = Font::default().with_size(FontSize::new(24.));
        assert_eq!(font.line_height(), None);
    }
}
