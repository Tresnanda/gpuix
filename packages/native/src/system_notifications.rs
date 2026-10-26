//! Kybern WP5: system notifications on Windows and Linux.
//!
//! The posting and the click callback are GPUI's own (`App::show_system_notification`
//! and `App::on_system_notification_response`: toasts on Windows, the freedesktop
//! D-Bus interface on Linux; macOS keeps its UserNotifications companion). This
//! module adds what the app needs around them: whether notifications can show at
//! all (`status`), and a click handler that first brings the window forward, then
//! tells JavaScript which tag was clicked. Everything here that is not a platform
//! call is a pure function with a test.
use napi::bindgen_prelude::*;
#[cfg(any(target_os = "windows", target_os = "linux", target_os = "freebsd"))]
use napi::threadsafe_function::{ThreadsafeFunction, ThreadsafeFunctionCallMode};

/// What JavaScript receives for a click: the tag it posted, and the pressed
/// action button (null for the notification body).
pub(crate) fn response_event(tag: &str, action_id: Option<&str>) -> serde_json::Value {
    serde_json::json!({ "tag": tag, "action": action_id })
}

/// The `NotificationStatus` strings `src/native/notifications.ts` shares with
/// the macOS companion. A platform that cannot say whether the user disabled
/// the app's notifications reports `granted` once the service is reachable.
pub(crate) fn status_name(reachable: bool, disabled: bool) -> &'static str {
    match (reachable, disabled) {
        (false, _) => "unsupported",
        (true, true) => "denied",
        (true, false) => "granted",
    }
}

#[cfg(any(target_os = "linux", target_os = "freebsd"))]
fn platform_status(_identifier: &str) -> &'static str {
    // Without a notification daemon (a headless session, a bare window manager)
    // Notify would fail; report it up front so Settings says so and nothing posts.
    match notify_rust::get_server_information() {
        Ok(server) => {
            log::debug!("notification server: {} {}", server.name, server.version);
            status_name(true, false)
        }
        Err(error) => {
            log::debug!("no notification server: {error}");
            status_name(false, false)
        }
    }
}

#[cfg(target_os = "windows")]
fn platform_status(identifier: &str) -> &'static str {
    use windows::core::HSTRING;
    use windows::UI::Notifications::{NotificationSetting, ToastNotificationManager};
    use windows::Win32::System::WinRT::{RoInitialize, RO_INIT_MULTITHREADED};
    // This runs on a libuv worker. An already initialised thread answers with
    // S_FALSE or RPC_E_CHANGED_MODE, and WinRT works in both cases.
    let _ = unsafe { RoInitialize(RO_INIT_MULTITHREADED) };
    let setting = ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(identifier))
        .and_then(|notifier| notifier.Setting());
    match setting {
        Ok(setting) => status_name(true, setting != NotificationSetting::Enabled),
        Err(error) => {
            log::debug!("toast notifier unavailable: {error}");
            status_name(false, false)
        }
    }
}

#[cfg(not(any(target_os = "linux", target_os = "freebsd", target_os = "windows")))]
fn platform_status(_identifier: &str) -> &'static str {
    "unsupported"
}

/// `systemNotificationStatus(identifier)`: runs off the JS thread because the
/// Linux probe is a D-Bus round trip that can wait for service activation.
pub struct StatusTask {
    identifier: String,
}

impl StatusTask {
    pub fn new(identifier: String) -> Self {
        Self { identifier }
    }
}

impl Task for StatusTask {
    type Output = &'static str;
    type JsValue = String;

    fn compute(&mut self) -> Result<Self::Output> {
        Ok(platform_status(&self.identifier))
    }

    fn resolve(&mut self, _: Env, output: Self::Output) -> Result<String> {
        Ok(output.to_string())
    }
}

/// Registers the click handler (replacing an earlier one). A click raises the
/// window before JavaScript hears of it, the same order as the macOS delegate;
/// `None` leaves clicks unhandled.
#[cfg(any(target_os = "windows", target_os = "linux", target_os = "freebsd"))]
pub(crate) fn set_click_callback(
    window: gpui::AnyWindowHandle,
    cx: &mut gpui::App,
    callback: Option<ThreadsafeFunction<serde_json::Value>>,
) {
    cx.on_system_notification_response(move |response, cx| {
        let Some(callback) = &callback else { return };
        window
            .update(cx, |_view, window, cx| {
                cx.activate(true);
                window.activate_window();
            })
            .ok();
        callback.call(
            Ok(response_event(&response.tag, response.action_id.as_deref())),
            ThreadsafeFunctionCallMode::NonBlocking,
        );
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn click_events_carry_the_tag_and_the_action() {
        assert_eq!(
            response_event("thread-1", None),
            serde_json::json!({ "tag": "thread-1", "action": null })
        );
        assert_eq!(
            response_event("thread-1", Some("open")),
            serde_json::json!({ "tag": "thread-1", "action": "open" })
        );
    }

    #[test]
    fn status_names_match_the_macos_companion() {
        assert_eq!(status_name(true, false), "granted");
        assert_eq!(status_name(true, true), "denied");
        // A disabled flag means nothing when the service cannot be reached.
        assert_eq!(status_name(false, true), "unsupported");
        assert_eq!(status_name(false, false), "unsupported");
    }
}
