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
