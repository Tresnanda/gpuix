//! Layout geometry reuse for opted-in PR rows. Prepaint and paint always run:
//! viewport masks, selection, links, hover, hitboxes and automation stay live.
use gpui::{AnyElement, App, Bounds, Element, GlobalElementId, InspectorElementId, IntoElement, LayoutId, Pixels, Window};
pub(crate) fn apply(child: AnyElement, key: [u64; 3]) -> AnyElement {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if !*ENABLED.get_or_init(|| std::env::var("KYBERN_SCROLL_LAYOUT").as_deref() != Ok("0")) {
        return child;
    }
    ScrollLayout { child, key }.into_any_element()
}
struct ScrollLayout {
    child: AnyElement,
    key: [u64; 3],
}
impl IntoElement for ScrollLayout {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}
impl Element for ScrollLayout {
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
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let id = self.child.request_layout(window, cx);
        window.cache_scroll_layout(id, self.key);
        (id, ())
    }
    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        self.child.prepaint(window, cx);
    }
    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        self.child.paint(window, cx);
    }
}
