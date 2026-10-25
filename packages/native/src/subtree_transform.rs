//! `paintTransform` (render9): a paint-only scale / rotation / translation / opacity / blur of a
//! host div's whole subtree about a transform origin, evaluated at native display callbacks.
//! GPUI keeps each transformed primitive's untransformed base (`Window::transform_subtrees`),
//! so a frame re-maps retained paint, hitboxes and accessibility bounds without rendering React,
//! running Taffy, waking islands or invalidating ancestors. Layout slots never move.
//!
//! ```json
//! { "initial": false | {"scale": .97, "scaleX": 1, "scaleY": 1, "opacity": 0, "blur": 2, "translateY": 16},
//!   "animate": {"scale": 1, "opacity": 1, "blur": 0, "translateY": 0},
//!   // optional rounded clip: the host box inset by clipTop/Right/Bottom/Left (px, before the transform), corners
//!   // of clipRadius px on screen at any scale; give it in both ends (no clipRadius, or a negative one: no clip)
//!   // e.g. {"clipLeft": 120, "clipRight": 120, "clipRadius": 12}
//!   "transition": {"duration": .35, "delay": 0, "ease": [.22, 1, .36, 1]},
//!   "keyframes": [{"offset": .6, "rotate": 4, "scale": 1.06, "translateY": -1}],
//!   "origin": [.5, .5],
//!   "press": {"scale": .97, "down": {"duration": .08, "ease": [...]}, "up": {"duration": .15, "ease": [...]}},
//!   "generation": 1, "retain": true }
//! ```
//! Missing values are the identity (scale 1, opacity 1, others 0). `keyframes` are CSS-style
//! intermediate stops between the start (`initial`, or the current value on a retarget) and
//! `animate`; each segment uses `ease`, and a property absent from a stop interpolates across
//! it. Changing `animate`, `keyframes` or `generation` retargets from the value on screen; a new `generation` with an
//! `initial` value restarts from that value instead.
//! `press` is the desktop's `:active` squash, driven natively by mouse down/up on the host. `retain` keeps the
//! scope tagged at rest (identity), so an exit or reveal that starts later is retained from its first frame.
use crate::{renderer::GpuixView, retained_tree::RetainedTree};
use gpui::{
    AnyElement, App, Bounds, DispatchPhase, Element, GlobalElementId, Hitbox, HitboxBehavior, InspectorElementId, IntoElement,
    LayoutId, MouseButton, MouseDownEvent, MouseUpEvent, Pixels, SubtreeTransformValue, Window,
};
use std::{cell::RefCell, collections::HashMap};
use web_time::Instant;

#[derive(Clone, Copy, Debug, Default, PartialEq, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct Partial {
    scale: Option<f32>,
    scale_x: Option<f32>,
    scale_y: Option<f32>,
    shadow_opacity: Option<f32>,
    rotate: Option<f32>,
    translate_x: Option<f32>,
    translate_y: Option<f32>,
    opacity: Option<f32>,
    blur: Option<f32>,
    clip_top: Option<f32>,
    clip_right: Option<f32>,
    clip_bottom: Option<f32>,
    clip_left: Option<f32>,
    clip_radius: Option<f32>,
}
impl Partial {
    fn over(self, base: SubtreeTransformValue) -> SubtreeTransformValue {
        SubtreeTransformValue {
            scale: self.scale.unwrap_or(base.scale),
            scale_x: self.scale_x.unwrap_or(base.scale_x),
            scale_y: self.scale_y.unwrap_or(base.scale_y),
            shadow_opacity: self.shadow_opacity.unwrap_or(base.shadow_opacity),
            rotate: self.rotate.unwrap_or(base.rotate),
            translate_x: self.translate_x.unwrap_or(base.translate_x),
            translate_y: self.translate_y.unwrap_or(base.translate_y),
            opacity: self.opacity.unwrap_or(base.opacity),
            blur: self.blur.unwrap_or(base.blur),
            clip: [
                self.clip_top.unwrap_or(base.clip[0]),
                self.clip_right.unwrap_or(base.clip[1]),
                self.clip_bottom.unwrap_or(base.clip[2]),
                self.clip_left.unwrap_or(base.clip[3]),
            ],
            clip_radius: self.clip_radius.unwrap_or(base.clip_radius),
        }
    }
    fn get(&self, field: usize) -> Option<f32> {
        self.fields()[field]
    }
    fn fields(&self) -> [Option<f32>; FIELDS] {
        [
            self.scale,
            self.rotate,
            self.translate_x,
            self.translate_y,
            self.opacity,
            self.blur,
            self.scale_x,
            self.scale_y,
            self.shadow_opacity,
            self.clip_top,
            self.clip_right,
            self.clip_bottom,
            self.clip_left,
            self.clip_radius,
        ]
    }
}
const FIELDS: usize = 14;
fn field(value: &SubtreeTransformValue, field: usize) -> f32 {
    let [top, right, bottom, left] = value.clip;
    [
        value.scale,
        value.rotate,
        value.translate_x,
        value.translate_y,
        value.opacity,
        value.blur,
        value.scale_x,
        value.scale_y,
        value.shadow_opacity,
        top,
        right,
        bottom,
        left,
        value.clip_radius,
    ][field]
}
fn set_field(value: &mut SubtreeTransformValue, field: usize, to: f32) {
    match field {
        0 => value.scale = to,
        1 => value.rotate = to,
        2 => value.translate_x = to,
        3 => value.translate_y = to,
        4 => value.opacity = to,
        5 => value.blur = to,
        6 => value.scale_x = to,
        7 => value.scale_y = to,
        8 => value.shadow_opacity = to,
        9..=12 => value.clip[field - 9] = to,
        _ => value.clip_radius = to,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct Timing {
    #[serde(default = "default_duration")]
    duration: f64,
    #[serde(default)]
    delay: f64,
    #[serde(default = "default_ease")]
    ease: [f64; 4],
}
fn default_duration() -> f64 {
    0.2
}
fn default_ease() -> [f64; 4] {
    [0.23, 1.0, 0.32, 1.0]
}
impl Default for Timing {
    fn default() -> Self {
        Self { duration: default_duration(), delay: 0.0, ease: default_ease() }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, serde::Deserialize)]
struct Press {
    scale: f32,
    #[serde(default)]
    down: Option<Timing>,
    #[serde(default)]
    up: Option<Timing>,
}

#[derive(Clone, Debug, PartialEq, serde::Deserialize)]
#[serde(untagged)]
enum Initial {
    Off(bool),
    Value(Partial),
}

#[derive(Clone, Debug, PartialEq, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct Source {
    #[serde(default)]
    initial: Option<Initial>,
    #[serde(default)]
    animate: Partial,
    #[serde(default)]
    transition: Option<Timing>,
    #[serde(default)]
    keyframes: Vec<KeyStop>,
    #[serde(default)]
    origin: Option<[f32; 2]>,
    #[serde(default)]
    press: Option<Press>,
    #[serde(default)]
    generation: Option<f64>,
    /// Keep the scope's retained paint while it rests at identity, so the next change (an exit, a hover reveal
    /// hiding) is a retained update from its first frame instead of a render.
    #[serde(default)]
    retain: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, serde::Deserialize)]
struct KeyStop {
    offset: f64,
    #[serde(flatten)]
    value: Partial,
}

fn parse(value: &serde_json::Value) -> Option<Source> {
    let source: Source = serde_json::from_value(value.clone()).ok()?;
    let finite = |p: &Partial| {
        p.fields().iter().flatten().all(|v| v.is_finite())
            && [p.scale, p.scale_x, p.scale_y].iter().flatten().all(|s| *s > 0.0)
    };
    let timing = |t: &Timing| t.duration.is_finite() && t.duration >= 0.0 && t.delay.is_finite() && t.delay >= 0.0 && t.ease.iter().all(|v| v.is_finite());
    let ok = finite(&source.animate)
        && source.transition.as_ref().is_none_or(timing)
        && source.keyframes.iter().all(|k| k.offset.is_finite() && (0.0..=1.0).contains(&k.offset) && finite(&k.value))
        && match &source.initial {
            Some(Initial::Value(p)) => finite(p),
            _ => true,
        }
        && source.origin.is_none_or(|o| o.iter().all(|v| v.is_finite()))
        && source.press.as_ref().is_none_or(|p| {
            p.scale.is_finite() && p.scale > 0.0 && p.down.as_ref().is_none_or(timing) && p.up.as_ref().is_none_or(timing)
        });
    ok.then(|| {
        let mut source = source;
        source.keyframes.sort_by(|a, b| a.offset.total_cmp(&b.offset));
        source
    })
}
pub(crate) fn valid(value: &serde_json::Value) -> bool {
    value.is_null() || parse(value).is_some()
}

/// `cubic-bezier(x1, y1, x2, y2)` at `x`; y may leave [0, 1] (`--ease-bounce`).
fn bezier(ease: [f64; 4], x: f64) -> f64 {
    let [x1, y1, x2, y2] = ease;
    let at = |t: f64, a: f64, b: f64| 3.0 * a * t * (1.0 - t).powi(2) + 3.0 * b * t * t * (1.0 - t) + t * t * t;
    let (mut lo, mut hi) = (0.0f64, 1.0f64);
    for _ in 0..28 {
        let mid = (lo + hi) * 0.5;
        if at(mid, x1, x2) < x { lo = mid } else { hi = mid }
    }
    at((lo + hi) * 0.5, y1, y2)
}

struct Track {
    source: Source,
    from: SubtreeTransformValue,
    to: SubtreeTransformValue,
    stops: Vec<KeyStop>,
    timing: Timing,
    start: Instant,
    pressed: bool,
    settled: bool,
    blur_seen: bool,
    /// A tick found no retained paint and asked for a render; until the scope is found again (it was hidden, never
    /// painted), further ticks do not ask again.
    fallback: bool,
}
impl Track {
    fn new(source: Source, now: Instant) -> Self {
        let to = source.animate.over(SubtreeTransformValue::default());
        let from = match &source.initial {
            Some(Initial::Value(p)) => p.over(SubtreeTransformValue::default()),
            _ => to,
        };
        let mut track = Track {
            stops: source.keyframes.clone(),
            timing: source.transition.unwrap_or_default(),
            source,
            from,
            to,
            start: now,
            pressed: false,
            settled: false,
            blur_seen: false,
            fallback: false,
        };
        track.settled = !track.active(now);
        track.blur_seen = track.has_blur();
        track
    }
    fn has_blur(&self) -> bool {
        self.from.blur > 0.0 || self.to.blur > 0.0 || self.stops.iter().any(|k| k.value.blur.is_some_and(|b| b > 0.0))
    }
    fn target(&self) -> SubtreeTransformValue {
        let mut to = self.source.animate.over(SubtreeTransformValue::default());
        if let (true, Some(press)) = (self.pressed, self.source.press) {
            to.scale *= press.scale;
        }
        to
    }
    fn retarget(&mut self, now: Instant, timing: Timing, stops: Vec<KeyStop>) {
        self.from = self.value(now);
        self.to = self.target();
        self.stops = stops;
        self.timing = timing;
        self.start = now;
        self.settled = false;
        self.blur_seen |= self.has_blur();
    }
    fn sync(&mut self, source: Source, now: Instant) {
        if source == self.source {
            return;
        }
        let generation = source.generation != self.source.generation;
        let retarget = source.animate != self.source.animate || source.keyframes != self.source.keyframes || generation;
        self.source = source;
        if retarget {
            let timing = self.source.transition.unwrap_or_default();
            let stops = self.source.keyframes.clone();
            self.retarget(now, timing, stops);
            // A new generation with an `initial` value restarts from it (a loop's next pass) instead of the value on screen.
            if let (true, Some(Initial::Value(initial))) = (generation, &self.source.initial) {
                self.from = initial.over(SubtreeTransformValue::default());
            }
        }
    }
    fn press(&mut self, pressed: bool, now: Instant) -> bool {
        let Some(press) = self.source.press else { return false };
        if self.pressed == pressed {
            return false;
        }
        self.pressed = pressed;
        let timing = if pressed { press.down } else { press.up }.unwrap_or_default();
        self.retarget(now, timing, Vec::new());
        true
    }
    fn progress(&self, now: Instant) -> f64 {
        let elapsed = now.saturating_duration_since(self.start).as_secs_f64() - self.timing.delay;
        if self.timing.duration <= 0.0 { if elapsed >= 0.0 { 1.0 } else { 0.0 } } else { (elapsed / self.timing.duration).clamp(0.0, 1.0) }
    }
    fn active(&self, now: Instant) -> bool {
        self.progress(now) < 1.0 && self.from != self.to || !self.stops.is_empty() && self.progress(now) < 1.0
    }
    fn value(&self, now: Instant) -> SubtreeTransformValue {
        let p = self.progress(now);
        let mut out = self.to;
        for f in 0..FIELDS {
            // The stops that set this property, with the start and the target at the ends.
            let mut points = vec![(0.0, field(&self.from, f))];
            points.extend(self.stops.iter().filter_map(|k| k.value.get(f).map(|v| (k.offset, v))));
            points.push((1.0, field(&self.to, f)));
            let i = points.windows(2).position(|w| p <= w[1].0).unwrap_or(points.len() - 2);
            let ((o0, v0), (o1, v1)) = (points[i], points[i + 1]);
            let t = if o1 > o0 { ((p - o0) / (o1 - o0)).clamp(0.0, 1.0) } else { 1.0 };
            let e = bezier(self.timing.ease, t) as f32;
            set_field(&mut out, f, v0 + (v1 - v0) * e);
        }
        out.opacity = out.opacity.clamp(0.0, 1.0);
        out.shadow_opacity = out.shadow_opacity.clamp(0.0, 1.0);
        out.blur = out.blur.max(0.0);
        out.scale = out.scale.max(0.0001);
        out.scale_x = out.scale_x.max(0.0001);
        out.scale_y = out.scale_y.max(0.0001);
        out
    }
    /// Paint the scope (and keep its base primitives) while it can change without a render.
    fn tagged(&self, now: Instant) -> bool {
        self.source.press.is_some() || self.source.retain || self.active(now) || !self.value(now).is_identity()
    }
}

thread_local! {
    static TRACKS: RefCell<HashMap<u64, Track>> = RefCell::default();
    static CLOCK: RefCell<Option<crate::automation::AutomationClock>> = const { RefCell::new(None) };
    static VIEW: RefCell<Option<gpui::WeakEntity<GpuixView>>> = const { RefCell::new(None) };
    static SCHEDULED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}
fn now() -> Instant {
    CLOCK.with(|c| c.borrow().as_ref().map_or_else(Instant::now, |c| c.now()))
}

pub(crate) fn forget(id: u64) {
    TRACKS.with(|t| {
        t.borrow_mut().remove(&id);
    });
}
pub(crate) fn sync(tree: &RetainedTree, now: Instant) {
    TRACKS.with(|tracks| {
        let mut tracks = tracks.borrow_mut();
        tracks.retain(|id, _| tree.elements.get(id).is_some_and(|e| e.custom_props().contains_key("paintTransform")));
        for (id, element) in &tree.elements {
            let Some(source) = element.custom_props().get("paintTransform").and_then(parse) else { continue };
            match tracks.get_mut(id) {
                Some(track) => track.sync(source, now),
                None => {
                    tracks.insert(*id, Track::new(source, now));
                }
            }
        }
    });
}
pub(crate) fn apply(child: AnyElement, id: u64, tree: &RetainedTree, now: Instant) -> AnyElement {
    let Some(source) = tree.elements.get(&id).and_then(|e| e.custom_props().get("paintTransform")) else { return child };
    let scope = TRACKS.with(|tracks| {
        let mut tracks = tracks.borrow_mut();
        // A newly mounted virtual row can introduce its scope during prepaint.
        if !tracks.contains_key(&id) {
            let source = parse(source)?;
            tracks.insert(id, Track::new(source, now));
        }
        let track = &tracks[&id];
        track.tagged(now).then(|| (track.value(now), track.source.origin.unwrap_or([0.5, 0.5]), track.source.press.is_some(), track.blur_seen))
    });
    let Some((value, origin, press, blur)) = scope else { return child };
    if gpui::kybern_motion_trace_enabled() {
        gpui::kybern_motion_trace(format_args!("ptx-apply {id}:{:.3}", value.opacity));
    }
    Transformed { child, id, value, origin, press, blur }.into_any_element()
}

struct Transformed {
    child: AnyElement,
    id: u64,
    value: SubtreeTransformValue,
    origin: [f32; 2],
    press: bool,
    blur: bool,
}
impl Transformed {
    fn origin(&self, bounds: Bounds<Pixels>) -> gpui::Point<Pixels> {
        gpui::point(bounds.origin.x + bounds.size.width * self.origin[0], bounds.origin.y + bounds.size.height * self.origin[1])
    }
}
impl IntoElement for Transformed {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}
impl Element for Transformed {
    type RequestLayoutState = ();
    type PrepaintState = Option<Hitbox>;
    fn id(&self) -> Option<gpui::ElementId> {
        None
    }
    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }
    fn request_layout(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, w: &mut Window, c: &mut App) -> (LayoutId, ()) {
        (self.child.request_layout(w, c), ())
    }
    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        w: &mut Window,
        c: &mut App,
    ) -> Option<Hitbox> {
        let origin = self.origin(bounds);
        w.with_subtree_transform(self.id, origin, bounds, self.value, |w| {
            // Under the children, so it never takes their presses; occluding overlays block it.
            let hitbox = self.press.then(|| w.insert_hitbox(bounds, HitboxBehavior::Normal));
            self.child.prepaint(w, c);
            hitbox
        })
    }
    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        hitbox: &mut Option<Hitbox>,
        w: &mut Window,
        c: &mut App,
    ) {
        let origin = self.origin(bounds);
        let blur = self.blur;
        w.with_subtree_transform(self.id, origin, bounds, self.value, |w| {
            self.child.paint(w, c);
            // `filter: blur()` over what the subtree painted: a unit-blur backdrop pass marked with
            // fade_top -1, whose radius follows the scope's blur value on every retained frame.
            if blur {
                w.paint_backdrop(bounds.dilate(gpui::px(8.0)), gpui::px(1.0), 1.0, gpui::px(0.0), 1.0, -1.0);
            }
        });
        if let Some(hitbox) = hitbox.take() {
            let id = self.id;
            w.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
                if phase == DispatchPhase::Capture && event.button == MouseButton::Left && hitbox.is_hovered(window) {
                    press(id, true, window, cx);
                }
            });
            w.on_mouse_event(move |event: &MouseUpEvent, phase, window, cx| {
                if phase == DispatchPhase::Capture && event.button == MouseButton::Left {
                    press(id, false, window, cx);
                }
            });
        }
    }
}

fn press(id: u64, pressed: bool, window: &mut Window, cx: &mut App) {
    let now = now();
    if TRACKS.with(|tracks| tracks.borrow_mut().get_mut(&id).is_some_and(|track| track.press(pressed, now))) {
        let Some(entity) = VIEW.with(|v| v.borrow().clone()) else { return };
        schedule_entity(entity, window);
        let _ = cx;
    }
}

/// Remember the clock and view for native press handlers, which run outside a render.
pub(crate) fn remember(view: &GpuixView, cx: &mut gpui::Context<GpuixView>) {
    CLOCK.with(|c| *c.borrow_mut() = Some(view.clock.clone()));
    VIEW.with(|v| *v.borrow_mut() = Some(cx.weak_entity()));
}
fn prepare(view: &GpuixView, cx: &mut gpui::Context<GpuixView>) {
    remember(view, cx);
    sync(&view.tree.lock().unwrap(), view.clock.now());
}
/// At the start of a render (the tree is locked by the caller and already synchronized).
pub(crate) fn arm(view: &GpuixView, window: &mut Window, cx: &mut gpui::Context<GpuixView>) {
    remember(view, cx);
    let now = view.clock.now();
    if TRACKS.with(|t| t.borrow().values().any(|track| track.active(now))) {
        schedule_entity(cx.weak_entity(), window);
    }
}
fn schedule_entity(entity: gpui::WeakEntity<GpuixView>, window: &mut Window) {
    if SCHEDULED.with(|s| s.replace(true)) {
        return;
    }
    window.on_next_frame(move |window, cx| {
        SCHEDULED.with(|s| s.set(false));
        let Some(entity) = entity.upgrade() else { return };
        entity.update(cx, |view, cx| {
            tick(view, window, cx);
        });
    });
}

/// Present the current values. False when a scope has no retained paint yet (not tagged at
/// the last render), so the caller renders normally.
pub(crate) fn tick(view: &mut GpuixView, window: &mut Window, cx: &mut gpui::Context<GpuixView>) -> bool {
    let now = view.clock.now();
    let (updates, active, settled, rerender) = TRACKS.with(|tracks| {
        let mut tracks = tracks.borrow_mut();
        let (mut updates, mut active, mut settled, mut rerender) = (Vec::new(), false, Vec::new(), Vec::new());
        for (&id, track) in tracks.iter_mut() {
            if track.settled {
                continue;
            }
            updates.push((id, track.value(now)));
            if track.active(now) {
                active = true;
            } else {
                track.settled = true;
                settled.push((id, track.source.generation));
                // A finished blur leaves no backdrop pass behind.
                if track.blur_seen {
                    track.blur_seen = track.has_blur() && track.to.blur > 0.0;
                    rerender.push(id);
                }
            }
        }
        (updates, active, settled, rerender)
    });
    let (found, missing): (Vec<_>, Vec<_>) = updates.into_iter().partition(|(id, _)| window.has_subtree_transform(*id));
    // One fallback render per scope that has no retained paint; a scope that render did not paint (hidden) waits for the
    // next ordinary render instead of rendering the window every frame of its tween.
    let missing: Vec<_> = TRACKS.with(|tracks| {
        let mut tracks = tracks.borrow_mut();
        for (id, _) in &found {
            if let Some(track) = tracks.get_mut(id) { track.fallback = false; }
        }
        missing.into_iter().filter(|(id, _)| tracks.get_mut(id).is_some_and(|track| !std::mem::replace(&mut track.fallback, true))).collect()
    });
    if gpui::kybern_motion_trace_enabled() {
        let list = |v: &[(u64, SubtreeTransformValue)]| v.iter().map(|(id, value)| format!(" {id}:{:.3}", value.opacity)).collect::<String>();
        gpui::kybern_motion_trace(format_args!("ptx-tick found{} missing{}", list(&found), list(&missing)));
    }
    if !found.is_empty() {
        window.transform_subtrees(&found);
    }
    if !missing.is_empty() {
        // Nothing retained for these scopes yet (or not painted): render them normally; the render tags them.
        let mut tree = view.tree.lock().unwrap();
        for (id, _) in &missing {
            tree.mark_paint_fallback(*id);
        }
        tree.full_redraw = true;
        drop(tree);
        crate::cache_island::notify_changed_islands(view, cx);
        cx.notify();
    }
    for (id, generation) in settled {
        let Some(generation) = generation else { continue };
        if !view.tree.lock().unwrap().elements.get(&id).is_some_and(|e| e.events().contains("motionComplete")) {
            continue;
        }
        if let Some(callback) = &view.event_callback {
            let mut event = crate::element_tree::EventPayload::default();
            event.element_id = id as f64;
            event.event_type = "motionComplete".into();
            event.motion_generation = Some(generation);
            callback(event);
        }
    }
    if !rerender.is_empty() {
        let mut tree = view.tree.lock().unwrap();
        for id in rerender {
            tree.mark_paint_fallback(id);
        }
        drop(tree);
        crate::cache_island::notify_changed_islands(view, cx);
        cx.notify();
    }
    if active {
        schedule_entity(cx.weak_entity(), window);
    }
    missing.is_empty()
}

/// A batch that only changed `paintTransform` values of tagged scopes presents them retained.
pub(crate) fn invalidate(view: &mut GpuixView, window: &mut Window, cx: &mut gpui::Context<GpuixView>) -> bool {
    prepare(view, cx);
    let mut tree = view.tree.lock().unwrap();
    if tree.subtree_transform_updates.is_empty() {
        return false;
    }
    let ids = std::mem::take(&mut tree.subtree_transform_updates);
    let only = !tree.full_redraw && tree.paint_updates.is_empty();
    let others = !tree.subtree_translation_updates.is_empty();
    if !only {
        for id in ids {
            tree.mark_paint_fallback(id);
        }
        tree.full_redraw = true;
        return false;
    }
    drop(tree);
    tick(view, window, cx) && !others
}
