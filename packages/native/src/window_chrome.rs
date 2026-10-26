//! Kybern: what an app-drawn title bar needs to look native off macOS (WP3).
//! `chrome` reports the window's state (maximized, active, decorations and
//! tiling, the controls the compositor offers, the desktop's button layout,
//! appearance, reduced motion); `observe` tells JS when any of it changes.
//! Windows draws nothing for us: the caption buttons are app elements with a
//! `windowControl` area. Linux draws them too when decorations are
//! client-side, and the app resizes the window from its edges.
use crate::renderer::GpuixView;
use std::sync::atomic::{AtomicU8, Ordering};

/// What a double click on a drag region does off macOS and Windows (where the
/// platform decides): set from the desktop's preference by JS.
static DOUBLE_CLICK: AtomicU8 = AtomicU8::new(DoubleClick::Maximize as u8);

// Read by the Linux drag region only.
#[cfg_attr(any(target_os = "macos", target_os = "windows"), allow(dead_code))]
#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum DoubleClick {
    Maximize = 0,
    Minimize = 1,
    Menu = 2,
    Nothing = 3,
}

pub(crate) fn set_double_click(action: &str) -> napi::Result<()> {
    let action = match action {
        "maximize" => DoubleClick::Maximize,
        "minimize" => DoubleClick::Minimize,
        "menu" => DoubleClick::Menu,
        "none" => DoubleClick::Nothing,
        other => return Err(napi::Error::from_reason(format!("Unknown double-click action {other:?}; use maximize, minimize, menu or none"))),
    };
    DOUBLE_CLICK.store(action as u8, Ordering::Relaxed);
    Ok(())
}

#[cfg_attr(any(target_os = "macos", target_os = "windows"), allow(dead_code))]
pub(crate) fn double_click() -> DoubleClick {
    match DOUBLE_CLICK.load(Ordering::Relaxed) {
        1 => DoubleClick::Minimize,
        2 => DoubleClick::Menu,
        3 => DoubleClick::Nothing,
        _ => DoubleClick::Maximize,
    }
}

pub(crate) fn resize_edge(edge: &str) -> napi::Result<gpui::ResizeEdge> {
    use gpui::ResizeEdge::*;
    Ok(match edge {
        "top" => Top,
        "top-right" => TopRight,
        "right" => Right,
        "bottom-right" => BottomRight,
        "bottom" => Bottom,
        "bottom-left" => BottomLeft,
        "left" => Left,
        "top-left" => TopLeft,
        other => return Err(napi::Error::from_reason(format!("Unknown resize edge {other:?}"))),
    })
}

fn platform() -> &'static str {
    if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(target_os = "windows") {
        "windows"
    } else {
        "linux"
    }
}

fn buttons(side: &[Option<gpui::WindowButton>]) -> Vec<&'static str> {
    side.iter().flatten().map(|button| button.id()).collect()
}

/// The window's title-bar state, as one JSON object.
pub(crate) fn chrome(window: &gpui::Window, cx: &gpui::App) -> serde_json::Value {
    let (decorations, tiling) = match window.window_decorations() {
        gpui::Decorations::Server => ("server", None),
        gpui::Decorations::Client { tiling } => ("client", Some(tiling)),
    };
    let controls = window.window_controls();
    let layout = cx.button_layout();
    let appearance = match window.appearance() {
        gpui::WindowAppearance::Light | gpui::WindowAppearance::VibrantLight => "light",
        gpui::WindowAppearance::Dark | gpui::WindowAppearance::VibrantDark => "dark",
    };
    serde_json::json!({
        "platform": platform(),
        "decorations": decorations,
        "tiling": tiling.map(|t| serde_json::json!({ "top": t.top, "left": t.left, "right": t.right, "bottom": t.bottom })),
        "maximized": window.is_maximized(),
        "fullscreen": window.is_fullscreen(),
        "active": window.is_window_active(),
        "resizable": window.is_resizable(),
        "minimizable": window.is_minimizable(),
        "controls": { "minimize": controls.minimize, "maximize": controls.maximize, "fullscreen": controls.fullscreen, "windowMenu": controls.window_menu },
        "buttonLayout": layout.map(|layout| serde_json::json!({ "left": buttons(&layout.left), "right": buttons(&layout.right) })),
        "appearance": appearance,
        "reducedMotion": reduced_motion(),
        "bounds": bounds(window, cx),
    })
}

/// The bounds to save and restore the window with: the restored (not maximized)
/// frame in GPUI's global logical coordinates, its state, and the display's id.
pub(crate) fn bounds(window: &gpui::Window, cx: &gpui::App) -> serde_json::Value {
    let (state, rect) = match window.window_bounds() {
        gpui::WindowBounds::Windowed(rect) => ("windowed", rect),
        gpui::WindowBounds::Maximized(rect) => ("maximized", rect),
        gpui::WindowBounds::Fullscreen(rect) => ("fullscreen", rect),
    };
    let display = window.display(cx).and_then(|display| display.uuid().ok()).map(|uuid| uuid.to_string());
    serde_json::json!({
        "state": state,
        "x": f32::from(rect.origin.x),
        "y": f32::from(rect.origin.y),
        "width": f32::from(rect.size.width),
        "height": f32::from(rect.size.height),
        "display": display,
    })
}

/// The display a saved frame was on, while it is still connected. Off macOS
/// only: macOS restores AppKit's own frame string.
#[cfg_attr(target_os = "macos", allow(dead_code))]
pub(crate) fn display_for(uuid: &str, cx: &gpui::App) -> Option<gpui::DisplayId> {
    cx.displays().into_iter().find(|display| display.uuid().is_ok_and(|id| id.to_string() == uuid)).map(|display| display.id())
}

/// Windows: "Animation effects" off (Settings › Accessibility › Visual effects)
/// is `SPI_GETCLIENTAREAANIMATION` false. `null` where the platform has no
/// such switch here (Linux reads GNOME's `enable-animations` in JS).
fn reduced_motion() -> Option<bool> {
    #[cfg(target_os = "windows")]
    unsafe {
        use windows::Win32::UI::WindowsAndMessaging::{SPI_GETCLIENTAREAANIMATION, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, SystemParametersInfoW};
        let mut enabled = windows::core::BOOL(1);
        let read = SystemParametersInfoW(SPI_GETCLIENTAREAANIMATION, 0, Some(&mut enabled as *mut _ as *mut std::ffi::c_void), SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0));
        return read.is_ok().then_some(!enabled.as_bool());
    }
    #[allow(unreachable_code)]
    None
}

thread_local! {
    static OBSERVERS: std::cell::RefCell<Vec<gpui::Subscription>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// Call `callback` with the new `chrome` when the window's bounds, activation,
/// appearance or the desktop's button layout change, so JS follows the window
/// without a waited bridge call per change. Unchanged states are not sent.
/// `None` stops.
pub(crate) fn observe(
    callback: Option<napi::threadsafe_function::ThreadsafeFunction<serde_json::Value>>,
    window: &mut gpui::Window,
    cx: &mut gpui::Context<GpuixView>,
) {
    use napi::threadsafe_function::ThreadsafeFunctionCallMode;
    let Some(callback) = callback else {
        OBSERVERS.with(|observers| observers.borrow_mut().clear());
        return;
    };
    let callback = std::sync::Arc::new(callback);
    let last = std::rc::Rc::new(std::cell::RefCell::new(serde_json::Value::Null));
    let notify = move || {
        let callback = callback.clone();
        let last = last.clone();
        move |_: &mut GpuixView, window: &mut gpui::Window, cx: &mut gpui::Context<GpuixView>| {
            let state = chrome(window, cx);
            if *last.borrow() != state {
                *last.borrow_mut() = state.clone();
                callback.call(Ok(state), ThreadsafeFunctionCallMode::NonBlocking);
            }
        }
    };
    let subscriptions = vec![
        cx.observe_window_bounds(window, notify()),
        cx.observe_window_activation(window, notify()),
        cx.observe_window_appearance(window, notify()),
        cx.observe_button_layout_changed(window, notify()),
    ];
    OBSERVERS.with(|observers| *observers.borrow_mut() = subscriptions);
}
