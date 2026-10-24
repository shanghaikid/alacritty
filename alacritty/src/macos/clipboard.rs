//! Export clipboard images for command-line applications through explicit paste.

use std::io::{self, Write};
use std::path::Path;

use objc2::rc::{Retained, autoreleasepool};
use objc2_app_kit::{
    NSBitmapImageFileType, NSBitmapImageRep, NSPasteboard, NSPasteboardTypePNG,
    NSPasteboardTypeTIFF,
};
use objc2_foundation::{NSData, NSDictionary, ns_string};

/// Return a shell-quoted PNG path, followed by a space for subsequent input.
pub fn image_path() -> io::Result<Option<String>> {
    autoreleasepool(|_| {
        let pasteboard = NSPasteboard::generalPasteboard();
        let Some(data) = png_data(&pasteboard)? else { return Ok(None) };
        save_image(&data.to_vec(), &std::env::temp_dir()).map(Some)
    })
}

fn png_data(pasteboard: &NSPasteboard) -> io::Result<Option<Retained<NSData>>> {
    // SAFETY: These immutable AppKit constants are available on supported macOS versions.
    let types = unsafe { [NSPasteboardTypePNG, NSPasteboardTypeTIFF, ns_string!("public.jpeg")] };
    let mut invalid_image = false;
    for ty in types {
        let Some(data) = pasteboard.dataForType(ty) else { continue };
        if let Some(png) = to_png(data) {
            return Ok(Some(png));
        }
        invalid_image = true;
    }

    if invalid_image {
        Err(io::Error::new(io::ErrorKind::InvalidData, "could not decode clipboard image as PNG"))
    } else {
        Ok(None)
    }
}

fn to_png(data: Retained<NSData>) -> Option<Retained<NSData>> {
    let image = NSBitmapImageRep::imageRepWithData(&data)?;
    // Inspect the actual encoding, since the clipboard's advertised type may be wrong.
    if data.iter().take(8).eq(*b"\x89PNG\r\n\x1a\n") {
        return Some(data);
    }

    // SAFETY: An empty properties dictionary contains no incorrectly typed values.
    unsafe {
        image.representationUsingType_properties(NSBitmapImageFileType::PNG, &NSDictionary::new())
    }
}

fn save_image(png: &[u8], directory: &Path) -> io::Result<String> {
    // NamedTempFile creates a unique file with owner-only permissions. Keep it after
    // paste and application exit since the recipient may read it asynchronously.
    let mut file = tempfile::Builder::new()
        .prefix("alacritty-clipboard-")
        .suffix(".png")
        .tempfile_in(directory)?;
    let path = quote_path(file.path())?;
    file.write_all(png)?;
    file.keep().map_err(|err| err.error)?;
    Ok(path + " ")
}

fn quote_path(path: &Path) -> io::Result<String> {
    let path =
        path.to_str().filter(|path| !path.chars().any(char::is_control)).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "image path contains unsupported characters")
        })?;
    if path.bytes().all(|c| c.is_ascii_alphanumeric() || b"/._-".contains(&c)) {
        Ok(path.to_owned())
    } else {
        Ok(format!("'{}'", path.replace('\'', "'\\''")))
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::process::Command;

    use super::*;

    const PNG: &[u8] = include_bytes!("../../../extra/logo/compat/alacritty-term.png");

    #[test]
    fn image_conversion_uses_actual_format_and_preserves_png() {
        autoreleasepool(|_| {
            assert_eq!(to_png(NSData::with_bytes(PNG)).unwrap().to_vec(), PNG);
            assert!(to_png(NSData::with_bytes(b"not an image")).is_none());
            let original = NSBitmapImageRep::imageRepWithData(&NSData::with_bytes(PNG)).unwrap();
            for format in [NSBitmapImageFileType::TIFF, NSBitmapImageFileType::JPEG] {
                let data = unsafe {
                    original.representationUsingType_properties(format, &NSDictionary::new())
                }
                .unwrap();
                let png = to_png(data).unwrap();
                assert!(png.to_vec().starts_with(b"\x89PNG\r\n\x1a\n"));
                let decoded = NSBitmapImageRep::imageRepWithData(&png).unwrap();
                assert_eq!(decoded.pixelsWide(), original.pixelsWide());
                assert_eq!(decoded.pixelsHigh(), original.pixelsHigh());
            }
        });
    }

    #[test]
    #[ignore = "requires access to the macOS pasteboard server"]
    fn native_image_formats_roundtrip_without_changing_clipboard() {
        autoreleasepool(|_| {
            // Use an isolated pasteboard, never the user's system clipboard.
            let pasteboard = NSPasteboard::pasteboardWithUniqueName();
            let original = NSBitmapImageRep::imageRepWithData(&NSData::with_bytes(PNG)).unwrap();
            let formats = unsafe {
                [
                    (NSPasteboardTypePNG, NSBitmapImageFileType::PNG),
                    (NSPasteboardTypeTIFF, NSBitmapImageFileType::TIFF),
                    (ns_string!("public.jpeg"), NSBitmapImageFileType::JPEG),
                ]
            };
            for (ty, format) in formats {
                let data = unsafe {
                    original.representationUsingType_properties(format, &NSDictionary::new())
                }
                .unwrap();
                pasteboard.clearContents();
                assert!(pasteboard.setData_forType(Some(&data), ty));
                let change_count = pasteboard.changeCount();
                let png = png_data(&pasteboard).unwrap().unwrap();
                assert!(png.to_vec().starts_with(b"\x89PNG\r\n\x1a\n"));
                let decoded = NSBitmapImageRep::imageRepWithData(&png).unwrap();
                assert_eq!(decoded.pixelsWide(), original.pixelsWide());
                assert_eq!(decoded.pixelsHigh(), original.pixelsHigh());
                assert_eq!(pasteboard.changeCount(), change_count);
                assert_eq!(pasteboard.dataForType(ty).unwrap().to_vec(), data.to_vec());
            }
            pasteboard.clearContents();
        });
    }

    #[test]
    #[ignore = "requires access to the macOS pasteboard server"]
    fn empty_and_malformed_images() {
        autoreleasepool(|_| {
            let pasteboard = NSPasteboard::pasteboardWithUniqueName();
            assert!(png_data(&pasteboard).unwrap().is_none());
            let invalid = NSData::with_bytes(b"not an image");
            assert!(pasteboard.setData_forType(Some(&invalid), unsafe { NSPasteboardTypePNG }));
            assert_eq!(png_data(&pasteboard).unwrap_err().kind(), io::ErrorKind::InvalidData);

            // A broken preferred representation must not hide a valid fallback.
            let original = NSBitmapImageRep::imageRepWithData(&NSData::with_bytes(PNG)).unwrap();
            let tiff = unsafe {
                original.representationUsingType_properties(
                    NSBitmapImageFileType::TIFF,
                    &NSDictionary::new(),
                )
            }
            .unwrap();
            assert!(pasteboard.setData_forType(Some(&tiff), unsafe { NSPasteboardTypeTIFF }));
            assert!(png_data(&pasteboard).unwrap().unwrap().to_vec().starts_with(b"\x89PNG"));

            // A valid image with an incorrect advertised type must still be saved as PNG.
            pasteboard.clearContents();
            assert!(pasteboard.setData_forType(Some(&tiff), unsafe { NSPasteboardTypePNG }));
            assert!(png_data(&pasteboard).unwrap().unwrap().to_vec().starts_with(b"\x89PNG"));
            pasteboard.clearContents();
        });
    }

    #[test]
    fn exported_files_are_private_unique_and_survive_export() {
        let directory = tempfile::tempdir().unwrap();
        let first = save_image(PNG, directory.path()).unwrap();
        let second = save_image(PNG, directory.path()).unwrap();
        assert_ne!(first, second);
        for pasted in [first, second] {
            assert!(pasted.ends_with(".png "));
            let path = Path::new(pasted.trim_end());
            assert_eq!(fs::read(path).unwrap(), PNG);
            assert_eq!(fs::metadata(path).unwrap().permissions().mode() & 0o777, 0o600);
        }
        assert!(save_image(PNG, &directory.path().join("missing")).is_err());
    }

    #[test]
    fn relative_temp_directory_produces_absolute_path() {
        let directory = tempfile::tempdir_in(".").unwrap();
        let relative = Path::new(directory.path().file_name().unwrap());
        assert!(relative.is_relative());
        let pasted = save_image(PNG, relative).unwrap();
        let path = Path::new(pasted.trim_end());
        assert!(path.is_absolute());
        assert_eq!(fs::read(path).unwrap(), PNG);
    }

    #[test]
    fn quoted_paths_are_literal_shell_arguments() {
        for path in ["/tmp/simple.png", "/tmp/a b/'截图'/$HOME/$(printf injected);&.png"] {
            let quoted = quote_path(Path::new(path)).unwrap();
            let output = Command::new("/bin/sh")
                .args(["-c", &format!("printf '%s' {quoted}")])
                .output()
                .unwrap();
            assert!(output.status.success());
            assert_eq!(output.stdout, path.as_bytes());
        }
        assert!(quote_path(Path::new("/tmp/a\nb.png")).is_err());
        assert!(quote_path(Path::new("/tmp/a\x1bb.png")).is_err());
    }
}
