//! Vsync subscriptions with one queued JS delivery per subscriber.
use std::{cell::RefCell, collections::HashMap, rc::{Rc, Weak}, time::Instant};
use napi::{threadsafe_function::{ThreadsafeFunction, ThreadsafeFunctionCallMode}, Error, Result};
use serde_json::{json, Value};
#[derive(Default)]
pub(crate) struct Clock { origin: Option<Instant>, next: u32, scheduled: bool, entries: HashMap<u32, Entry> }
struct Entry { callback: ThreadsafeFunction<Value>, pending: bool }
pub(crate) type Shared = Rc<RefCell<Clock>>;
pub(crate) fn add(clock: Shared, callback: ThreadsafeFunction<Value>, window: &gpui::Window) -> Result<u32> {
    let mut state=clock.borrow_mut();
    if state.entries.len()>=64 { return Err(Error::from_reason("At most 64 frame subscribers")); }
    state.next=state.next.checked_add(1).ok_or_else(|| Error::from_reason("Frame tokens exhausted"))?;
    let token=state.next; state.entries.insert(token,Entry{callback,pending:false});
    if !state.scheduled { state.scheduled=true; drop(state); schedule(Rc::downgrade(&clock),window); }
    Ok(token)
}
pub(crate) fn ack(clock: &Shared, token:u32) { if let Some(entry)=clock.borrow_mut().entries.get_mut(&token){entry.pending=false;} }
pub(crate) fn remove(clock: &Shared, token:u32) { clock.borrow_mut().entries.remove(&token); }
fn schedule(clock: Weak<RefCell<Clock>>, window: &gpui::Window) {
    window.on_next_frame(move |window,_| {
        let Some(shared)=clock.upgrade() else {return;};
        let mut state=shared.borrow_mut();
        if state.entries.is_empty(){state.scheduled=false;return;}
        let now=Instant::now(); let origin=*state.origin.get_or_insert(now);
        let time=now.duration_since(origin).as_secs_f64()*1000.0;
        for entry in state.entries.values_mut(){
            if !entry.pending {entry.pending=true;entry.callback.call(Ok(json!({"timeMs":time})),ThreadsafeFunctionCallMode::NonBlocking);}
        }
        drop(state);schedule(clock,window);
    });
}

