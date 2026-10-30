//! Vsync subscriptions with one queued JS delivery per subscriber.
use std::{cell::RefCell, collections::HashMap, rc::{Rc, Weak}, time::{Duration, Instant}};
use napi::{threadsafe_function::{ThreadsafeFunction, ThreadsafeFunctionCallMode}, Error, Result, Status};
use serde_json::{json, Value};
#[derive(Default)]
pub(crate) struct Clock { origin: Option<Instant>, next: u32, scheduled: bool, entries: HashMap<u32, Entry> }
// `missed`: a frame passed while the delivery was still pending. Off macOS the
// acknowledge is queued to the UI thread and can land after the next frame;
// it then delivers at once instead of waiting a whole frame.
struct Entry { callback: ThreadsafeFunction<Value>, pending: bool, pending_since: Instant, #[cfg_attr(target_os = "macos", allow(dead_code))] missed: bool }
pub(crate) type Shared = Rc<RefCell<Clock>>;
pub(crate) fn add(clock: Shared, callback: ThreadsafeFunction<Value>, window: &gpui::Window) -> Result<u32> {
    let mut state=clock.borrow_mut();
    if state.entries.len()>=64 { return Err(Error::from_reason("At most 64 frame subscribers")); }
    state.next=state.next.checked_add(1).ok_or_else(|| Error::from_reason("Frame tokens exhausted"))?;
    let token=state.next; state.entries.insert(token,Entry{callback,pending:false,pending_since:Instant::now(),missed:false});
    if !state.scheduled { state.scheduled=true; drop(state); schedule(Rc::downgrade(&clock),window); }
    Ok(token)
}
pub(crate) fn ack(clock: &Shared, token:u32) {
    let mut state=clock.borrow_mut();
    let time=state.origin.map(|origin|Instant::now().duration_since(origin).as_secs_f64()*1000.0);
    let Some(entry)=state.entries.get_mut(&token) else {return;};
    entry.pending=false;
    #[cfg(not(target_os = "macos"))]
    if std::mem::take(&mut entry.missed) { if let Some(time)=time { deliver(entry,time); } }
    #[cfg(target_os = "macos")]
    let _=time;
}
fn deliver(entry:&mut Entry, time:f64) {
    entry.pending_since=Instant::now();
    entry.pending=entry.callback.call(Ok(json!({"timeMs":time})),ThreadsafeFunctionCallMode::NonBlocking)==Status::Ok;
}
pub(crate) fn remove(clock: &Shared, token:u32) { clock.borrow_mut().entries.remove(&token); }
fn schedule(clock: Weak<RefCell<Clock>>, window: &gpui::Window) {
    window.on_next_frame(move |window,_| {
        let Some(shared)=clock.upgrade() else {return;};
        let mut state=shared.borrow_mut();
        if state.entries.is_empty(){state.scheduled=false;return;}
        let now=Instant::now(); let origin=*state.origin.get_or_insert(now);
        let time=now.duration_since(origin).as_secs_f64()*1000.0;
        for entry in state.entries.values_mut(){
            // A lost delivery or acknowledge must not silence a subscriber forever.
            if !entry.pending || now.duration_since(entry.pending_since)>=Duration::from_secs(1) {deliver(entry,time);}
            else if cfg!(not(target_os = "macos")) {entry.missed=true;}
        }
        drop(state);schedule(clock,window);
    });
}

