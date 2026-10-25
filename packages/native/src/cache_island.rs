//! Cached islands (Kybern patch on GPUIX).
//!
//! An element with the custom prop `cache` (`<div cache style={{ width, height }}>`)
//! becomes its own GPUI view rendered through `AnyView::cached`. On a frame where
//! nothing inside it changed, GPUI replays the island's previous prepaint and
//! paint instead of rebuilding and laying it out again. A window whose heavy
//! parts are islands then pays only for the island that changed: a 16-dot loader
//! ticking next to a sidebar rebuilds 16 dots, not the sidebar.
//!
//! Rules (the island is laid out from its own size, never from its content):
//! - It needs a definite `width` and `height` (pixels or percentages). Without
//!   both, it is built inline like any other element. `cache="fill"` instead
//!   trusts the parent to size it (flex-grow, stretch): its box takes the
//!   element's layout style as-is.
//! - Virtual rows establish full width before measuring percentage islands.
//! - Margins and position insets do not belong on the island itself; put them
//!   on a parent.
//! - Ordinary selectable text registries replay with the cached scene.
//!
//! Invalidation: every root render compares the island's `subtree_revision`
//! (bumped by the retained tree on any change below it) with the revision it
//! last rendered, and re-renders islands that changed, that hold a running
//! `motion.div`, or whose motion just settled. GPUI marks such a view dirty in
//! the same frame, before the island prepaints.
//!
//! Automation: bounds are recorded during paint, which a reused island skips. An
//! island snapshots the bounds of its own elements when it really paints, and a
//! zero-size canvas after it re-inserts that snapshot on reused frames (a reused
//! island has, by construction, the same bounds).

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use gpui::prelude::*;
use gpui::{AnyElement, Entity, WeakEntity};

use crate::automation::{get_bounds, insert_bounds, ElementBounds};
use crate::renderer::{build_element, BuildCtx, GpuixView, Inherited};
use crate::retained_tree::{RetainedElement, RetainedTree};
use crate::style::{DimensionValue, StyleDesc};
use crate::text::paint::{SelectionSnapshot, selection_cursor, selection_snapshot, replay_selection};

fn debug() -> bool {
    std::env::var_os("KYBERN_DEBUG_ISLANDS").is_some()
}

// Diagnostic A/B switch; default remains cached. Read once, no per-frame env work.
fn inline_list_islands() -> bool {
    static INLINE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *INLINE.get_or_init(|| std::env::var_os("KYBERN_INLINE_LIST_ISLANDS").is_some())
}

/// The custom prop that marks an island.
pub(crate) const CACHE_PROP: &str = "cache";

type Snapshot = Rc<RefCell<Vec<(u64, ElementBounds)>>>;

pub(crate) struct IslandView {
    node_id: u64,
    gpuix: WeakEntity<GpuixView>,
    inherited: Rc<RefCell<Option<Inherited>>>,
    /// The subtree revision this view last rendered.
    rendered: Rc<Cell<u64>>,
    snapshot: Snapshot,
    painted: Rc<Cell<bool>>,
}

struct IslandEntry {
    view: Entity<IslandView>,
    /// Shared with the view, so the root render never touches the entity.
    inherited: Rc<RefCell<Option<Inherited>>>,
    rendered_revision: Rc<Cell<u64>>,
    motion_was_active: bool,
    snapshot: Snapshot,
    painted: Rc<Cell<bool>>,
    selection_snapshot: Rc<RefCell<SelectionSnapshot>>,
}

/// Live islands, keyed by retained element id. Owned by `GpuixView`.
#[derive(Default)]
pub(crate) struct Islands {
    entries: HashMap<u64, IslandEntry>,
    /// Islands seen by the root render that have no view yet. Their views are
    /// created after the draw (see `island_element`).
    pending: Vec<u64>,
    create_scheduled: bool,
}

impl Islands {
    /// Drop islands whose element is gone or no longer declares `cache`.
    pub(crate) fn prune(&mut self, tree: &RetainedTree) {
        self.entries
            .retain(|id, _| tree.elements.get(id).is_some_and(is_island));
    }
}

fn fills_parent(element: &RetainedElement) -> bool {
    element
        .custom_props()
        .get(CACHE_PROP)
        .is_some_and(|value| value.as_str() == Some("fill"))
}

pub(crate) fn is_island(element: &RetainedElement) -> bool {
    element
        .custom_props()
        .get(CACHE_PROP)
        .is_some_and(|value| value.as_bool().unwrap_or(!value.is_null()))
}

fn definite(value: &Option<DimensionValue>) -> bool {
    matches!(
        value,
        Some(DimensionValue::Pixels(_)) | Some(DimensionValue::Percentage(_))
    )
}

/// The island's outer box: its size and flex participation, nothing it paints.
fn outer_style(style: &StyleDesc, fill: bool) -> Option<StyleDesc> {
    if !fill && (!definite(&style.width) || !definite(&style.height)) {
        return None;
    }
    Some(StyleDesc {
        width: style.width.clone(),
        height: style.height.clone(),
        min_width: style.min_width.clone(),
        min_height: style.min_height.clone(),
        max_width: style.max_width.clone(),
        max_height: style.max_height.clone(),
        flex_grow: style.flex_grow,
        flex_shrink: style.flex_shrink,
        flex_basis: style.flex_basis,
        align_self: style.align_self.clone(),
        ..Default::default()
    })
}

fn is_descendant(tree: &RetainedTree, mut id: u64, ancestor: u64) -> bool {
    loop {
        if id == ancestor {
            return true;
        }
        match tree.elements.get(&id).and_then(|element| element.parent) {
            Some(parent) => id = parent,
            None => return false,
        }
    }
}

fn subtree_ids(tree: &RetainedTree, root: u64, out: &mut Vec<u64>) {
    out.push(root);
    if let Some(element) = tree.elements.get(&root) {
        for &child in &element.children {
            subtree_ids(tree, child, out);
        }
    }
}

/// Called from `build_element` for an element with `cache`. Returns `None` when
/// the element cannot be an island (no definite size, or it is the island being
/// built), so the caller builds it inline.
pub(crate) fn island_element(
    id: u64,
    element: &RetainedElement,
    ctx: &mut BuildCtx,
    _window: &mut gpui::Window,
    cx: &mut gpui::Context<GpuixView>,
) -> Option<AnyElement> {
    if ctx.island_root == Some(id) || (ctx.in_virtual_row && inline_list_islands()) {
        return None;
    }
    let fill = fills_parent(element);
    let outer = match element.style.as_deref() {
        Some(style) => outer_style(style, fill)?,
        None if fill => StyleDesc::default(),
        None => return None,
    };

    let motion_active = ctx.tree.motion_ids.iter().any(|&motion_id| {
        ctx.motion_states
            .get(&motion_id)
            .is_some_and(|state| state.is_active(ctx.now))
            && is_descendant(ctx.tree, motion_id, id)
    });

    // A new island renders inline this frame; its view is created after the
    // draw. Creating (or updating) the entity during the draw would count as an
    // access before its own first render, and GPUI would then never track it
    // for invalidation (wakes would be dropped and it would replay stale paint).
    let Some(entry) = ctx.islands.entries.get_mut(&id) else {
        if !ctx.islands.pending.contains(&id) {
            ctx.islands.pending.push(id);
        }
        if !ctx.islands.create_scheduled {
            ctx.islands.create_scheduled = true;
            cx.defer_in(_window, |view, _window, cx| {
                create_pending_islands(view, cx);
                cx.notify();
            });
        }
        return None;
    };
    let inherited = ctx.inherited.clone();
    // Never `update` the island entity here: GPUI tracks a cached view for
    // invalidation by the entities first accessed during its own render, so an
    // access from this (earlier) render would hide it, and later wakes would be
    // dropped. Share data through `inherited` and wake by entity id.
    *entry.inherited.borrow_mut() = Some(inherited);
    let changed = entry.rendered_revision.get() != element.subtree_revision;
    if debug() && changed {
        eprintln!("[islands] root render sees {id} changed ({} → {})", entry.rendered_revision.get(), element.subtree_revision);
    }
    if motion_active || entry.motion_was_active {
        // Marked during the draw, so it counts from the next frame: request one.
        gpui::App::notify(cx, entry.view.entity_id());
        _window.request_animation_frame();
    }
    entry.motion_was_active = motion_active;

    let replay = entry.snapshot.clone();
    let painted = entry.painted.clone();
    let selection = entry.selection_snapshot.clone();
    let cursor = Rc::new(Cell::new(Default::default()));
    let before = cursor.clone();
    // The view fills its box through flexbox, not `size_full()`: a percentage of
    // a stretched or flex-grown parent does not resolve in Taffy, and the island
    // would collapse to nothing.
    let cached = entry
        .view
        .clone()
        .cached(gpui::StyleRefinement::default().flex_grow_1().min_h_0());
    let wrapper = crate::renderer::apply_styles(gpui::div(), &outer)
        .flex()
        .flex_col()
        .relative()
        .child(gpui::canvas(|_, _, _| (), move |_, _, _, _| before.set(selection_cursor())).absolute().size_0())
        .child(cached)
        .child(gpui::canvas(|_, _, _| (), move |_, _, _, _| {
            if painted.replace(false) {
                *selection.borrow_mut() = selection_snapshot(cursor.get());
            } else {
                replay_selection(&selection.borrow());
            }
        }).absolute().size_0())
        .child(
            gpui::canvas(
                |_, _, _| (),
                move |_, _, _, _| {
                    for (id, bounds) in replay.borrow().iter() {
                        // A changed island already painted fresh ordinary bounds.
                        // Its deferred snapshot runs later, so replay must only
                        // fill gaps; overwriting here freezes the old coordinates
                        // into that next snapshot after layout/translation changes.
                        if get_bounds(*id).is_none() {
                            insert_bounds(*id, bounds.clone());
                        }
                    }
                },
            )
            .absolute()
            .size_0(),
        );
    Some(wrapper.into_any_element())
}

/// Create views for islands the root render met for the first time.
fn create_pending_islands(view: &mut GpuixView, cx: &mut gpui::Context<GpuixView>) {
    let pending = std::mem::take(&mut view.islands.pending);
    view.islands.create_scheduled = false;
    let tree_arc = view.tree.clone();
    let tree = tree_arc.lock().unwrap();
    for id in pending {
        if view.islands.entries.contains_key(&id) || !tree.elements.get(&id).is_some_and(is_island) {
            continue;
        }
        let snapshot: Snapshot = Rc::default();
        let inherited: Rc<RefCell<Option<Inherited>>> = Rc::default();
        // Never matches a real revision until the view renders.
        let rendered = Rc::new(Cell::new(u64::MAX));
        let gpuix = cx.weak_entity();
        let painted = Rc::new(Cell::new(false));
        let island = cx.new(|_| IslandView {
            node_id: id,
            gpuix,
            inherited: inherited.clone(),
            rendered: rendered.clone(),
            snapshot: snapshot.clone(),
            painted: painted.clone(),
        });
        view.islands.entries.insert(
            id,
            IslandEntry {
                view: island,
                inherited,
                rendered_revision: rendered,
                motion_was_active: false,
                snapshot,
                painted,
                selection_snapshot: Default::default(),
            },
        );
    }
}

/// Wake every island whose subtree changed since it last rendered. Runs when a
/// batch is applied, outside any draw, so the islands re-render in the next frame.
pub(crate) fn notify_changed_islands(view: &mut GpuixView, cx: &mut gpui::Context<GpuixView>) {
    let tree_arc = view.tree.clone();
    let tree = tree_arc.lock().unwrap();
    for (id, entry) in &view.islands.entries {
        let changed = tree
            .elements
            .get(id)
            .is_some_and(|element| element.subtree_revision != entry.rendered_revision.get());
        if changed {
            if debug() {
                eprintln!("[islands] pre-draw wake {id} (rendered {} → now {})", entry.rendered_revision.get(), tree.elements.get(id).map(|e| e.subtree_revision).unwrap_or(0));
            }
            gpui::App::notify(cx, entry.view.entity_id());
        }
    }
}

impl gpui::Render for IslandView {
    fn render(
        &mut self,
        window: &mut gpui::Window,
        cx: &mut gpui::Context<Self>,
    ) -> impl gpui::IntoElement {
        let Some(gpuix) = self.gpuix.upgrade() else {
            return gpui::Empty.into_any_element();
        };
        let node_id = self.node_id;
        if debug() {
            eprintln!("[islands] render {node_id}");
        }
        let Some(inherited) = self.inherited.borrow().clone() else {
            return gpui::Empty.into_any_element();
        };
        let (element, ids, revision) = gpuix.update(cx, |view, cx| {
            view.build_island_subtree(node_id, inherited, window, cx)
        });
        self.rendered.set(revision);
        self.painted.set(true);
        let snapshot = self.snapshot.clone();
        // A row, so the island's root stretches to the cached box's height (the
        // cross axis) and keeps its own width, flex-grow or percentage width.
        gpui::div()
            .flex()
            .flex_row()
            .size_full()
            .child(element)
            .child(
                // Deferred children paint after the ordinary island canvas. Snapshot
                // last, after overlays too, so replay retains their hit coordinates.
                gpui::deferred(gpui::canvas(
                    |_, _, _| (),
                    move |_, _, _, _| {
                        let mut snapshot = snapshot.borrow_mut();
                        snapshot.clear();
                        snapshot.extend(ids.iter().filter_map(|&id| get_bounds(id).map(|b| (id, b))));
                    },
                )
                .absolute()
                .size_0()).with_priority(usize::MAX),
            )
            .into_any_element()
    }
}

impl GpuixView {
    /// Build one island's subtree, the way `build_virtual_child` builds a row.
    pub(crate) fn build_island_subtree(
        &mut self,
        node_id: u64,
        inherited: Inherited,
        window: &mut gpui::Window,
        cx: &mut gpui::Context<Self>,
    ) -> (AnyElement, Vec<u64>, u64) {
        let tree_arc = self.tree.clone();
        let tree = tree_arc.lock().unwrap();
        let Some(revision) = tree.elements.get(&node_id).map(|element| element.subtree_revision) else {
            return (gpui::Empty.into_any_element(), Vec::new(), u64::MAX);
        };
        let mut ids = Vec::new();
        subtree_ids(&tree, node_id, &mut ids);
        let callback = self.event_callback.clone();
        let now = self.clock.now();
        let mut highlight_events = Vec::new();
        let keyboard_focus = self.keyboard_focus_path(&tree, window, cx);
        let mut ctx = BuildCtx {
            tree: &tree,
            event_callback: &callback,
            focus_handles: &self.focus_handles,
            scroll_handles: &mut self.scroll_handles,
            custom_registry: &mut self.custom_registry,
            virtual_lists: &mut self.virtual_lists,
            motion_states: &mut self.motion_states,
            now,
            selection: self.selection.clone(),
            inherited,
            highlights: &mut self.highlights,
            paint_reports: &self.paint_reports,
            highlight_events: &mut highlight_events,
            keyboard_focus,
            island_root: Some(node_id),
            in_virtual_row: false,
            islands: &mut self.islands,
        };
        let element = build_element(node_id, &mut ctx, window, cx);
        crate::renderer::emit_highlight_events(&callback, &highlight_events);
        (element, ids, revision)
    }
}

/// Pending offsets do not mutate the retained tree; wake every owning island.
/// Called outside draw, including nested islands, before the root is invalidated.
pub(crate) fn notify_pending_scrolls(view: &mut GpuixView, cx: &mut gpui::Context<GpuixView>) {
    let ids = crate::renderer::pending_scroll_ids();
    if ids.is_empty() { return; }
    let tree = view.tree.lock().unwrap();
    for (ancestor, entry) in &view.islands.entries {
        if ids.iter().any(|id| is_descendant(&tree, *id, *ancestor)) {
            gpui::App::notify(cx, entry.view.entity_id());
        }
    }
}
