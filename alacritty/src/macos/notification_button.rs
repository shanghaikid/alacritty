//! Persistent Codex notification switch in the native title bar.
use objc2::rc::Retained;
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSAccessibility, NSButton, NSImage, NSLayoutAttribute, NSTitlebarAccessoryViewController,
    NSView, NSWindow,
};
use objc2_foundation::{NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize, ns_string};
use winit::event_loop::EventLoopProxy;

use crate::event::{Event, EventType};

struct TargetState {
    proxy: EventLoopProxy<Event>,
}

define_class!(
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[ivars = TargetState]
    struct NotificationTarget;

    unsafe impl NSObjectProtocol for NotificationTarget {}
    impl NotificationTarget {
        #[unsafe(method(toggleCodexNotifications:))]
        fn toggle(&self, _sender: &NSButton) {
            let _ = self.ivars().proxy.send_event(Event::new(EventType::ToggleAgentNotifications, None));
        }
    }
);

pub struct NotificationButton {
    window: Retained<NSWindow>,
    accessory: Retained<NSTitlebarAccessoryViewController>,
    button: Retained<NSButton>,
    _target: Retained<NotificationTarget>,
}

impl NotificationButton {
    pub fn new(window: Retained<NSWindow>, proxy: EventLoopProxy<Event>, enabled: bool) -> Self {
        let mtm = MainThreadMarker::new().expect("title bar runs on main thread");
        let allocated = NotificationTarget::alloc(mtm).set_ivars(TargetState { proxy });
        // SAFETY: NSObject initializer and our action signature match AppKit.
        let target: Retained<NotificationTarget> = unsafe { msg_send![super(allocated), init] };
        let button = unsafe {
            NSButton::buttonWithTitle_target_action(
                ns_string!(""),
                Some(&target),
                Some(sel!(toggleCodexNotifications:)),
                mtm,
            )
        };
        button.setBordered(false);
        button.setFrame(NSRect::new(NSPoint::new(0., 0.), NSSize::new(30., 22.)));
        let view = NSView::new(mtm);
        view.setFrame(NSRect::new(NSPoint::new(0., 0.), NSSize::new(34., 22.)));
        view.addSubview(&button);
        let accessory = NSTitlebarAccessoryViewController::new(mtm);
        accessory.setView(&view);
        accessory.setLayoutAttribute(NSLayoutAttribute::Right);
        window.addTitlebarAccessoryViewController(&accessory);
        let result = Self { window, accessory, button, _target: target };
        result.set_enabled(enabled);
        result
    }

    pub fn set_enabled(&self, enabled: bool) {
        let (symbol, tooltip) = if enabled {
            (ns_string!("bell"), ns_string!("Codex 通知已开启，点击关闭"))
        } else {
            (ns_string!("bell.slash"), ns_string!("Codex 通知已关闭，点击开启"))
        };
        if let Some(image) =
            NSImage::imageWithSystemSymbolName_accessibilityDescription(symbol, Some(tooltip))
        {
            image.setSize(NSSize::new(14., 14.));
            image.setTemplate(true);
            self.button.setImage(Some(&image));
        }
        self.button.setToolTip(Some(tooltip));
        self.button.setAccessibilityLabel(Some(tooltip));
    }
}

impl Drop for NotificationButton {
    fn drop(&mut self) {
        if !self.window.styleMask().contains(objc2_app_kit::NSWindowStyleMask::Titled) {
            return;
        }
        let accessories = self.window.titlebarAccessoryViewControllers();
        for index in 0..accessories.count() {
            if std::ptr::eq(&*accessories.objectAtIndex(index), &*self.accessory) {
                self.window.removeTitlebarAccessoryViewControllerAtIndex(index as _);
                break;
            }
        }
    }
}
