//! `onFileDragChange` (render9): a Finder drag (GPUI external paths) entering or leaving an element,
//! so React can ease a drop highlight the way the desktop's `dragover`/`dragleave` do. Enter and leave
//! come from drag moves over the element's bounds; a drop on it, the drag leaving the window or a
//! cancelled drag (GPUI refreshes the window for each) end it.
use crate::renderer::{EventCallback, emit_event_full, point_to_xy};
use gpui::InteractiveElement;
use std::{cell::RefCell, collections::HashMap};

thread_local! {
    /// Elements a file drag is over, with the callback that reports their leave.
    static OVER: RefCell<HashMap<u64, Option<EventCallback>>> = RefCell::default();
}

fn change(callback: &Option<EventCallback>, id: u64, over: bool, position: gpui::Point<gpui::Pixels>) {
    let was = OVER.with(|o| if over { o.borrow_mut().insert(id, callback.clone()).is_some() } else { o.borrow_mut().remove(&id).is_some() });
    if was == over {
        return;
    }
    emit_event_full(callback, id, "fileDragChange", |p| {
        let (x, y) = point_to_xy(position);
        p.x = Some(x);
        p.y = Some(y);
        p.hovered = Some(over);
    });
}

pub(crate) fn attach(el: gpui::Stateful<gpui::Div>, id: u64, callback: Option<EventCallback>) -> gpui::Stateful<gpui::Div> {
    use gpui::{ParentElement, Styled};
    let on_end = callback.clone();
    el.on_drag_move::<gpui::ExternalPaths>(move |event, _window, _cx| {
        let position = event.event.position;
        change(&callback, id, event.bounds.contains(&position), position);
    })
    // The drop (a mouse up while the paths are dragged; GPUI allows one drop listener per type, so this is not an
    // `on_drop`), the drag leaving the window, or its end: GPUI dispatches all of these as mouse events.
    .child(
        gpui::canvas(|_, _, _| (), move |_, _, window, _| {
            let on_drop = on_end.clone();
            window.on_mouse_event(move |_: &gpui::MouseUpEvent, phase, window, cx| {
                if phase == gpui::DispatchPhase::Capture && cx.has_active_drag() {
                    change(&on_drop, id, false, window.mouse_position());
                }
            });
            let on_end = on_end.clone();
            window.on_mouse_event(move |event: &gpui::FileDropEvent, _, window, _| {
                if matches!(event, gpui::FileDropEvent::Exited | gpui::FileDropEvent::Ended) {
                    change(&on_end, id, false, window.mouse_position());
                }
            });
        })
        .absolute()
        .size_0(),
    )
}

/// At each render: a drag that ended or left the window leaves every element it was over.
pub(crate) fn sweep(window: &gpui::Window, cx: &gpui::App) {
    if cx.has_active_drag() || OVER.with(|o| o.borrow().is_empty()) {
        return;
    }
    let over: Vec<_> = OVER.with(|o| o.borrow().iter().map(|(id, cb)| (*id, cb.clone())).collect());
    for (id, callback) in over {
        change(&callback, id, false, window.mouse_position());
    }
}

pub(crate) fn forget(id: u64) {
    OVER.with(|o| {
        o.borrow_mut().remove(&id);
    });
}
