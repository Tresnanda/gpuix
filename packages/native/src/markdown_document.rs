//! Renderer-owned immutable markdown revisions, parsed on the napi worker pool.
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
    pub blocks: serde_json::Value,
    pub stable_prefix_blocks: u32,
    pub parsed_bytes: u32,
}
use crate::markdown::parser::Block;
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
#[derive(Clone)]
pub(crate) struct Layout {
    pub width: f32,
    pub scale: f32,
    pub start: usize,
    pub positions: Arc<Vec<(f32, f32)>>,
}
impl Layout {
    fn json(&self) -> Value {
        serde_json::json!({"width":self.width,"scale":self.scale,"startBlock":self.start,"blocks":self.positions.iter().enumerate().map(|(index,(y,height))|serde_json::json!({"index":self.start+index,"y":y,"height":height})).collect::<Vec<_>>()})
    }
}
pub struct Document {
    pub parsed: crate::markdown_model::Parsed,
    pub revision: u32,
    pub selection_id: u64,
    pub(crate) layout: Mutex<Option<Layout>>,
    pub highlights: HashMap<String, Arc<crate::syntax::HighlightedDocument>>,
}
impl std::ops::Deref for Document {
    type Target = crate::markdown_model::Parsed;
    fn deref(&self) -> &Self::Target {
        &self.parsed
    }
}

pub struct ParseTask {
    registry: Shared,
    id: String,
    revision: u32,
    source: Option<String>,

    previous: Option<Arc<Document>>,
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
            .ok_or_else(|| Error::from_reason("Unknown, released or stale markdown document"))
    }
    pub(crate) fn layout(&self, id: &str, revision: u32) -> Result<Value> {
        self.entries
            .get(id)
            .and_then(|slot| slot.ready.as_ref())
            .filter(|doc| doc.revision == revision)
            .map(|doc| doc.layout.lock().unwrap().as_ref().map_or(Value::Null, Layout::json))
            .ok_or_else(|| Error::from_reason("Unknown, released or stale markdown document"))
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
            return Err(Error::from_reason("Markdown source exceeds 2 MiB"));
        }
        let source = source.to_owned();
        let mut state = registry.lock().unwrap();
        if state.pending >= 8 {
            return Err(Error::from_reason("Markdown queue is full (8 jobs)"));
        }
        let (id, revision, old_bytes) = if let Some(id) = id {
            let old = state.entries.get(&id).ok_or_else(|| Error::from_reason("Unknown or released markdown document"))?;
            if options.get("expectedRevision").and_then(Value::as_u64) != Some(old.revision as u64) {
                return Err(Error::from_reason("Stale markdown revision"));
            }
            (id, old.revision.checked_add(1).ok_or_else(|| Error::from_reason("Markdown revision exhausted"))?, old.bytes)
        } else {
            if state.entries.len() >= 32 {
                return Err(Error::from_reason("At most 32 markdown documents"));
            }
            state.next += 1;
            (format!("markdown-{}", state.next), 1, 0)
        };
        if state.bytes - old_bytes + source.len() > MAX_TOTAL {
            return Err(Error::from_reason("Markdown documents exceed 16 MiB"));
        }
        state.bytes = state.bytes - old_bytes + source.len();
        state.pending += 1;
        // Existing mounted ranges own their old Arc until their revision changes.
        let ready = state.entries.get(&id).and_then(|slot| slot.ready.clone());
        state.entries.insert(id.clone(), Slot { revision, bytes: source.len(), ready: ready.clone() });
        drop(state);
        Ok(ParseTask { registry, id, revision, source: Some(source), previous: ready })
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
            return Err(Error::from_reason("Markdown parse cancelled"));
        }
        let source = self.source.take().unwrap();
        static SELECTION_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1u64 << 51);
        let selection_id =
            self.previous.as_ref().map_or_else(|| SELECTION_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed), |d| d.selection_id);
        let parsed = crate::markdown_model::parse(source, self.previous.as_ref().map(|d| &d.parsed));
        let mut highlights = self
            .previous
            .as_ref()
            .map(|old| {
                old.highlights
                    .iter()
                    .filter(|(path, _)| {
                        path.split('/').next().and_then(|s| s.parse::<usize>().ok()).is_some_and(|index| index < parsed.stable_prefix)
                    })
                    .map(|(path, doc)| (path.clone(), doc.clone()))
                    .collect()
            })
            .unwrap_or_default();
        for (index, block) in parsed.blocks.iter().enumerate().skip(parsed.stable_prefix) {
            prepare_highlights(block, index.to_string(), &mut highlights);
        }
        Ok(Arc::new(Document { highlights, parsed, revision: self.revision, selection_id, layout: Mutex::new(None) }))
    }

    fn resolve(&mut self, _: Env, document: Self::Output) -> Result<Handle> {
        let mut state = self.registry.lock().unwrap();
        let slot = state
            .entries
            .get_mut(&self.id)
            .filter(|s| s.revision == self.revision)
            .ok_or_else(|| Error::from_reason("Markdown parse cancelled"))?;
        let blocks = document.blocks.iter().enumerate().map(|(index, block)| {
            let range = &document.ranges[index];
            serde_json::json!({"id": format!("{}:{}:{}", self.id, range.start,index), "index":index, "startByte":range.start, "endByte":range.end,
                "kind": match block.as_ref() { Block::Paragraph{..}=>"paragraph", Block::Heading{..}=>"heading", Block::CodeBlock{..}=>"code", Block::List{..}=>"list", Block::BlockQuote{..}=>"quote", Block::Table{..}=>"table", Block::Rule=>"rule" }})
        }).collect::<Vec<_>>();
        let stable_prefix_blocks = document.stable_prefix as u32;
        let parsed_bytes = document.parsed_bytes as u32;
        slot.ready = Some(document);
        Ok(Handle {
            id: self.id.clone(),
            revision: self.revision,
            blocks: serde_json::Value::Array(blocks),
            stable_prefix_blocks,
            parsed_bytes,
        })
    }
}

fn prepare_highlights(block: &Block, path: String, output: &mut HashMap<String, Arc<crate::syntax::HighlightedDocument>>) {
    match block {
        Block::CodeBlock { code, language } => {
            let document = if code.len() <= crate::syntax::DEFAULT_MAX_SOURCE_BYTES {
                crate::syntax::cache::highlight_cached(code, None, language.as_deref())
            } else {
                crate::syntax::highlight_with_limits(
                    crate::syntax::HighlightRequest { source: code, path: None, fence_tag: language.as_deref() },
                    crate::syntax::HighlightLimits { max_source_bytes: MAX_SOURCE, max_spans: 200_000 },
                )
                .ok()
                .map(Arc::new)
            };
            if let Some(document) = document {
                output.insert(path, document);
            }
        }
        Block::BlockQuote { children } => {
            for (index, child) in children.iter().enumerate() {
                prepare_highlights(child, format!("{path}/0/{index}"), output);
            }
        }
        Block::List { items, .. } => {
            for (item, children) in items.iter().enumerate() {
                for (index, child) in children.iter().enumerate() {
                    prepare_highlights(child, format!("{path}/{item}/{index}"), output);
                }
            }
        }
        _ => {}
    }
}
