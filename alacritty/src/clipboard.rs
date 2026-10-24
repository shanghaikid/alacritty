use log::{debug, warn};
use winit::raw_window_handle::RawDisplayHandle;

use alacritty_terminal::term::ClipboardType;

#[cfg(any(feature = "x11", target_os = "macos", windows))]
use copypasta::ClipboardContext;
use copypasta::ClipboardProvider;
use copypasta::nop_clipboard::NopClipboardContext;
#[cfg(all(feature = "wayland", not(any(target_os = "macos", windows))))]
use copypasta::wayland_clipboard;
#[cfg(all(feature = "x11", not(any(target_os = "macos", windows))))]
use copypasta::x11_clipboard::{Primary as X11SelectionClipboard, X11ClipboardContext};

pub struct Clipboard {
    clipboard: Box<dyn ClipboardProvider>,
    selection: Option<Box<dyn ClipboardProvider>>,
}

impl Clipboard {
    pub unsafe fn new(display: RawDisplayHandle) -> Self {
        match display {
            #[cfg(all(feature = "wayland", not(any(target_os = "macos", windows))))]
            RawDisplayHandle::Wayland(display) => {
                let (selection, clipboard) = unsafe {
                    wayland_clipboard::create_clipboards_from_external(display.display.as_ptr())
                };
                Self { clipboard: Box::new(clipboard), selection: Some(Box::new(selection)) }
            },
            _ => Self::default(),
        }
    }

    /// Used for tests, to handle missing clipboard provider when built without the `x11`
    /// feature, and as default clipboard value.
    pub fn new_nop() -> Self {
        Self { clipboard: Box::new(NopClipboardContext::new().unwrap()), selection: None }
    }
}

impl Default for Clipboard {
    fn default() -> Self {
        #[cfg(any(target_os = "macos", windows))]
        return Self { clipboard: Box::new(ClipboardContext::new().unwrap()), selection: None };

        #[cfg(all(feature = "x11", not(any(target_os = "macos", windows))))]
        return Self {
            clipboard: Box::new(ClipboardContext::new().unwrap()),
            selection: Some(Box::new(X11ClipboardContext::<X11SelectionClipboard>::new().unwrap())),
        };

        #[cfg(not(any(feature = "x11", target_os = "macos", windows)))]
        return Self::new_nop();
    }
}

impl Clipboard {
    /// Load an explicit paste, allowing image files only outside terminal search.
    ///
    /// Keep `load` text-only: OSC 52 queries must never export clipboard images.
    pub fn load_for_paste(&mut self, allow_images: bool) -> String {
        let text = self.load(ClipboardType::Clipboard);
        if !text.is_empty() || !allow_images {
            return text;
        }

        #[cfg(target_os = "macos")]
        match crate::macos::clipboard::image_path() {
            Ok(Some(path)) => return path,
            Ok(None) => (),
            Err(err) => warn!("Unable to paste clipboard image: {err}"),
        }

        text
    }

    pub fn store(&mut self, ty: ClipboardType, text: impl Into<String>) {
        let clipboard = match (ty, &mut self.selection) {
            (ClipboardType::Selection, Some(provider)) => provider,
            (ClipboardType::Selection, None) => return,
            _ => &mut self.clipboard,
        };

        clipboard.set_contents(text.into()).unwrap_or_else(|err| {
            warn!("Unable to store text in clipboard: {err}");
        });
    }

    pub fn load(&mut self, ty: ClipboardType) -> String {
        let clipboard = match (ty, &mut self.selection) {
            (ClipboardType::Selection, Some(provider)) => provider,
            _ => &mut self.clipboard,
        };

        match clipboard.get_contents() {
            Err(err) => {
                debug!("Unable to load text from clipboard: {err}");
                String::new()
            },
            Ok(text) => text,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::error::Error;

    use super::*;

    struct TextClipboard(String);

    impl ClipboardProvider for TextClipboard {
        fn get_contents(&mut self) -> Result<String, Box<dyn Error + Send + Sync>> {
            Ok(self.0.clone())
        }

        fn set_contents(&mut self, text: String) -> Result<(), Box<dyn Error + Send + Sync>> {
            self.0 = text;
            Ok(())
        }
    }

    #[test]
    fn text_keeps_priority_for_explicit_paste() {
        let text = "echo 'hello'\n你好";
        let mut clipboard =
            Clipboard { clipboard: Box::new(TextClipboard(text.to_owned())), selection: None };
        assert_eq!(clipboard.load_for_paste(true), text);
        assert_eq!(clipboard.load_for_paste(false), text);
        assert_eq!(clipboard.load(ClipboardType::Clipboard), text);
    }

    #[test]
    fn text_only_paste_accepts_empty_clipboard() {
        let mut clipboard =
            Clipboard { clipboard: Box::new(TextClipboard(String::new())), selection: None };
        assert_eq!(clipboard.load_for_paste(false), "");
        assert_eq!(clipboard.load(ClipboardType::Clipboard), "");
    }
}
