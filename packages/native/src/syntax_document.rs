//! Renderer-owned immutable syntax revisions, parsed on the napi worker pool.
use napi::{Env, Error, Result, Task};
use std::{
    cell::RefCell,
    collections::HashMap,
    sync::{Arc, Mutex},
};
#[napi_derive::napi(object)]
pub struct Handle {
    pub id: String,
    pub revision: u32,
    pub line_count: u32,
}
use crate::syntax::{HighlightRequest, HighlightedDocument};
use serde_json::Value;
const MAX_SOURCE: usize = 2 * 1024 * 1024;
const MAX_TOTAL: usize = 16 * 1024 * 1024;
#[derive(Default)]
pub(crate) struct Registry {
    next: u64,
    pending: usize,
    bytes: usize,
    entries: HashMap<String, Slot>,
}
struct Slot {
    revision: u32,
    bytes: usize,
    language: Option<String>,
    path: Option<String>,
    ready: Option<Arc<Document>>,
}
pub(crate) type Shared = Arc<Mutex<Registry>>;
thread_local! { static CURRENT: RefCell<Option<std::sync::Weak<Mutex<Registry>>>> = const { RefCell::new(None) }; }
pub(crate) fn activate(registry: &Shared) {
    CURRENT.with(|r| *r.borrow_mut() = Some(Arc::downgrade(registry)));
}
pub(crate) fn lookup(id: &str, revision: u32) -> Option<Arc<Document>> {
    CURRENT
        .with(|r| r.borrow().as_ref()?.upgrade()?.lock().ok()?.entries.get(id)?.ready.as_ref().filter(|d| d.revision == revision).cloned())
}
pub struct Document {
    pub source: String,
    pub offsets: Vec<usize>,
    pub highlight: Option<Arc<HighlightedDocument>>,
    pub revision: u32,
    pub selection_id: u64,
}
impl Document {
    pub fn line(&self, index: usize) -> &str {
        let start = self.offsets[index];
        let end = self.offsets.get(index + 1).map(|n| n - 1).unwrap_or(self.source.len());
        &self.source[start..end]
    }
}
pub struct ParseTask {
    registry: Shared,
    id: String,
    revision: u32,
    source: Option<String>,
    language: Option<String>,
    path: Option<String>,
    selection_id: u64,
}
impl Drop for ParseTask {
    fn drop(&mut self) {
        self.registry.lock().unwrap().pending -= 1;
    }
}
impl Registry {
    pub(crate) fn source(&self, id: &str, revision: u32) -> Result<String> {
        self.entries
            .get(id)
            .and_then(|slot| slot.ready.as_ref())
            .filter(|doc| doc.revision == revision)
            .map(|doc| doc.source.clone())
            .ok_or_else(|| Error::from_reason("Unknown, released or stale syntax document"))
    }
    pub(crate) fn stats(&self) -> Value {
        serde_json::json!({"documents":self.entries.len(),"sourceBytes":self.bytes,"pending":self.pending})
    }
    pub(crate) fn close(&mut self) {
        self.entries.clear();
        self.bytes = 0;
    }
    pub(crate) fn prepare(registry: Shared, id: Option<String>, options: Value) -> Result<ParseTask> {
        let source = options.get("text").and_then(Value::as_str).ok_or_else(|| Error::from_reason("text is required"))?;
        if source.len() > MAX_SOURCE {
            return Err(Error::from_reason("Syntax source exceeds 2 MiB"));
        }
        let source = source.to_owned();
        let mut state = registry.lock().unwrap();
        if state.pending >= 8 {
            return Err(Error::from_reason("Syntax queue is full (8 jobs)"));
        }
        let (id, revision, language, path, old_bytes) = if let Some(id) = id {
            let old = state.entries.get(&id).ok_or_else(|| Error::from_reason("Unknown or released syntax document"))?;
            if options.get("expectedRevision").and_then(Value::as_u64) != Some(old.revision as u64) {
                return Err(Error::from_reason("Stale syntax revision"));
            }
            (
                id,
                old.revision.checked_add(1).ok_or_else(|| Error::from_reason("Syntax revision exhausted"))?,
                old.language.clone(),
                old.path.clone(),
                old.bytes,
            )
        } else {
            if state.entries.len() >= 32 {
                return Err(Error::from_reason("At most 32 syntax documents"));
            }
            state.next += 1;
            (
                format!("syntax-{}", state.next),
                1,
                options.get("language").and_then(Value::as_str).map(str::to_owned),
                options.get("path").and_then(Value::as_str).map(str::to_owned),
                0,
            )
        };
        if state.bytes - old_bytes + source.len() > MAX_TOTAL {
            return Err(Error::from_reason("Syntax documents exceed 16 MiB"));
        }
        state.bytes = state.bytes - old_bytes + source.len();
        state.pending += 1;
        // Existing mounted ranges own their old Arc until their revision changes.
        let ready = state.entries.get(&id).and_then(|slot| slot.ready.clone());
        state.entries.insert(id.clone(), Slot { revision, bytes: source.len(), language: language.clone(), path: path.clone(), ready });
        static SELECTION_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1u64 << 52);
        let selection_id = SELECTION_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        drop(state);
        Ok(ParseTask { registry, id, revision, source: Some(source), language, path, selection_id })
    }
    pub(crate) fn release(&mut self, id: &str) {
        if let Some(slot) = self.entries.remove(id) {
            self.bytes -= slot.bytes;
        }
    }
}
impl Task for ParseTask {
    type Output = Arc<Document>;
    type JsValue = Handle;
    fn compute(&mut self) -> Result<Self::Output> {
        if self.registry.lock().unwrap().entries.get(&self.id).map(|s| s.revision) != Some(self.revision) {
            return Err(Error::from_reason("Syntax parse cancelled"));
        }
        let source = self.source.take().unwrap();
        let mut offsets = vec![0];
        offsets.extend(source.match_indices('\n').map(|(i, _)| i + 1));
        let highlight = crate::syntax::highlight_with_limits(
            HighlightRequest { source: &source, path: self.path.as_deref(), fence_tag: self.language.as_deref() },
            crate::syntax::HighlightLimits { max_source_bytes: MAX_SOURCE, max_spans: 200_000 },
        )
        .ok()
        .map(Arc::new);
        Ok(Arc::new(Document { source, offsets, highlight, revision: self.revision, selection_id: self.selection_id }))
    }
    fn resolve(&mut self, _: Env, document: Self::Output) -> Result<Handle> {
        let mut state = self.registry.lock().unwrap();
        let slot = state
            .entries
            .get_mut(&self.id)
            .filter(|s| s.revision == self.revision)
            .ok_or_else(|| Error::from_reason("Syntax parse cancelled"))?;
        let count = document.offsets.len();
        slot.ready = Some(document);
        Ok(Handle { id: self.id.clone(), revision: self.revision, line_count: count as u32 })
    }
}
