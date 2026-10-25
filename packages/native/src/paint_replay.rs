//! Fixed-geometry, passive leaf paint updates. Unknown cases fall back to notify.
use gpui::{AnyElement, App, Bounds, Element, GlobalElementId, InspectorElementId, IntoElement, LayoutId, Pixels, Window};
use crate::{retained_tree::RetainedElement, style::StyleDesc};

fn enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var("KYBERN_SCOPED_PAINT").as_deref() != Ok("0"))
}

// This allowlist intentionally excludes text, images, events, hover/active,
// filters, motion, borders and shadows. No geometry or input state is patched.
pub(crate) fn kind(element: &RetainedElement, style: &StyleDesc) -> Option<u8> {
    use crate::style::DimensionValue::Pixels;
    if !enabled() || !element.children.is_empty() || !element.events().is_empty()
        || element.content.is_some() || style.hover.is_some() || style.active.is_some()
        || style.box_shadow.is_some() || style.border_width.is_some()
        || style.border_top_width.is_some() || style.border_bottom_width.is_some()
        || style.border_left_width.is_some() || style.border_right_width.is_some()
        || element.custom_props().contains_key("motion") || element.custom_props().contains_key("backdrop")
        || element.custom_props().contains_key("edgeFade")
        || !matches!(style.width, Some(Pixels(v)) if v.is_finite() && v > 0.0)
        || !matches!(style.height, Some(Pixels(v)) if v.is_finite() && v > 0.0)
    { return None; }
    match element.element_type.as_str() {
        "div" if style.background.is_none() && style.background_color.is_some() => Some(0),
        "svg" if style.background.is_none() && style.background_color.is_none() => Some(1),
        _ => None,
    }
}

pub(crate) fn paint_only(element: &RetainedElement, next: &StyleDesc) -> bool {
    let Some(old) = element.style.as_deref() else { return false; };
    let Some(kind) = kind(element, old) else { return false; };
    if self::kind(element, next) != Some(kind) { return false; }
    let mut unchanged = next.clone();
    if kind == 0 { unchanged.background_color.clone_from(&old.background_color); }
    else { unchanged.opacity = old.opacity; }
    unchanged == *old && color(element, next).is_some()
}

pub(crate) fn color(element: &RetainedElement, style: &StyleDesc) -> Option<gpui::Hsla> {
    let kind = kind(element, style)?;
    let value = if kind == 0 { style.background_color.as_deref()? }
        else { style.color.as_deref().unwrap_or("#e2e2e2") };
    let opacity = style.opacity.unwrap_or(1.0);
    if !opacity.is_finite() || !(0.0..=1.0).contains(&opacity) { return None; }
    Some(gpui::Hsla::from(crate::color::parse_color_rgba(value)?).opacity(opacity as f32))
}

pub(crate) fn apply(child: AnyElement, element: &RetainedElement, style: Option<&StyleDesc>) -> AnyElement {
    let Some(kind) = style.and_then(|style| kind(element, style)) else { return child; };
    let animation = element.custom_props().get("paintAnimation").and_then(|value| {
        let mode = value.get("kind")?.as_str()?;
        if mode != "matrix" && mode != "breathe" { return None; }
        let cycle_ms = value.get("cycleMs")?.as_f64()?;
        let delay_ms = value.get("delayMs").and_then(|v| v.as_f64()).unwrap_or(0.0);
        if !cycle_ms.is_finite() || cycle_ms < 100.0 || !delay_ms.is_finite() || delay_ms < 0.0 { return None; }
        Some(gpui::PaintAnimation { cycle_ms, delay_ms, breathe: mode == "breathe", started: std::time::Instant::now() })
    });
    let color = style.and_then(|style| color(element, style)).unwrap_or_default();
    Tagged { child, id: element.id, kind, animation, color }.into_any_element()
}
struct Tagged { child: AnyElement, id: u64, kind: u8, animation: Option<gpui::PaintAnimation>, color: gpui::Hsla }
impl IntoElement for Tagged { type Element = Self; fn into_element(self) -> Self { self } }
impl Element for Tagged {
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
    fn paint(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, _: Bounds<Pixels>, _: &mut (), _: &mut (), window: &mut Window, cx: &mut App) {
        window.with_repaint_tag(self.id, self.kind, self.animation, self.color, |window| self.child.paint(window, cx));
    }
}

pub(crate) fn invalidate(view: &mut crate::renderer::GpuixView, window: &mut Window, cx: &mut gpui::Context<crate::renderer::GpuixView>) {
    let tree_arc = view.tree.clone();
    let mut tree = tree_arc.lock().unwrap();
    tree.paint_replay_enabled = true;
    let ids = std::mem::take(&mut tree.paint_updates);
    let paints = ids.iter().filter_map(|id| {
        let element = tree.elements.get(id)?;
        Some((*id, color(element, element.style.as_deref()?)?))
    }).collect::<Vec<_>>();
    if !tree.full_redraw && !ids.is_empty() && paints.len() == ids.len() && window.repaint_tagged(&paints) {
        if std::env::var_os("KYBERN_DEBUG_ISLANDS").is_some() { eprintln!("[islands] scoped paint {} leaves", ids.len()); }
        return;
    }
    // A mount, mixed batch, clipped/unpainted leaf or unsupported primitive must
    // rebuild normally. Mark every deferred mutation before notifying islands.
    for id in ids { tree.mark_paint_fallback(id); }
    tree.full_redraw = false;
    drop(tree);
    crate::cache_island::notify_changed_islands(view, cx);
    cx.notify();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{retained_tree::RetainedTree, style::DimensionValue};
    use std::sync::Arc;
    fn fill() -> StyleDesc { StyleDesc { width: Some(DimensionValue::Pixels(14.0)), height: Some(DimensionValue::Pixels(14.0)), background_color: Some("#ffffff".into()), ..Default::default() } }
    #[test]
    fn paint_allowlist_rejects_geometry_interaction_and_complex_paint() {
        let old = fill();
        let mut node = RetainedElement::new(1, "div".into(), 1);
        node.style = Some(Arc::new(old.clone()));
        let mut next = old.clone(); next.background_color = Some("#aabbcc".into());
        assert!(paint_only(&node, &next));
        next.width = Some(DimensionValue::Pixels(15.0)); assert!(!paint_only(&node, &next));
        next = old.clone(); next.opacity = Some(0.2); assert!(!paint_only(&node, &next));
        node.events_mut().insert("click".into()); assert_eq!(kind(&node, &old), None);
        node.events_mut().clear(); node.children.push(2); assert_eq!(kind(&node, &old), None);
        node.children.clear(); node.custom_props_mut().insert("backdrop".into(), serde_json::json!({"blur":4})); assert_eq!(kind(&node, &old), None);
    }
    #[test]
    fn paint_is_deferred_but_mixed_geometry_and_fallback_invalidate_ancestors() {
        let mut tree = RetainedTree::new();
        tree.create_element(1, "div".into()); tree.create_element(2, "div".into()); tree.append_child(1,2);
        tree.set_style(2,Arc::new(fill())); tree.full_redraw=false; tree.paint_replay_enabled=true;
        let before=tree.elements[&1].subtree_revision;
        let mut next=fill(); next.background_color=Some("#ff0000".into());
        tree.set_style(2,Arc::new(next.clone()));
        assert!(!tree.full_redraw); assert!(tree.paint_updates.contains(&2)); assert_eq!(tree.elements[&1].subtree_revision,before);
        tree.mark_paint_fallback(2); assert!(tree.full_redraw); assert!(tree.elements[&1].subtree_revision>before);
        tree.full_redraw=false; next.width=Some(DimensionValue::Pixels(25.0)); tree.set_style(2,Arc::new(next)); assert!(tree.full_redraw);
    }
    #[test]
    fn hosts_without_scene_replay_keep_normal_revision_semantics() {
        let mut tree=RetainedTree::new(); tree.create_element(1,"div".into()); tree.set_style(1,Arc::new(fill()));
        let before=tree.elements[&1].subtree_revision;
        let mut next=fill(); next.background_color=Some("#ff0000".into()); tree.set_style(1,Arc::new(next));
        assert!(tree.elements[&1].subtree_revision>before); assert!(tree.paint_updates.is_empty());
    }
}
