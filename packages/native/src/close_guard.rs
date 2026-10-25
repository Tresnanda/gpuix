//! A deferred close decision shared by AppKit's close button and the Window menu.
use crate::renderer::{EventCallback, emit_event_full};
use std::cell::RefCell;
#[derive(Default)]
struct Guard {
    callback: Option<EventCallback>,
    pending: Option<u32>,
    generation: u32,
}
thread_local! { static GUARD: RefCell<Guard> = RefCell::new(Guard::default()); }

pub(crate) fn install(callback: Option<EventCallback>, window: &mut gpui::Window, cx: &mut gpui::App) {
    GUARD.with(|guard| {
        let mut guard = guard.borrow_mut();
        guard.callback = callback;
        guard.pending = None;
    });
    window.on_window_should_close(cx, |_, _| should_close());
}
fn should_close() -> bool {
    let event = GUARD.with(|guard| {
        let mut guard = guard.borrow_mut();
        let Some(callback) = guard.callback.clone() else {
            return Ok(());
        };
        if guard.pending.is_some() {
            return Err(None);
        }
        guard.generation = guard.generation.wrapping_add(1);
        let token = guard.generation;
        guard.pending = Some(token);
        Err(Some((callback, token)))
    });
    match event {
        Ok(()) => true,
        Err(None) => false,
        Err(Some((callback, token))) => {
            emit_event_full(&Some(callback), 0, "shouldClose", |p| p.request_id = Some(token));
            false
        }
    }
}
pub(crate) fn request(window: &mut gpui::Window) {
    if should_close() {
        window.remove_window();
    }
}
pub(crate) fn resolve(token: u32, allow: bool, window: &mut gpui::Window) {
    let current = GUARD.with(|guard| {
        let mut guard = guard.borrow_mut();
        if guard.pending != Some(token) {
            return false;
        }
        guard.pending = None;
        true
    });
    if current && allow {
        window.remove_window();
    }
}
