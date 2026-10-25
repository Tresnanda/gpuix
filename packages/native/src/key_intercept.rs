//! Opt-in asynchronous key cancellation without blocking the JS/main thread.
//! GPUI interceptors suppress bindings; the first root key listener retains the
//! original KeyDownEvent (including is_held), which the interceptor API omits.
use crate::renderer::{EventCallback, emit_event_full};
use gpui::{App, FocusHandle, KeyDownEvent, Window, prelude::*};
use std::{
    cell::{Cell, RefCell},
    collections::VecDeque,
};

struct Pending {
    token: u32,
    event: KeyDownEvent,
    focus: FocusHandle,
    answer: Option<bool>,
}
thread_local! {
    static TARGET: RefCell<Option<(u64, FocusHandle, bool)>> = const { RefCell::new(None) };
    static PENDING: RefCell<VecDeque<Pending>> = const { RefCell::new(VecDeque::new()) };
    static NEXT: Cell<u32> = const { Cell::new(0) };
    static REPLAY: Cell<bool> = const { Cell::new(false) };
}
pub(crate) fn replaying() -> bool {
    REPLAY.with(Cell::get)
}
pub(crate) fn pending() -> bool {
    PENDING.with(|pending| !pending.borrow().is_empty())
}
pub(crate) fn capture(id: u64, focus: FocusHandle, cancellable: bool, window: &mut Window, cx: &mut App) {
    if replaying() {
        return;
    }
    TARGET.with(|target| *target.borrow_mut() = Some((id, focus, cancellable)));
    window.prevent_default();
    cx.stop_propagation();
}
pub(crate) fn listener(callback: Option<EventCallback>) -> impl gpui::IntoElement {
    gpui::canvas(
        |_, _, _| (),
        move |_, _, window, _| {
            let callback = callback.clone();
            window.on_root_key_event(move |event: &KeyDownEvent, phase, window, cx| {
                if phase != gpui::DispatchPhase::Capture {
                    return;
                }
                let Some((id, focus, cancellable)) = TARGET.with(|target| target.borrow_mut().take()) else {
                    return;
                };
                let token = NEXT.with(|next| {
                    let token = next.get().wrapping_add(1);
                    next.set(token);
                    token
                });
                // Auto-repeat must remain bounded if JS is paused/unresponsive.
                PENDING.with(|pending| {
                    let mut pending = pending.borrow_mut();
                    if pending.len() == 256 {
                        pending.pop_front();
                    }
                    pending.push_back(Pending { token, event: event.clone(), focus, answer: if cancellable { None } else { Some(false) } });
                });
                emit_event_full(&callback, id, "keyDown", |payload| {
                    payload.request_id = cancellable.then_some(token);
                    payload.key = Some(event.keystroke.key.clone());
                    payload.key_char = event.keystroke.key_char.clone();
                    payload.modifiers = Some(event.keystroke.modifiers.into());
                    payload.is_held = Some(event.is_held);
                });
                window.prevent_default();
                cx.stop_propagation();
            });
        },
    )
    .absolute()
    .size_0()
}
pub(crate) fn resolve(token: u32, prevented: bool, window: &mut Window, cx: &mut App) {
    PENDING.with(|pending| {
        if let Some(item) = pending.borrow_mut().iter_mut().find(|item| item.token == token) {
            item.answer = Some(prevented);
        }
    });
    loop {
        let next = PENDING.with(|pending| {
            let mut pending = pending.borrow_mut();
            if pending.front().is_some_and(|item| item.answer.is_some()) { pending.pop_front() } else { None }
        });
        let Some(item) = next else {
            break;
        };
        if item.answer == Some(false) && item.focus.is_focused(window) {
            REPLAY.with(|flag| flag.set(true));
            if item.event.keystroke.key_char.is_some() {
                window.dispatch_keystroke(item.event.keystroke, cx);
            } else {
                window.dispatch_event(gpui::PlatformInput::KeyDown(item.event), cx);
            }
            REPLAY.with(|flag| flag.set(false));
        }
    }
}
