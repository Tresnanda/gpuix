//! Native titlebar gestures. macOS does not implement WindowControlArea hit tests.
use std::cell::Cell;

use gpui::{Div, Stateful, prelude::*};

thread_local! {
    /// Set while a press inside a drag region landed on a control (Tauri's
    /// `data-tauri-drag-region="deep"` skips buttons and other interactive
    /// children). Reset by the region's capture listener for every press.
    static CONTROL_PRESSED: Cell<bool> = const { Cell::new(false) };
}

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

/// Inside a drag region a control keeps the press instead of moving the window.
pub(crate) fn control(el: Stateful<Div>) -> Stateful<Div> {
    el.on_mouse_down(gpui::MouseButton::Left, |_, _, _| CONTROL_PRESSED.with(|pressed| pressed.set(true)))
}
