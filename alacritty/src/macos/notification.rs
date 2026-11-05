//! Command completion notifications. Never include shell input or command output.
use std::time::Duration;

use block2::RcBlock;
use objc2::runtime::Bool;
use objc2_foundation::{NSBundle, NSError, NSString};
use objc2_user_notifications::{
    UNAuthorizationOptions, UNMutableNotificationContent, UNNotificationRequest,
    UNUserNotificationCenter,
};

pub fn command_finished(window: u64, status: Option<u32>, elapsed: Duration) {
    // The notification center raises an Objective-C exception for unbundled CLI binaries.
    if NSBundle::mainBundle().bundleIdentifier().is_none() {
        return;
    }
    let body = match status {
        Some(0) => format!("Command succeeded after {} seconds", elapsed.as_secs()),
        Some(code) => {
            format!("Command exited with status {code} after {} seconds", elapsed.as_secs())
        },
        None => format!("Command finished after {} seconds", elapsed.as_secs()),
    };
    let completion = RcBlock::new(move |granted: Bool, error: *mut NSError| {
        if !error.is_null() {
            log::warn!("Could not request notification permission");
            return;
        }
        if !granted.as_bool() {
            return;
        }
        let content = UNMutableNotificationContent::new();
        content.setTitle(&NSString::from_str("Alacritty"));
        content.setBody(&NSString::from_str(&body));
        // Replace the previous notification for this terminal rather than accumulating a backlog.
        let id = NSString::from_str(&format!("alacritty-command-{window}"));
        let request =
            UNNotificationRequest::requestWithIdentifier_content_trigger(&id, &content, None);
        let delivered = RcBlock::new(|error: *mut NSError| {
            if !error.is_null() {
                log::warn!("Could not deliver command completion notification");
            }
        });
        UNUserNotificationCenter::currentNotificationCenter()
            .addNotificationRequest_withCompletionHandler(&request, Some(&delivered));
    });
    UNUserNotificationCenter::currentNotificationCenter()
        .requestAuthorizationWithOptions_completionHandler(
            UNAuthorizationOptions::Alert,
            &completion,
        );
}

// Agent notifications use a stable session identifier. The generation check is
// held through submission so a delayed permission callback cannot resurrect a
// notification after its question was answered.
use std::collections::HashMap;
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{AnyThread, DefinedClass, define_class, msg_send};
use objc2_foundation::{NSArray, NSObject, NSObjectProtocol, NSUserDefaults, ns_string};
use objc2_user_notifications::{
    UNNotification, UNNotificationPresentationOptions, UNNotificationResponse,
    UNUserNotificationCenterDelegate,
};
use winit::event_loop::EventLoopProxy;

use crate::cli::AgentNotification;
use crate::event::{Event, EventType};
use crate::polling::ipc::{self, SocketReply};

const PREFIX: &str = "alacritty-codex-";

pub fn enabled() -> bool {
    let defaults = NSUserDefaults::standardUserDefaults();
    let key = ns_string!("CodexNotificationsEnabled");
    defaults.objectForKey(key).is_none() || defaults.boolForKey(key)
}

struct DelegateState {
    proxy: EventLoopProxy<Event>,
}

define_class!(
    #[unsafe(super = NSObject)]
    #[ivars = DelegateState]
    struct AgentDelegate;

    unsafe impl NSObjectProtocol for AgentDelegate {}

    unsafe impl UNUserNotificationCenterDelegate for AgentDelegate {
        #[unsafe(method(userNotificationCenter:willPresentNotification:withCompletionHandler:))]
        fn present(
            &self,
            _center: &UNUserNotificationCenter,
            _notification: &UNNotification,
            completion: &block2::DynBlock<dyn Fn(UNNotificationPresentationOptions)>,
        ) {
            // The bridge already checks the exact window/pane's visibility.
            completion
                .call((UNNotificationPresentationOptions::Banner
                    | UNNotificationPresentationOptions::List,));
        }

        #[unsafe(method(userNotificationCenter:didReceiveNotificationResponse:withCompletionHandler:))]
        fn respond(
            &self,
            _center: &UNUserNotificationCenter,
            response: &UNNotificationResponse,
            completion: &block2::DynBlock<dyn Fn()>,
        ) {
            let id = response.notification().request().identifier().to_string();
            // Dismissal must not activate the terminal.
            if response.actionIdentifier().to_string()
                == "com.apple.UNNotificationDefaultActionIdentifier"
            {
                if let Some(session) = id.strip_prefix(PREFIX) {
                    let _ = self.ivars().proxy.send_event(Event::new(
                        EventType::AgentNotificationActivated(session.to_owned()),
                        None,
                    ));
                }
            }
            completion.call(());
        }
    }
);

// The only ivar is a thread-safe event proxy; notification callbacks may run on
// Apple's background queue. No AppKit objects are accessed from those callbacks.
unsafe impl Send for AgentDelegate {}
unsafe impl Sync for AgentDelegate {}

#[derive(Default)]
struct Entry {
    event: String,
    generation: u64,
    target: Option<AgentNotification>,
}

pub struct AgentNotifications {
    enabled: Arc<AtomicBool>,
    entries: Arc<Mutex<HashMap<String, Entry>>>,
    _delegate: Option<Retained<AgentDelegate>>,
}

impl AgentNotification {
    pub fn valid(&self) -> bool {
        let identifier = |s: &str| {
            !s.is_empty()
                && s.len() <= 160
                && s.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_.:".contains(&b))
        };
        identifier(&self.session)
            && (self.kind == "clear" || identifier(&self.event))
            && matches!(
                self.kind.as_str(),
                "done" | "goal" | "question" | "permission" | "blocked" | "limited" | "clear"
            )
            && self.project.len() <= 192
            && !self.project.chars().any(char::is_control)
            && self.tmux_socket.len() <= 1024
            && (self.tmux_socket.is_empty() || self.tmux_socket.starts_with('/'))
            && (self.pane.is_empty()
                || self
                    .pane
                    .strip_prefix('%')
                    .is_some_and(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit())))
            && (self.client_tty.is_empty()
                || self
                    .client_tty
                    .strip_prefix("/dev/ttys")
                    .is_some_and(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit())))
    }
}

impl AgentNotifications {
    pub fn new(proxy: EventLoopProxy<Event>) -> Self {
        let delegate = if NSBundle::mainBundle().bundleIdentifier().is_some() {
            let allocated = AgentDelegate::alloc().set_ivars(DelegateState { proxy });
            // SAFETY: NSObject's designated initializer has no extra preconditions.
            let delegate: Retained<AgentDelegate> = unsafe { msg_send![super(allocated), init] };
            UNUserNotificationCenter::currentNotificationCenter()
                .setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
            Some(delegate)
        } else {
            None
        };
        Self {
            enabled: Arc::new(AtomicBool::new(enabled())),
            entries: Default::default(),
            _delegate: delegate,
        }
    }

    pub fn set_enabled(&self, value: bool) {
        NSUserDefaults::standardUserDefaults()
            .setBool_forKey(value, ns_string!("CodexNotificationsEnabled"));
        self.refresh_enabled();
    }

    pub fn refresh_enabled(&self) -> bool {
        let value = enabled();
        self.enabled.store(value, Ordering::Release);
        if !value {
            self.clear_all();
        }
        value
    }

    pub fn clear_all(&self) {
        let sessions: Vec<_> = self.entries.lock().unwrap().keys().cloned().collect();
        for session in sessions {
            self.clear(&session);
        }
    }

    pub fn clear_window(&self, window: u64) {
        let sessions: Vec<_> = self
            .entries
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, entry)| {
                entry.target.as_ref().is_some_and(|target| target.window_id == window)
            })
            .map(|(session, _)| session.clone())
            .collect();
        for session in sessions {
            self.clear(&session);
        }
    }

    pub fn update(&self, notification: AgentNotification) {
        self.refresh_enabled();
        if !notification.valid() || self._delegate.is_none() {
            return;
        }
        if notification.kind == "clear" {
            self.clear(&notification.session);
            return;
        }
        if !enabled() || !self.enabled.load(Ordering::Acquire) {
            return;
        }
        let generation = {
            let mut entries = self.entries.lock().unwrap();
            // Only local IPC can add entries, and bound even that input.
            if entries.len() >= 1024 && !entries.contains_key(&notification.session) {
                return;
            }
            let entry = entries.entry(notification.session.clone()).or_default();
            if entry.event == notification.event {
                return;
            }
            entry.generation += 1;
            entry.event.clone_from(&notification.event);
            entry.target = Some(notification.clone());
            entry.generation
        };
        let entries = self.entries.clone();
        let is_enabled = self.enabled.clone();
        let completion = RcBlock::new(move |granted: Bool, error: *mut NSError| {
            if !is_enabled.load(Ordering::Acquire) {
                return;
            }
            if !error.is_null() || !granted.as_bool() {
                log::warn!("Codex notifications are not authorized in macOS notification settings");
                return;
            }
            let entries = entries.lock().unwrap();
            let Some(entry) = entries.get(&notification.session) else {
                return;
            };
            if entry.generation != generation || entry.target.is_none() {
                return;
            }
            let content = UNMutableNotificationContent::new();
            let title = if notification.project.is_empty() {
                "Codex".into()
            } else {
                format!("Codex · {}", notification.project)
            };
            content.setTitle(&NSString::from_str(&title));
            content.setBody(&NSString::from_str(match notification.kind.as_str() {
                "goal" => "目标已完成",
                "question" => "有问题需要你回答",
                "permission" => "正在等待你的审批",
                "blocked" => "任务遇到阻碍，需要你处理",
                "limited" => "任务因预算或用量限制停止",
                _ => "任务已完成",
            }));
            let id = NSString::from_str(&format!("{PREFIX}{}", notification.session));
            let request =
                UNNotificationRequest::requestWithIdentifier_content_trigger(&id, &content, None);
            let delivered = RcBlock::new(|error: *mut NSError| {
                if !error.is_null() {
                    log::warn!("Could not deliver Codex notification");
                }
            });
            UNUserNotificationCenter::currentNotificationCenter()
                .addNotificationRequest_withCompletionHandler(&request, Some(&delivered));
        });
        UNUserNotificationCenter::currentNotificationCenter()
            .requestAuthorizationWithOptions_completionHandler(
                UNAuthorizationOptions::Alert,
                &completion,
            );
    }

    pub fn clear(&self, session: &str) {
        let mut entries = self.entries.lock().unwrap();
        if let Some(entry) = entries.get_mut(session) {
            entry.generation += 1;
            entry.target = None;
        }
        if self._delegate.is_none() {
            return;
        }
        let ids =
            NSArray::from_retained_slice(&[NSString::from_str(&format!("{PREFIX}{session}"))]);
        let center = UNUserNotificationCenter::currentNotificationCenter();
        center.removePendingNotificationRequestsWithIdentifiers(&ids);
        center.removeDeliveredNotificationsWithIdentifiers(&ids);
    }

    pub fn activate(&self, session: &str) -> Option<AgentNotification> {
        let target = self.entries.lock().unwrap().get(session)?.target.clone();
        self.clear(session);
        target
    }
}

pub fn list_delivered(stream: UnixStream) {
    if NSBundle::mainBundle().bundleIdentifier().is_none() {
        return;
    }
    let completion = RcBlock::new(move |items: std::ptr::NonNull<NSArray<UNNotification>>| {
        // SAFETY: Apple passes a non-null NSArray valid for this callback.
        let items = unsafe { items.as_ref() };
        let ids: Vec<_> = (0..items.count())
            .map(|i| items.objectAtIndex(i).request().identifier().to_string())
            .filter(|id| id.starts_with(PREFIX))
            .collect();
        if let Ok(mut stream) = stream.try_clone() {
            ipc::send_reply(
                &mut stream,
                SocketReply::ProgramStatus(serde_json::to_string(&ids).unwrap()),
            );
        }
    });
    UNUserNotificationCenter::currentNotificationCenter()
        .getDeliveredNotificationsWithCompletionHandler(&completion);
}

/// This is only invoked by a native notification click, never by PTY output.
pub fn activate_pane(target: AgentNotification) {
    if target.tmux_socket.is_empty() || target.pane.is_empty() {
        return;
    }
    std::thread::spawn(move || {
        use std::process::{Command, Stdio};
        let Ok(exe) = std::env::current_exe() else {
            return;
        };
        let Some(dir) = exe.parent() else {
            return;
        };
        let tmux = dir.join("tmux");
        let run = |args: &[&str]| {
            Command::new(&tmux)
                .args(["-S", &target.tmux_socket])
                .args(args)
                .stdin(Stdio::null())
                .output()
        };
        if let Ok(output) = run(&["display-message", "-p", "-t", &target.pane, "#{session_id}"]) {
            if !output.status.success() {
                return;
            }
            let session = String::from_utf8_lossy(&output.stdout).trim().to_owned();
            if !target.client_tty.is_empty() {
                let _ = run(&["switch-client", "-c", &target.client_tty, "-t", &session]);
            }
            let _ = run(&["select-window", "-t", &target.pane]);
            let _ = run(&["select-pane", "-t", &target.pane]);
        }
    });
}

#[cfg(test)]
mod agent_tests {
    use super::*;
    #[test]
    fn rejects_unsafe_or_unbounded_routing_fields() {
        let mut n = AgentNotification {
            session: "test-session".into(),
            event: "turn-1:done".into(),
            kind: "done".into(),
            project: "project".into(),
            window_id: 1,
            tmux_socket: "/tmp/tmux-501/default".into(),
            pane: "%12".into(),
            client_tty: "/dev/ttys001".into(),
        };
        assert!(n.valid());
        n.pane = "; run-shell evil".into();
        assert!(!n.valid());
        n.pane.clear();
        n.session = "x".repeat(161);
        assert!(!n.valid());
        n.session = "ok".into();
        n.project = "line\nbreak".into();
        assert!(!n.valid());
    }
}
