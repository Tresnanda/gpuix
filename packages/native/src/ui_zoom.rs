//! Kybern (#292): whole-interface zoom. `Window::set_ui_zoom` (zed) renders the
//! window at the display scale times the zoom and lays it out in a viewport of
//! its content size divided by it, so every px value in the element tree scales
//! and the layout reflows, like a browser's page zoom. Window bounds, sizes and
//! the minimum size stay in OS points.
//!
//! The macOS View menu's Zoom In / Zoom Out / Actual Size (`app_menu.rs`) only
//! name their shortcuts: their bindings sit in a context no element has, so a
//! key never dispatches them and reaches JS's window key listener instead (where
//! an open image keeps the chords). A click on the menu item reaches
//! [`menu_command`], which hands the command to JS.

use std::cell::RefCell;

/// `{ zoom, scale, displayWidth, displayHeight }`: the zoom applied, the display's own scale
/// factor, and the visible size of the window's display in OS points (null when unknown).
pub(crate) fn report(window: &gpui::Window, cx: &gpui::App) -> serde_json::Value {
    let display = window.display(cx).map(|display| display.visible_bounds().size);
    serde_json::json!({
        "zoom": window.ui_zoom(),
        "scale": window.platform_scale_factor(),
        "displayWidth": display.map(|size| f32::from(size.width)),
        "displayHeight": display.map(|size| f32::from(size.height)),
    })
}

/// Sets the minimum content size first (a smaller window grows to it), then the zoom.
pub(crate) fn apply(window: &mut gpui::Window, cx: &mut gpui::App, zoom: f64, min: Option<(f64, f64)>) -> serde_json::Value {
    if let Some((width, height)) = min {
        window.set_window_min_size(Some(gpui::size(gpui::px(width as f32), gpui::px(height as f32))));
    }
    window.set_ui_zoom(zoom as f32, cx);
    report(window, cx)
}

thread_local! {
    static MENU_CALLBACK: RefCell<Option<std::sync::Arc<napi::threadsafe_function::ThreadsafeFunction<String>>>> = const { RefCell::new(None) };
}

pub(crate) fn set_menu_callback(callback: Option<napi::threadsafe_function::ThreadsafeFunction<String>>) {
    MENU_CALLBACK.with(|slot| *slot.borrow_mut() = callback.map(std::sync::Arc::new));
}

/// A View menu command: `zoomIn`, `zoomOut` or `actualSize`. A key equivalent that AppKit
/// matched after the window passed on the key is dropped: JS already had that key.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn menu_command(command: &'static str) {
    #[cfg(target_os = "macos")]
    if current_event_is_key_down() {
        return;
    }
    MENU_CALLBACK.with(|slot| {
        if let Some(callback) = slot.borrow().as_ref() {
            callback.call(Ok(command.to_string()), napi::threadsafe_function::ThreadsafeFunctionCallMode::NonBlocking);
        }
    });
}

#[cfg(target_os = "macos")]
#[allow(unexpected_cfgs)]
fn current_event_is_key_down() -> bool {
    use objc::{class, msg_send, sel, sel_impl};
    // NSEventTypeKeyDown
    const KEY_DOWN: u64 = 10;
    unsafe {
        let app: *mut objc::runtime::Object = msg_send![class!(NSApplication), sharedApplication];
        let event: *mut objc::runtime::Object = msg_send![app, currentEvent];
        if event.is_null() {
            return false;
        }
        let kind: u64 = msg_send![event, type];
        kind == KEY_DOWN
    }
}
