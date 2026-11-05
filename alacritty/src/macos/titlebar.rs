//! Double-click actions for the title bar over a full-size terminal content view.

use std::cell::Cell;
use std::ptr::{self, NonNull};

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{ClassType, MainThreadMarker};
use objc2_app_kit::{NSControl, NSEvent, NSEventMask, NSEventType, NSWindow, NSWindowStyleMask};
use objc2_foundation::{NSObjectProtocol, NSPoint, NSRect, NSUserDefaults, ns_string};

/// Removes the local event monitor before winit destroys the window.
pub struct Titlebar {
    monitor: Retained<AnyObject>,
    _main_thread: MainThreadMarker,
}

impl Titlebar {
    pub fn new(window: Retained<NSWindow>) -> Option<Self> {
        let mtm = MainThreadMarker::new().expect("title-bar events require the main thread");
        let swallow_release = Cell::new(false);
        let restore_frame = Cell::new(None);
        let handler = RcBlock::new(move |event: NonNull<NSEvent>| {
            // SAFETY: AppKit supplies a live event for the duration of the callback.
            let event_ref = unsafe { event.as_ref() };
            if !event_ref.window(mtm).is_some_and(|target| ptr::eq(&*target, &*window)) {
                return event.as_ptr();
            }
            if event_ref.r#type() == NSEventType::LeftMouseUp {
                return if swallow_release.replace(false) {
                    ptr::null_mut()
                } else {
                    event.as_ptr()
                };
            }
            swallow_release.set(false);
            if event_ref.clickCount() != 2 || !is_titlebar_background(&window, event_ref, mtm) {
                return event.as_ptr();
            }

            // Consume the second press and release so neither AppKit nor the terminal
            // also handles the double-click. Single clicks and dragging stay native.
            swallow_release.set(true);
            let defaults = NSUserDefaults::standardUserDefaults();
            let preference = defaults.stringForKey(ns_string!("AppleActionOnDoubleClick"));
            match action(preference.as_ref().map(|value| value.to_string()).as_deref()) {
                Action::Minimize => window.performMiniaturize(None),
                Action::Zoom => window.performZoom(None),
                Action::Fill => {
                    if let Some(screen) = window.screen() {
                        let current = window.frame();
                        let target = screen.visibleFrame();
                        if let Some(next) = fill_frame(current, target, &restore_frame) {
                            window.setFrame_display_animate(next, true, true);
                        } else {
                            // A window maximized at startup already has an AppKit
                            // restore frame, even before our first Fill action.
                            window.performZoom(None);
                        }
                    }
                },
                Action::None => (),
            }
            ptr::null_mut()
        });
        // SAFETY: The callback only returns the original event or null. AppKit copies
        // the block and calls it on the main thread. Drop removes the monitor.
        let monitor = unsafe {
            NSEvent::addLocalMonitorForEventsMatchingMask_handler(
                NSEventMask::LeftMouseDown | NSEventMask::LeftMouseUp,
                &handler,
            )
        };
        if monitor.is_none() {
            log::warn!("Could not install transparent title-bar double-click handler");
        }
        monitor.map(|monitor| Self { monitor, _main_thread: mtm })
    }
}

impl Drop for Titlebar {
    fn drop(&mut self) {
        // SAFETY: This token came from addLocalMonitor and is removed exactly once.
        unsafe { NSEvent::removeMonitor(&self.monitor) };
    }
}

fn is_titlebar_background(window: &NSWindow, event: &NSEvent, mtm: MainThreadMarker) -> bool {
    let style = window.styleMask();
    if !style.contains(NSWindowStyleMask::Titled | NSWindowStyleMask::FullSizeContentView)
        || style.contains(NSWindowStyleMask::FullScreen)
    {
        return false;
    }

    let frame = window.frame();
    // Calculate the native title-bar height without including the tab strip or
    // accessories in contentLayoutRect. Coordinates here are window-local points.
    let content = NSWindow::contentRectForFrameRect_styleMask(
        frame,
        style & !NSWindowStyleMask::FullSizeContentView,
        mtm,
    );
    let title_height = frame.size.height - content.size.height;
    let point = event.locationInWindow();
    if !in_titlebar(point, frame.size.width, frame.size.height, title_height) {
        return false;
    }

    let Some(content_view) = window.contentView() else { return false };
    // Hit-test from the frame view so traffic lights and native accessories are
    // included, then exclude controls and all their descendants.
    // SAFETY: Accessed only on AppKit's main thread.
    let Some(frame_view) = (unsafe { content_view.superview() }) else { return false };
    let point = frame_view.convertPoint_fromView(point, None);
    let mut hit = frame_view.hitTest(point);
    while let Some(view) = hit {
        if view.isKindOfClass(NSControl::class()) {
            return false;
        }
        // SAFETY: Accessed only on AppKit's main thread.
        hit = unsafe { view.superview() };
    }
    true
}

fn in_titlebar(point: NSPoint, width: f64, height: f64, title_height: f64) -> bool {
    point.x >= 0. && point.x < width && point.y >= height - title_height && point.y < height
}

#[derive(Debug, PartialEq, Eq)]
enum Action {
    Zoom,
    Fill,
    Minimize,
    None,
}

fn action(preference: Option<&str>) -> Action {
    match preference {
        Some("Minimize") => Action::Minimize,
        Some("Fill") => Action::Fill,
        Some("None") => Action::None,
        _ => Action::Zoom,
    }
}

fn fill_frame(current: NSRect, target: NSRect, restore: &Cell<Option<NSRect>>) -> Option<NSRect> {
    let near = |a: f64, b: f64| (a - b).abs() <= 1.;
    let filled = near(current.origin.x, target.origin.x)
        && near(current.origin.y, target.origin.y)
        && near(current.size.width, target.size.width)
        && near(current.size.height, target.size.height);
    if filled {
        return restore.take();
    }
    restore.set(Some(current));
    Some(target)
}

#[cfg(test)]
mod tests {
    use super::*;
    use objc2_foundation::NSSize;

    #[test]
    fn titlebar_excludes_terminal_tab_strip_and_window_edges() {
        assert!(in_titlebar(NSPoint::new(300., 590.), 800., 600., 28.));
        for point in [(300., 571.), (300., 550.), (-1., 590.), (800., 590.), (300., 600.)] {
            assert!(!in_titlebar(NSPoint::new(point.0, point.1), 800., 600., 28.));
        }
    }

    #[test]
    fn honors_system_double_click_actions() {
        assert_eq!(action(None), Action::Zoom);
        assert_eq!(action(Some("Maximize")), Action::Zoom);
        assert_eq!(action(Some("Fill")), Action::Fill);
        assert_eq!(action(Some("Minimize")), Action::Minimize);
        assert_eq!(action(Some("None")), Action::None);
    }

    #[test]
    fn fill_restores_and_tracks_manual_resize() {
        let original = NSRect::new(NSPoint::new(50., 60.), NSSize::new(800., 600.));
        let screen = NSRect::new(NSPoint::new(0., 40.), NSSize::new(1500., 900.));
        let resized = NSRect::new(NSPoint::new(20., 80.), NSSize::new(900., 700.));
        let restore = Cell::new(None);
        assert_eq!(fill_frame(screen, screen, &restore), None);
        assert_eq!(fill_frame(original, screen, &restore), Some(screen));
        assert_eq!(fill_frame(screen, screen, &restore), Some(original));
        assert_eq!(fill_frame(original, screen, &restore), Some(screen));
        assert_eq!(fill_frame(resized, screen, &restore), Some(screen));
        assert_eq!(fill_frame(screen, screen, &restore), Some(resized));
    }
}
