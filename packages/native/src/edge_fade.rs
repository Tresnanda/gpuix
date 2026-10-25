//! `edgeFade` (N3): an alpha ramp toward chosen edges of an element, applied to
//! everything the element and its subtree paint (quads, borders, shadows, glyphs,
//! images, paths, underlines, surfaces, backdrops). Same layout/prepaint as the
//! child; paint runs inside `Window::with_edge_fade`, one link in the rounded
//! clip chain every shader already walks: no extra pass or render target.
//! Hit testing ignores it. The idea follows ZUI's `EdgeFade` (a Zed fork,
//! Apache-2.0/GPL); this is our own implementation and shares no code with it.
use gpui::{AnyElement,App,Bounds,Element,GlobalElementId,InspectorElementId,IntoElement,LayoutId,Pixels,Window};
#[derive(Clone,Copy,Default,serde::Deserialize)]
#[serde(default, rename_all="camelCase")]
struct Spec { top:f32, right:f32, bottom:f32, left:f32, inset_top:f32, inset_bottom:f32 }
/// Logical px. `insetTop`/`insetBottom` move that edge of the ramp inward:
/// content beyond it is transparent (a hard cut when its ramp is 0).
pub(crate) fn apply(child:AnyElement,value:Option<&serde_json::Value>)->AnyElement {
    let Some(v)=value else{return child};
    let Ok(spec)=serde_json::from_value::<Spec>(v.clone()) else{return child};
    let clean=|v:f32|if v.is_finite(){v.max(0.0)}else{0.0};
    let mut s=Spec{top:clean(spec.top),right:clean(spec.right),bottom:clean(spec.bottom),left:clean(spec.left),inset_top:clean(spec.inset_top),inset_bottom:clean(spec.inset_bottom)};
    // An inset without a ramp still cuts; a sub-pixel band makes that edge active.
    if s.inset_top>0.0&&s.top==0.0{s.top=0.01}
    if s.inset_bottom>0.0&&s.bottom==0.0{s.bottom=0.01}
    if s.top==0.0&&s.right==0.0&&s.bottom==0.0&&s.left==0.0{return child}
    EdgeFade{child,spec:s}.into_any_element()
}
struct EdgeFade{child:AnyElement,spec:Spec}
impl IntoElement for EdgeFade {type Element=Self;fn into_element(self)->Self{self}}
impl Element for EdgeFade {
 type RequestLayoutState=();type PrepaintState=();
 fn id(&self)->Option<gpui::ElementId>{None}
 fn source_location(&self)->Option<&'static std::panic::Location<'static>>{None}
 fn request_layout(&mut self,_:Option<&GlobalElementId>,_:Option<&InspectorElementId>,window:&mut Window,cx:&mut App)->(LayoutId,()){(self.child.request_layout(window,cx),())}
 fn prepaint(&mut self,_:Option<&GlobalElementId>,_:Option<&InspectorElementId>,_:Bounds<Pixels>,_:&mut (),window:&mut Window,cx:&mut App){self.child.prepaint(window,cx);}
 fn paint(&mut self,_:Option<&GlobalElementId>,_:Option<&InspectorElementId>,bounds:Bounds<Pixels>,_:&mut (),_:&mut (),window:&mut Window,cx:&mut App){
    let s=self.spec;
    let mut ramp=bounds;
    ramp.origin.y+=gpui::px(s.inset_top);
    ramp.size.height=(ramp.size.height-gpui::px(s.inset_top+s.inset_bottom)).max(gpui::px(0.0));
    window.with_edge_fade(ramp,[s.top,s.right,s.bottom,s.left],|window|self.child.paint(window,cx));
 }
}

/// Native capabilities the TS side reads once at startup (`src/native/local-build.ts`).
/// The stock addon has no such export.
#[napi_derive::napi]
pub fn kybern_capabilities() -> serde_json::Value {
    serde_json::json!({ "edgeFade": true })
}
