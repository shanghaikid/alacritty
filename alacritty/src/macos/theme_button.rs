//! A native title-bar accessory, with no extra toolbar row or terminal resizing.

use std::cell::RefCell;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use objc2::rc::Retained;
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSAccessibility, NSButton, NSLayoutAttribute, NSTitlebarAccessoryViewController, NSView,
    NSWindow,
};
use objc2_foundation::{NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize, ns_string};
use winit::window::Theme;

use crate::config::ui_config::Program;

struct TargetState {
    command: RefCell<Program>,
    busy: Arc<AtomicBool>,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements. All AppKit callbacks
    // and accesses to the command happen on the main thread.
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[ivars = TargetState]
    struct ThemeTarget;

    unsafe impl NSObjectProtocol for ThemeTarget {}

    impl ThemeTarget {
        #[unsafe(method(toggleTheme:))]
        fn toggle_theme(&self, _sender: &NSButton) {
            let state = self.ivars();
            if state.busy.swap(true, Ordering::AcqRel) {
                return;
            }
            let command = state.command.borrow().clone();
            let busy = state.busy.clone();
            // Do not block AppKit on file I/O or tmux. Keep failures in the
            // application log instead of silently discarding helper stderr.
            let result = std::thread::Builder::new().name("theme-toggle".into()).spawn(move || {
                match Command::new(command.program())
                    .args(command.args())
                    .stdin(Stdio::null())
                    .output()
                {
                    Ok(output) if output.status.success() => (),
                    Ok(output) => log::error!(
                        "Theme toggle failed: {}", String::from_utf8_lossy(&output.stderr).trim(),
                    ),
                    Err(error) => log::error!("Could not run theme toggle: {error}"),
                }
                busy.store(false, Ordering::Release);
            });
            if let Err(error) = result {
                state.busy.store(false, Ordering::Release);
                log::error!("Could not start theme toggle: {error}");
            }
        }
    }
);

pub struct ThemeButton {
    window: Retained<NSWindow>,
    accessory: Retained<NSTitlebarAccessoryViewController>,
    button: Retained<NSButton>,
    // NSControl's target is weak; retain it until after detaching the accessory.
    target: Retained<ThemeTarget>,
}

impl ThemeButton {
    pub fn new(window: Retained<NSWindow>, command: Program, theme: Option<Theme>) -> Self {
        let mtm = MainThreadMarker::new().expect("title-bar controls require the main thread");
        let allocated = ThemeTarget::alloc(mtm).set_ivars(TargetState {
            command: RefCell::new(command),
            busy: Arc::new(AtomicBool::new(false)),
        });
        // SAFETY: NSObject init and the selector below match our defined class.
        let target: Retained<ThemeTarget> = unsafe { msg_send![super(allocated), init] };
        let button = unsafe {
            NSButton::buttonWithTitle_target_action(
                ns_string!("◐"),
                Some(&target),
                Some(sel!(toggleTheme:)),
                mtm,
            )
        };
        button.setBordered(false);
        button.setFrame(NSRect::new(NSPoint::new(0., 0.), NSSize::new(30., 22.)));
        let view = NSView::new(mtm);
        view.setFrame(NSRect::new(NSPoint::new(0., 0.), NSSize::new(38., 22.)));
        view.addSubview(&button);
        let accessory = NSTitlebarAccessoryViewController::new(mtm);
        accessory.setView(&view);
        accessory.setLayoutAttribute(NSLayoutAttribute::Right);
        window.addTitlebarAccessoryViewController(&accessory);
        let owner = Self { window, accessory, button, target };
        owner.set_theme(theme);
        owner
    }

    pub fn update(&self, command: Program, theme: Option<Theme>) {
        *self.target.ivars().command.borrow_mut() = command;
        self.set_theme(theme);
    }

    fn set_theme(&self, theme: Option<Theme>) {
        let tooltip = if theme == Some(Theme::Dark) {
            ns_string!("Switch to Merino Light")
        } else {
            ns_string!("Switch to Merino Dark")
        };
        self.button.setToolTip(Some(tooltip));
        self.button.setAccessibilityLabel(Some(tooltip));
    }
}

impl Drop for ThemeButton {
    fn drop(&mut self) {
        let accessories = self.window.titlebarAccessoryViewControllers();
        for index in 0..accessories.count() {
            let accessory = accessories.objectAtIndex(index);
            if std::ptr::eq(&*accessory, &*self.accessory) {
                self.window.removeTitlebarAccessoryViewControllerAtIndex(index as _);
                break;
            }
        }
    }
}
