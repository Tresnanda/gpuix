//! Post-paint observations. No animation frame request or polling loop.
use crate::{automation, retained_tree::RetainedTree};
use gpui::{IntoElement, Styled};
use napi::{
    threadsafe_function::{ThreadsafeFunction, ThreadsafeFunctionCallMode},
    Error, Result,
};
use serde_json::{json, Value};
use std::{cell::RefCell, collections::HashMap, rc::Rc};
#[derive(Default)]
pub(crate) struct Observers {
    next: u32,
    frame: u64,
    entries: HashMap<u32, Entry>,
}
struct Entry {
    target: Value,
    callback: ThreadsafeFunction<Value>,
    last: Option<Value>,
}
pub(crate) type Shared = Rc<RefCell<Observers>>;
impl Observers {
    pub(crate) fn add(&mut self, target: Value, callback: ThreadsafeFunction<Value>, tree: &RetainedTree) -> Result<u32> {
        if self.entries.len() >= 256 {
            return Err(Error::from_reason("At most 256 layout observers are supported"));
        }
        if target != Value::Null {
            if let Some(id) = target.as_u64() {
                if !tree.elements.contains_key(&id) {
                    return Err(Error::from_reason("Element is not mounted"));
                }
            } else if let Some(test) = target.get("testId").and_then(Value::as_str) {
                if tree.elements.values().filter(|e| e.test_id.as_deref() == Some(test)).count() != 1 {
                    return Err(Error::from_reason("testId must resolve uniquely"));
                }
            } else {
                return Err(Error::from_reason("Expected element ID or { testId }"));
            }
        }
        self.next = self.next.checked_add(1).ok_or_else(|| Error::from_reason("Observer tokens exhausted"))?;
        self.entries.insert(self.next, Entry { target, callback, last: None });
        Ok(self.next)
    }
    pub(crate) fn remove(&mut self, token: u32) {
        self.entries.remove(&token);
    }
    pub(crate) fn contains(&self, token: u32) -> bool {
        self.entries.contains_key(&token)
    }
    pub(crate) fn close(&mut self) {
        self.entries.clear();
    }
}
fn rect(b: automation::ElementBounds) -> Value {
    json!({"x":b.x,"y":b.y,"width":b.width,"height":b.height})
}
pub(crate) fn after_paint(observers: Shared, tree: std::sync::Arc<std::sync::Mutex<RetainedTree>>) -> impl IntoElement {
    gpui::canvas(|_,_,_| (), move |_,_,window,cx| {
  if observers.borrow().entries.is_empty() { return; }
  let observers = observers.clone(); let tree = tree.clone();
  let size = window.viewport_size(); let scale = window.scale_factor();
  // Deferred app work runs after all ordinary/deferred paint and cached replay.
  cx.defer(move |_| {
   let tree = tree.lock().unwrap(); let mut state = observers.borrow_mut(); state.frame += 1; let frame = state.frame;
   for entry in state.entries.values_mut() {
    let mut value = if entry.target.is_null() { json!({"width":f32::from(size.width),"height":f32::from(size.height),"scaleFactor":scale}) }
    else {
     let id = entry.target.as_u64().or_else(|| {
      let test = entry.target.get("testId")?.as_str()?;
      let mut matches = tree.elements.values().filter(|e| e.test_id.as_deref() == Some(test));
      let first = matches.next()?.id; if matches.next().is_some() { None } else { Some(first) }
     });
     let bounds = id.filter(|id| tree.elements.contains_key(id)).and_then(automation::get_bounds);
     json!({"elementId":id,"rect":bounds.clone().map(rect),"roundedClips":bounds.as_ref().and_then(|b| b.rounded_clips.as_deref()),"clippedRect":bounds.and_then(|b| b.clip).map(|b| json!({"x":b[0],"y":b[1],"width":b[2],"height":b[3]}))})
    };
    if entry.last.as_ref() == Some(&value) { continue; }
    entry.last = Some(value.clone()); value["frame"] = json!(frame);
    entry.callback.call(Ok(value), ThreadsafeFunctionCallMode::NonBlocking);
   }
  });
 }).absolute().w(gpui::px(0.)).h(gpui::px(0.))
}
