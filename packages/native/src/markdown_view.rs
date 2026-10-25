//! Exact measured block geometry, independent of disposable frame elements.
//! Like Zed List, only intersecting children are instantiated for paint. Unlike
//! splitting source, the closures render the canonical AST in its original context.
use gpui::{
    prelude::*, px, AnyElement, App, AvailableSpace, Bounds, Element, ElementId, GlobalElementId, InspectorElementId, LayoutId, Pixels,
    Window,
};
use std::{cell::RefCell, collections::HashMap, rc::Rc};
pub(crate) type Cache = Rc<RefCell<CacheState>>;
#[derive(Default)]
pub(crate) struct CacheState {
    geometry: HashMap<usize, Rc<RefCell<Geometry>>>,
    data: HashMap<(usize, std::any::TypeId), (u64, Rc<dyn std::any::Any>)>,
    epoch: u64,
    scale: Option<f32>,
    selectable: bool,
    selection: std::collections::BTreeMap<usize, LogicalText>,
    pub measured: Option<Rc<dyn Fn(f32, f32, &[(f32, f32)])>>,
    pub publish: bool,
}
impl CacheState {
    pub(crate) fn prepare(&mut self, scale: f32, selectable: bool) {
        if self.scale != Some(scale) {
            self.clear();
            self.scale = Some(scale);
        }
        self.selectable = selectable;
    }

    pub(crate) fn invalidate_suffix(&mut self, blocks: usize, sub: usize) {
        self.geometry.retain(|key, _| *key < sub);
        if let Some(root) = self.geometry.get(&0) {
            let mut root = root.borrow_mut();
            root.positions.truncate(blocks);
            root.ready = false;
        }
        self.data.retain(|(key, _), _| *key != 0 && *key < sub);
        self.selection.retain(|key, _| *key < sub);
    }
    pub(crate) fn clear(&mut self) {
        self.geometry.clear();
        self.data.clear();
        self.measured = None;
        self.selection.clear();
    }
}
pub(crate) fn cached<T: 'static>(cache: &Cache, key: usize, build: impl FnOnce() -> T) -> Rc<T> {
    let id = (key, std::any::TypeId::of::<T>());
    let mut state = cache.borrow_mut();
    let epoch = state.epoch;
    if let Some((last, value)) = state.data.get_mut(&id) {
        *last = epoch;
        if let Ok(value) = value.clone().downcast::<T>() {
            return value;
        }
    }
    drop(state);
    let value = Rc::new(build());
    let mut state = cache.borrow_mut();
    let epoch = state.epoch;
    state.data.insert(id, (epoch, value.clone()));
    value
}

#[derive(Default)]
pub(crate) struct Geometry {
    width: f32,
    scale: f32,
    positions: Vec<(f32, f32)>,
    height: f32,
    ready: bool,
}
type Build = Rc<dyn Fn(usize, &Window) -> AnyElement>;
pub(crate) fn flow(cache: &Cache, key: usize, gaps: Rc<Vec<f32>>, build: Build) -> AnyElement {
    let geometry = cache.borrow_mut().geometry.entry(key).or_default().clone();
    Flow { geometry, gaps, build, owner: (key == 0).then(|| cache.clone()) }.into_any_element()
}
struct Flow {
    geometry: Rc<RefCell<Geometry>>,
    gaps: Rc<Vec<f32>>,
    build: Build,
    owner: Option<Cache>,
}
impl IntoElement for Flow {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}
impl Element for Flow {
    type RequestLayoutState = ();
    type PrepaintState = Vec<AnyElement>;
    fn id(&self) -> Option<ElementId> {
        None
    }
    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }
    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        _: &mut App,
    ) -> (LayoutId, ()) {
        let geometry = self.geometry.clone();
        let gaps = self.gaps.clone();
        let build = self.build.clone();
        let owner = self.owner.clone();
        let mut style = gpui::Style::default();
        style.size.width = gpui::relative(1.0).into();
        let id = window.request_measured_layout(style, move |known, available, window, cx| {
            let width = f32::from(known.width.unwrap_or(match available.width {
                AvailableSpace::Definite(w) => w,
                // Horizontal container layout asks for intrinsic size before
                // resolving its real width. Do not discard exact geometry for
                // that unconstrained probe on every wheel frame. A definite
                // width (including a real resize) still invalidates below.
                _ => px({ let geo = geometry.borrow(); if geo.ready { geo.width } else { 320.0 } }),
            }))
            .max(1.0);
            let scale = window.scale_factor();
            let stale = {
                let geo = geometry.borrow();
                !geo.ready || geo.width != width || geo.scale != scale || geo.positions.len() != gaps.len()
            };
            if stale {
                let mut positions = {
                    let geo = geometry.borrow();
                    if !geo.ready && geo.width == width && geo.scale == scale {
                        geo.positions.clone()
                    } else {
                        Vec::new()
                    }
                };
                positions.truncate(gaps.len());
                let mut height = positions.last().map_or(0.0, |(y, h)| y + h);
                let begin = positions.len();
                for (index, gap) in gaps.iter().enumerate().skip(begin) {
                    let size = window.measure_markdown(cx, |window, cx| {
                        let mut child = build(index, window);
                        child.layout_as_root(gpui::size(AvailableSpace::Definite(px(width)), AvailableSpace::MaxContent), window, cx)
                    });
                    height += f32::from(window.pixel_snap(px(*gap)));
                    positions.push((height, f32::from(size.height)));
                    height += f32::from(size.height);
                }
                *geometry.borrow_mut() = Geometry { width, scale, positions, height, ready: true };
            }
            if let Some(owner) = &owner {
                let state = owner.borrow();
                let publish = stale || state.publish;
                let measured = state.measured.clone();
                drop(state);
                if publish {
                    if let Some(measured) = measured {
                        measured(width, scale, &geometry.borrow().positions);
                    }
                    owner.borrow_mut().publish = false;
                }
            }
            gpui::size(px(width), px(geometry.borrow().height))
        });
        (id, ())
    }
    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) -> Vec<AnyElement> {
        if let Some(cache) = &self.owner {
            cache.borrow_mut().epoch += 1;
        }
        let clip = window.content_mask().bounds;
        let top = f32::from(clip.top() - bounds.top()) - 160.0;
        let bottom = f32::from(clip.bottom() - bounds.top()) + 160.0;
        let geo = self.geometry.borrow();
        let start = geo.positions.partition_point(|(y, h)| y + h < top);
        let end = geo.positions.partition_point(|(y, _)| *y <= bottom);
        let mut children = Vec::with_capacity(end.saturating_sub(start));
        for index in start..end {
            let (y, height) = geo.positions[index];
            let mut child = (self.build)(index, window);
            child.prepaint_as_root(
                bounds.origin + gpui::point(px(0.0), px(y)),
                gpui::size(AvailableSpace::Definite(bounds.size.width), AvailableSpace::Definite(px(height))),
                window,
                cx,
            );
            children.push(child);
        }
        children
    }
    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        children: &mut Vec<AnyElement>,
        window: &mut Window,
        cx: &mut App,
    ) {
        for child in children {
            child.paint(window, cx);
        }
        if let Some(cache) = &self.owner {
            let mut state = cache.borrow_mut();
            let epoch = state.epoch;
            state.data.retain(|_, (last, _)| last.saturating_add(1) >= epoch);
        }
    }
}

#[derive(Clone)]
struct LogicalText {
    key: String,
    text: gpui::SharedString,
    range: std::ops::Range<usize>,
    group: Option<u64>,
}
thread_local! {
    static DOCUMENTS: RefCell<HashMap<u64,Vec<std::rc::Weak<RefCell<CacheState>>>>>=RefCell::new(HashMap::new());
}
pub(crate) fn selection_document(id: u64, cache: &Cache) {
    DOCUMENTS.with(|documents| {
        let mut documents = documents.borrow_mut();
        let current = Rc::downgrade(cache);
        documents.retain(|document, caches| {
            caches.retain(|previous| previous.strong_count() > 0 && (*document == id || !previous.ptr_eq(&current)));
            !caches.is_empty()
        });
        let caches = documents.entry(id).or_default();
        if !caches.iter().any(|previous| previous.ptr_eq(&Rc::downgrade(cache))) {
            caches.push(Rc::downgrade(cache));
        }
    });
}
pub(crate) fn record(cache: &Cache, id: u64, sub: usize, text: gpui::SharedString, range: std::ops::Range<usize>, group: Option<u64>) {
    cache.borrow_mut().selection.insert(sub, LogicalText { key: format!("{id}:{sub}"), text, range, group });
}
/// Resolve against the whole logical document, even after a wheel jump skipped
/// every run between the anchor and head. Geometry/hit testing stay viewport-only.
pub(crate) fn resolve_drag(selection: &crate::text::SharedSelection, key: &str, offset: usize) -> Option<bool> {
    let id = key.split_once(':')?.0.parse::<u64>().ok()?;
    let caches = DOCUMENTS.with(|documents| documents.borrow().get(&id).cloned())?;
    let mut logical = std::collections::BTreeMap::new();
    for cache in caches.into_iter().filter_map(|cache| cache.upgrade()) {
        if !cache.borrow().selectable {
            continue;
        }
        logical.extend(cache.borrow().selection.iter().map(|(sub, text)| (*sub, text.clone())));
    }
    let mut state = selection.lock();
    let anchor = state.document_anchor()?;
    if !anchor.split_once(':').is_some_and(|(s, _)| s == id.to_string()) {
        return None;
    }
    let entries: Vec<_> = logical
        .values()
        .map(|entry| crate::text::selection::RegisteredText { key: &entry.key, text: &entry.text[entry.range.clone()], group: entry.group })
        .collect();
    if !entries.iter().any(|entry| entry.key == anchor) {
        return None;
    }
    let head = entries.iter().position(|entry| entry.key == key)?;
    Some(state.update_drag(&entries, (head, offset)))
}
