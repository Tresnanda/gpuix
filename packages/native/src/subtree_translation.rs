//! Display-driven subtree translation; MotionState supplies the existing curves.
use crate::{motion::MotionState, renderer::GpuixView, retained_tree::RetainedTree};
use gpui::{AnyElement, App, Bounds, Element, GlobalElementId, InspectorElementId, IntoElement, LayoutId, Pixels, Point, Window};
use std::{cell::RefCell, collections::HashMap};
thread_local! {
    static STATES: RefCell<HashMap<u64,MotionState>> = RefCell::default();
    static TIME: std::cell::Cell<Option<web_time::Instant>> = const {std::cell::Cell::new(None)};
    static SCHEDULED: std::cell::Cell<bool> = const {std::cell::Cell::new(false)};
}
pub(crate) fn forget(id: u64) {
    STATES.with(|states| {
        states.borrow_mut().remove(&id);
    });
}
pub(crate) fn valid(value: &serde_json::Value) -> bool {
    if value.is_null() {
        return true;
    }
    let Some(animate) = value.get("animate").and_then(|v| v.as_object()) else {
        return false;
    };
    if animate.keys().any(|k| k != "translateX" && k != "translateY") {
        return false;
    }
    MotionState::new(value, web_time::Instant::now()).is_ok()
}
pub(crate) fn sync(tree: &RetainedTree, now: web_time::Instant) {
    TIME.with(|time| time.set(Some(now)));
    STATES.with(|states| {
        let mut states = states.borrow_mut();
        states.retain(|id, _| tree.elements.get(id).is_some_and(|e| e.custom_props().contains_key("paintTranslate")));
        for (id, element) in &tree.elements {
            let Some(source) = element.custom_props().get("paintTranslate") else {
                continue;
            };
            match states.entry(*id) {
                std::collections::hash_map::Entry::Occupied(mut e) => {
                    let _ = e.get_mut().sync(source, now);
                }
                std::collections::hash_map::Entry::Vacant(e) => {
                    if valid(source) {
                        if let Ok(state) = MotionState::new(source, now) {
                            e.insert(state);
                        }
                    }
                }
            }
        }
    });
}
fn point(id: u64, now: web_time::Instant) -> Point<Pixels> {
    STATES.with(|states| {
        states
            .borrow()
            .get(&id)
            .and_then(|s| s.visible_style(now))
            .map(|s| gpui::point(gpui::px(s.translate_x.unwrap_or(0.) as f32), gpui::px(s.translate_y.unwrap_or(0.) as f32)))
            .unwrap_or_default()
    })
}
pub(crate) fn values() -> HashMap<u64, Point<Pixels>> {
    let now = TIME.with(|time| time.get().unwrap_or_else(web_time::Instant::now));
    STATES.with(|s| {
        s.borrow()
            .iter()
            .filter_map(|(&id, state)| {
                state
                    .visible_style(now)
                    .map(|v| (id, gpui::point(gpui::px(v.translate_x.unwrap_or(0.) as f32), gpui::px(v.translate_y.unwrap_or(0.) as f32))))
            })
            .collect()
    })
}
pub(crate) fn apply(child: AnyElement, id: u64, tree: &RetainedTree, now: web_time::Instant) -> AnyElement {
    if !tree.elements.get(&id).is_some_and(|e| e.custom_props().contains_key("paintTranslate")) {
        return child;
    }
    // A root build synchronizes once; a newly mounted virtual row can introduce
    // its scope later in prepaint, so synchronize that one source as well.
    let source = &tree.elements[&id].custom_props()["paintTranslate"];
    STATES.with(|states| {
        let mut states = states.borrow_mut();
        if !states.contains_key(&id) {
            if let Ok(s) = MotionState::new(source, now) {
                states.insert(id, s);
            }
        }
    });
    Translated { child, id, offset: point(id, now) }.into_any_element()
}
struct Translated {
    child: AnyElement,
    id: u64,
    offset: Point<Pixels>,
}
impl IntoElement for Translated {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}
impl Element for Translated {
    type RequestLayoutState = ();
    type PrepaintState = ();
    fn id(&self) -> Option<gpui::ElementId> {
        None
    }
    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }
    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        w: &mut Window,
        c: &mut App,
    ) -> (LayoutId, ()) {
        (self.child.request_layout(w, c), ())
    }
    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        w: &mut Window,
        c: &mut App,
    ) {
        w.with_subtree_translation(self.id, self.offset, |w| self.child.prepaint(w, c));
    }
    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        _: &mut (),
        w: &mut Window,
        c: &mut App,
    ) {
        w.with_subtree_translation(self.id, self.offset, |w| self.child.paint(w, c));
    }
}
pub(crate) fn prepare(view: &GpuixView) {
    sync(&view.tree.lock().unwrap(), view.clock.now());
}
pub(crate) fn arm(view: &GpuixView, window: &mut Window, cx: &mut gpui::Context<GpuixView>) {
    if STATES.with(|s| s.borrow().values().any(|state| state.is_active(view.clock.now()))) {
        schedule(view, window, cx);
    }
}
fn schedule(view: &GpuixView, window: &mut Window, cx: &mut gpui::Context<GpuixView>) {
    if SCHEDULED.with(|s| s.replace(true)) {
        return;
    }
    let entity = cx.weak_entity();
    window.on_next_frame(move |window, cx| {
        SCHEDULED.with(|s| s.set(false));
        let Some(entity) = entity.upgrade() else {
            return;
        };
        entity.update(cx, |view, cx| {
            tick(view, window, cx);
        });
    });
    let _ = view;
}
pub(crate) fn tick(view: &mut GpuixView, window: &mut Window, cx: &mut gpui::Context<GpuixView>) -> bool {
    let now = view.clock.now();
    TIME.with(|time| time.set(Some(now)));
    let (updates, active, settled) = STATES.with(|states| {
        let mut states = states.borrow_mut();
        let mut updates = Vec::new();
        let mut active = false;
        let mut settled = Vec::new();
        for (&id, state) in states.iter_mut() {
            if let Some(s) = state.visible_style(now) {
                updates.push((id, gpui::point(gpui::px(s.translate_x.unwrap_or(0.) as f32), gpui::px(s.translate_y.unwrap_or(0.) as f32))));
            }
            let frame = state.frame(now);
            active |= frame.active;
            if frame.just_settled {
                settled.push((id, frame.generation));
            }
        }
        (updates, active, settled)
    });
    let painted = updates.is_empty() || window.translate_subtrees(&updates);
    if painted {
        let deltas = crate::automation::translate_subtree_bounds(&updates, window.viewport_size());
        view.custom_registry.translate_subtrees(&deltas, cx);
        crate::layout_observer::notify(&view.layout_observers, &view.tree, window.viewport_size(), window.scale_factor());
        for (id, generation) in settled {
            if !view.tree.lock().unwrap().elements.get(&id).is_some_and(|e| e.events().contains("motionComplete")) {
                continue;
            }
            if let Some(callback) = &view.event_callback {
                let mut event = crate::element_tree::EventPayload::default();
                event.element_id = id as f64;
                event.event_type = "motionComplete".into();
                event.motion_generation = Some(generation as f64);
                callback(event);
            }
        }
    }
    if active {
        schedule(view, window, cx);
    }
    painted
}
pub(crate) fn invalidate(view: &mut GpuixView, window: &mut Window, cx: &mut gpui::Context<GpuixView>) -> bool {
    prepare(view);
    let tree = view.tree.lock().unwrap();
    let only = !tree.full_redraw && !tree.subtree_translation_updates.is_empty() && tree.paint_updates.is_empty();
    if std::env::var_os("KYBERN_DEBUG_ISLANDS").is_some() && !tree.subtree_translation_updates.is_empty() {
        eprintln!(
            "[subtree] updates={} full_redraw={} paint_updates={} retained={only}",
            tree.subtree_translation_updates.len(),
            tree.full_redraw,
            tree.paint_updates.len()
        );
    }
    drop(tree);
    if only && tick(view, window, cx) {
        view.tree.lock().unwrap().subtree_translation_updates.clear();
        return true;
    }
    schedule(view, window, cx);
    false
}
