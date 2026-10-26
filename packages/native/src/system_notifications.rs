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
use std::sync::Mutex;
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

/// What `systemNotificationStatus` resolves to (as a JSON string): the status name, and on Linux
/// whether the server renders `<b>`/`<a>` markup in a body (`body-markup`;
/// null when it could not say, or off Linux).
pub(crate) fn status_value(status: &str, markup: Option<bool>) -> serde_json::Value {
    serde_json::json!({ "status": status, "markup": markup })
}

#[cfg(any(target_os = "linux", target_os = "freebsd"))]
fn platform_status(_identifier: &str) -> serde_json::Value {
    // Without a notification daemon (a headless session, a bare window manager)
    // Notify would fail; report it up front so Settings says so and nothing posts.
    match notify_rust::get_server_information() {
        Ok(server) => {
            log::debug!("notification server: {} {}", server.name, server.version);
            // Asked once, with the probe: a body is escaped only for a server that reads markup.
            let markup = notify_rust::get_capabilities()
                .map(|capabilities| capabilities.iter().any(|capability| capability == "body-markup"))
                .map_err(|error| log::debug!("notification capabilities: {error}"))
                .ok();
            status_value(status_name(true, false), markup)
        }
        Err(error) => {
            log::debug!("no notification server: {error}");
            status_value(status_name(false, false), None)
        }
    }
}

#[cfg(target_os = "windows")]
fn platform_status(identifier: &str) -> serde_json::Value {
    use windows::core::HSTRING;
    use windows::UI::Notifications::{NotificationSetting, ToastNotificationManager};
    use windows::Win32::System::WinRT::{RoInitialize, RO_INIT_MULTITHREADED};
    // This runs on a libuv worker. An already initialised thread answers with
    // S_FALSE or RPC_E_CHANGED_MODE, and WinRT works in both cases.
    let _ = unsafe { RoInitialize(RO_INIT_MULTITHREADED) };
    let notifier = match ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(identifier)) {
        Ok(notifier) => notifier,
        Err(error) => {
            log::warn!("toast notifier unavailable for {identifier}: {error}");
            return status_value(status_name(false, false), None);
        }
    };
    // A notifier that exists but cannot say (some Windows Server images) is tried: a post that goes nowhere is harmless.
    match notifier.Setting() {
        Ok(setting) => status_value(status_name(true, setting != NotificationSetting::Enabled), None),
        Err(error) => {
            log::warn!("toast setting unreadable for {identifier}: {error}");
            status_value(status_name(true, false), None)
        }
    }
}

#[cfg(not(any(target_os = "linux", target_os = "freebsd", target_os = "windows")))]
fn platform_status(_identifier: &str) -> serde_json::Value {
    status_value("unsupported", None)
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
    type Output = serde_json::Value;
    type JsValue = String;

    fn compute(&mut self) -> Result<Self::Output> {
        Ok(platform_status(&self.identifier))
    }

    fn resolve(&mut self, _: Env, output: Self::Output) -> Result<String> {
        Ok(output.to_string())
    }
}

/// The identity JavaScript chose before the window exists (`kybernSetAppIdentity`).
static EARLY_IDENTITY: Mutex<Option<(String, String)>> = Mutex::new(None);

/// `kybernSetAppIdentity(identifier, name)`: the app's notification identity,
/// given at process start, before any window exists. The window thread applies
/// it first thing (`apply_early_identity`), so on Windows the process
/// AppUserModelID is set before the first window is created and the taskbar
/// groups it; a later `setAppIdentity` on the renderer still works and wins.
/// A free function (like `kybernCapabilities`), read through the addon itself:
/// the renderer does not exist yet.
#[napi_derive::napi]
pub fn kybern_set_app_identity(identifier: String, name: String) {
    if let Ok(mut early) = EARLY_IDENTITY.lock() {
        *early = Some((identifier, name));
    }
}

/// The identity given before the window opened, if any.
pub(crate) fn early_identity() -> Option<(String, String)> {
    EARLY_IDENTITY.lock().ok().and_then(|early| early.clone())
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
    fn the_status_value_carries_the_markup_capability() {
        assert_eq!(
            status_value("granted", Some(true)),
            serde_json::json!({ "status": "granted", "markup": true })
        );
        assert_eq!(
            status_value("unsupported", None),
            serde_json::json!({ "status": "unsupported", "markup": null })
        );
    }

    #[test]
    fn the_early_identity_is_kept_for_the_window_thread() {
        kybern_set_app_identity("dev.kybern.gpuix.test".into(), "Kybern".into());
        assert_eq!(early_identity(), Some(("dev.kybern.gpuix.test".into(), "Kybern".into())));
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
