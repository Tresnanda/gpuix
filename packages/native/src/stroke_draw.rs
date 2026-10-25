//! `paintAnimation: { kind: 'stroke', cycleMs, delayMs?, dash, ease?, from?, to?, retarget? }` on an `<svg source>`: the glyph's paths stroke
//! themselves in (`stroke-dasharray: dash`, `stroke-dashoffset: dash -> 0` over `cycleMs` after `delayMs`, on the
//! cubic-bezier `ease`, default `--ease-smooth-out`), like the desktop's `t-check-draw`. Paint-only: the scene swaps the
//! sprite's atlas tile for the current dash offset at every presented frame (`stroke_draw_scene.rs`), with no element
//! render, layout or prepaint. Reduce Motion is the caller's: it omits the prop. `from`/`to` set the offsets it runs between
//! (a ring's value: `to = dash * (1 - v)` on a closed path of length `dash`); with `retarget`, a new `to` eases from the offset
//! on screen (N2), and the glyph rests on the `to` tile instead of the plain glyph.
use gpui::{AnyElement, App, Bounds, Element, GlobalElementId, InspectorElementId, IntoElement, LayoutId, Pixels, Window};
use crate::retained_tree::RetainedElement;

pub(crate) fn spec(element: &RetainedElement) -> Option<gpui::StrokeDrawSpec> {
    if element.element_type != "svg" { return None; }
    let value = element.custom_props().get("paintAnimation")?;
    if value.get("kind")?.as_str()? != "stroke" { return None; }
    let number = |key: &str| value.get(key).and_then(serde_json::Value::as_f64).filter(|v| v.is_finite());
    let duration_ms = number("cycleMs").filter(|v| *v > 0.0)?;
    let delay_ms = number("delayMs").unwrap_or(0.0).max(0.0);
    let dash = number("dash").filter(|v| *v > 0.0)? as f32;
    let ease = match value.get("ease").and_then(serde_json::Value::as_array) {
        Some(list) if list.len() == 4 => {
            let mut ease = [0.0f64; 4];
            for (slot, v) in ease.iter_mut().zip(list) { *slot = v.as_f64().filter(|v| v.is_finite())?; }
            ease
        }
        _ => [0.22, 1.0, 0.36, 1.0],
    };
    let source = element.custom_props().get("source")?.as_str()?;
    if source.trim().is_empty() { return None; }
    let from = number("from").map_or(dash, |v| v as f32);
    let to = number("to").map_or(0.0, |v| v as f32);
    let retarget = value.get("retarget").and_then(serde_json::Value::as_bool).unwrap_or(false);
    Some(gpui::StrokeDrawSpec { bytes: std::sync::Arc::from(source.as_bytes()), dash, delay_ms, duration_ms, ease, from, to, retarget })
}

/// Stroke draws follow the view's automation clock, like `paintTransform`, so a paused clock pauses them too.
pub(crate) fn arm(clock: &crate::automation::AutomationClock) {
    static ARMED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if ARMED.swap(true, std::sync::atomic::Ordering::Relaxed) { return; }
    let clock = clock.clone();
    gpui::set_stroke_draw_clock(Some(std::sync::Arc::new(move || clock.now())));
}

pub(crate) fn apply(child: AnyElement, element: &RetainedElement) -> AnyElement {
    match spec(element) {
        Some(spec) => StrokeDraw { child, id: element.id, spec }.into_any_element(),
        None => child,
    }
}

struct StrokeDraw { child: AnyElement, id: u64, spec: gpui::StrokeDrawSpec }
impl IntoElement for StrokeDraw { type Element = Self; fn into_element(self) -> Self { self } }
impl Element for StrokeDraw {
    type RequestLayoutState = ();
    type PrepaintState = ();
    fn id(&self) -> Option<gpui::ElementId> { None }
    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> { None }
    fn request_layout(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, window: &mut Window, cx: &mut App) -> (LayoutId, ()) {
        (self.child.request_layout(window, cx), ())
    }
    fn prepaint(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, _: Bounds<Pixels>, _: &mut (), window: &mut Window, cx: &mut App) {
        self.child.prepaint(window, cx);
    }
    fn paint(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, bounds: Bounds<Pixels>, _: &mut (), _: &mut (), window: &mut Window, cx: &mut App) {
        let child = &mut self.child;
        window.with_stroke_draw(self.id, &self.spec, bounds, cx, |window, cx| child.paint(window, cx));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_only_stroke_on_svg_with_source() {
        let mut node = RetainedElement::new(1, "svg".into(), 1);
        node.custom_props_mut().insert("source".into(), serde_json::json!("<svg><path d=\"M0 0L1 1\"/></svg>"));
        node.custom_props_mut().insert("paintAnimation".into(), serde_json::json!({"kind":"stroke","cycleMs":250,"delayMs":150,"dash":32}));
        let spec = spec(&node).unwrap();
        assert_eq!((spec.dash, spec.delay_ms, spec.duration_ms, spec.ease), (32.0, 150.0, 250.0, [0.22, 1.0, 0.36, 1.0]));
        assert_eq!((spec.from, spec.to, spec.retarget), (32.0, 0.0, false));
        node.custom_props_mut().insert("paintAnimation".into(), serde_json::json!({"kind":"pop","cycleMs":250}));
        assert!(spec_is_none(&node));
        node.custom_props_mut().insert("paintAnimation".into(), serde_json::json!({"kind":"stroke","cycleMs":250}));
        assert!(spec_is_none(&node));
    }
    fn spec_is_none(node: &RetainedElement) -> bool { spec(node).is_none() }
}
