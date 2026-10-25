//! `hoverTransition` (N1): the colour transition a host div's `hover` / `active` background and border colours take.
//! Declared on the element or any ancestor (the app sets one at its root); `false` opts an element and its subtree out.
//!
//! ```json
//! { "duration": 0.09, "ease": [0.4, 0, 0.2, 1] }
//! ```
use crate::retained_tree::{RetainedElement, RetainedTree};

fn parse(value: &serde_json::Value) -> Option<gpui::ColorTransition> {
    let object = value.as_object()?;
    let duration = object.get("duration").and_then(serde_json::Value::as_f64).unwrap_or(0.09);
    let ease = object
        .get("ease")
        .and_then(|e| serde_json::from_value::<[f32; 4]>(e.clone()).ok())
        .unwrap_or([0.4, 0.0, 0.2, 1.0]);
    (duration.is_finite() && duration > 0.0 && ease.iter().all(|v| v.is_finite())).then_some(gpui::ColorTransition { duration: duration as f32, ease })
}

/// The nearest declaration on `element` or its ancestors.
pub(crate) fn transition_for(element: &RetainedElement, tree: &RetainedTree) -> Option<gpui::ColorTransition> {
    let mut current = Some(element);
    while let Some(node) = current {
        if let Some(value) = node.custom_props().get("hoverTransition") {
            return parse(value);
        }
        current = node.parent.and_then(|parent| tree.elements.get(&parent));
    }
    None
}

/// Only hover/active styles that change a colour fade.
pub(crate) fn fades(style: &crate::style::StyleDesc) -> bool {
    let colors = |s: &crate::style::StyleDesc| s.background_color.is_some() || s.border_color.is_some();
    style.hover.as_deref().is_some_and(colors) || style.active.as_deref().is_some_and(colors)
}
