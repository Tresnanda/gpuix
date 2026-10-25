//! Paint/prepaint translation: leave Taffy's layout and siblings untouched.
//! GPUI's element offset moves glyphs, hitboxes, selection and accessibility together.
use gpui::{AnyElement, App, Bounds, Element, GlobalElementId, InspectorElementId, IntoElement, LayoutId, Pixels, Point, Window};
pub(crate) fn apply(child: AnyElement, x: f64, y: f64) -> AnyElement {
    if x == 0.0 && y == 0.0 {
        return child;
    }
    Translate { child, offset: gpui::point(gpui::px(x as f32), gpui::px(y as f32)) }.into_any_element()
}
struct Translate {
    child: AnyElement,
    offset: Point<Pixels>,
}
impl IntoElement for Translate {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}
impl Element for Translate {
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
        (self.child.request_layout(window, cx), ())
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
        window.with_element_offset(self.offset, |window| {
            self.child.prepaint(window, cx);
        });
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
        window.with_element_offset(self.offset, |window| self.child.paint(window, cx));
    }
}
