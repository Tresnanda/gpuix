//! Native titlebar gestures. macOS does not implement WindowControlArea hit tests.
//!
//! - macOS: the press starts AppKit's window move; a double click runs the
//!   "Double-click a window's title bar to" preference.
//! - Windows: the region is the native caption (`WindowControlArea::Drag`,
//!   HTCAPTION), so Windows itself drags, snaps, maximizes on a double click and
//!   opens the system menu on a right click. Nothing here may stop the press.
//!   Controls inside it are `WindowControlArea::Client`, so they keep theirs.
//! - Linux: like Zed's title bar, the move starts on the first drag after a
//!   press (starting on the press loses the second click of a double click to
//!   the compositor's grab); a double click runs the desktop's action
//!   (`window_chrome::double_click`); a right click opens the window menu.
use std::cell::Cell;

use gpui::{Div, Stateful, prelude::*};

thread_local! {
    /// Set while a press inside a drag region landed on a control (Tauri's
    /// `data-tauri-drag-region="deep"` skips buttons and other interactive
    /// children). Reset by the region's capture listener for every press.
    static CONTROL_PRESSED: Cell<bool> = const { Cell::new(false) };
    /// Linux: a press on the region that the next drag turns into a window move.
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    static SHOULD_MOVE: Cell<bool> = const { Cell::new(false) };
}

#[cfg(target_os = "macos")]
pub(crate) fn apply(el: Stateful<Div>) -> Stateful<Div> {
    el.window_control_area(gpui::WindowControlArea::Drag)
        .capture_any_mouse_down(|_, _, _| CONTROL_PRESSED.with(|pressed| pressed.set(false)))
        .on_mouse_down(gpui::MouseButton::Left, |event, window, cx| {
            // Controls bubble first, so a press on one has already been marked.
            if CONTROL_PRESSED.with(|pressed| pressed.get()) {
                return;
            }
            if event.click_count == 2 {
                // Zoom, minimize or nothing, per System Settings › Desktop & Dock
                // ("Double-click a window's title bar to"), like a native title bar.
                window.titlebar_double_click();
            } else {
                // Uses AppKit's current NSEvent; synthetic GPUI events cannot move a window.
                // Starting on the press (not the first drag) keeps the grab point under the cursor.
                window.start_window_move();
            }
            cx.stop_propagation();
        })
}

#[cfg(target_os = "windows")]
pub(crate) fn apply(el: Stateful<Div>) -> Stateful<Div> {
    el.window_control_area(gpui::WindowControlArea::Drag)
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub(crate) fn apply(el: Stateful<Div>) -> Stateful<Div> {
    use crate::window_chrome::DoubleClick;
    el.window_control_area(gpui::WindowControlArea::Drag)
        .capture_any_mouse_down(|_, _, _| CONTROL_PRESSED.with(|pressed| pressed.set(false)))
        .on_mouse_down(gpui::MouseButton::Left, |event, window, cx| {
            if CONTROL_PRESSED.with(|pressed| pressed.get()) {
                return;
            }
            if event.click_count == 2 {
                SHOULD_MOVE.with(|moving| moving.set(false));
                match crate::window_chrome::double_click() {
                    DoubleClick::Maximize if window.is_resizable() && window.window_controls().maximize => window.zoom_window(),
                    DoubleClick::Minimize if window.window_controls().minimize => window.minimize_window(),
                    DoubleClick::Menu if window.window_controls().window_menu => window.show_window_menu(event.position),
                    _ => {}
                }
            } else {
                SHOULD_MOVE.with(|moving| moving.set(true));
            }
            cx.stop_propagation();
        })
        .on_mouse_down(gpui::MouseButton::Right, |event, window, cx| {
            if CONTROL_PRESSED.with(|pressed| pressed.get()) || !window.window_controls().window_menu {
                return;
            }
            window.show_window_menu(event.position);
            cx.stop_propagation();
        })
        .on_mouse_move(|event, window, _| {
            if event.pressed_button == Some(gpui::MouseButton::Left) && SHOULD_MOVE.with(|moving| moving.replace(false)) {
                window.start_window_move();
            }
        })
        .on_mouse_up(gpui::MouseButton::Left, |_, _, _| SHOULD_MOVE.with(|moving| moving.set(false)))
        .on_mouse_up_out(gpui::MouseButton::Left, |_, _, _| SHOULD_MOVE.with(|moving| moving.set(false)))
}

/// Tauri's `isClickableElement` for GPUIX host elements: an element with its
/// own press or click handling, an interactive role or a tab stop, plus
/// `windowDrag={false}` (`data-tauri-drag-region="false"`).
pub(crate) fn is_control(
    events: &std::collections::HashSet<String>,
    props: &std::collections::HashMap<String, serde_json::Value>,
) -> bool {
    const ROLES: [&str; 8] = ["button", "link", "menuitem", "tab", "checkbox", "radio", "switch", "option"];
    events.contains("click")
        || events.contains("mouseDown")
        || props.get("role").and_then(|role| role.as_str()).is_some_and(|role| ROLES.contains(&role))
        || props.get("tabIndex").and_then(|index| index.as_i64()).is_some_and(|index| index >= 0)
        || props.get("windowDrag").and_then(|drag| drag.as_bool()) == Some(false)
}

/// Inside a drag region a control keeps the press instead of moving the window,
/// with any button: a right click on a control is the control's (its context
/// menu), not the window menu Linux opens on the region. On Windows that is
/// decided by the hit test, before any press reaches GPUI.
pub(crate) fn control(el: Stateful<Div>) -> Stateful<Div> {
    #[cfg(target_os = "windows")]
    return el.window_control_area(gpui::WindowControlArea::Client);
    #[cfg(not(target_os = "windows"))]
    el.on_any_mouse_down(|_, _, _| CONTROL_PRESSED.with(|pressed| pressed.set(true)))
}

/// A Windows caption button (`windowControl` prop): `minimize`, `maximize` or
/// `close`. Windows answers the hit test with HTMINBUTTON / HTMAXBUTTON /
/// HTCLOSE, so the button acts natively (Snap Layouts on hovering maximize) and
/// GPUI still sees the pointer for hover and press styles. It occludes, so the
/// drag area behind it is not under the pointer. Elsewhere it is a plain
/// element: JS handles the click.
pub(crate) fn caption_button(el: Stateful<Div>, kind: &str) -> Stateful<Div> {
    #[cfg(target_os = "windows")]
    {
        let area = match kind {
            "minimize" => gpui::WindowControlArea::Min,
            "maximize" => gpui::WindowControlArea::Max,
            "close" => gpui::WindowControlArea::Close,
            _ => return el,
        };
        return el.window_control_area(area).occlude();
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = kind;
        el
    }
}
