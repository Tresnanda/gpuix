// Shared measured table geometry and an interactive horizontal scrollbar.
fn diff_gutters(file: &FileDiff, theme: &Theme) -> (f32,f32) {
    if !theme.diff.table_layout { let w=gutter_width(file,&theme.metrics); return (w,w); }
    let digit=number(theme.diff.gutter_digit_width,theme.metrics.diff_text_size*0.62415);
    let old=file.max_line.max(1);let new=old;
    (((old.max(1).ilog10()+1) as f32*digit+16.0).max(3.0*digit),
     ((new.max(1).ilog10()+1) as f32*digit+6.0).max(3.0*digit))
}
fn diff_scrollbar(handle: gpui::ScrollHandle, height:f32, tint:Hsla, id:u64) -> gpui::AnyElement {
    use gpui::prelude::*;
    let click=handle.clone();let drag=handle.clone();let paint=handle.clone();
    let set=|handle:&gpui::ScrollHandle,x:gpui::Pixels,cx:&mut gpui::App| {
        let bounds=handle.bounds();let w=f32::from(bounds.size.width);let max=f32::from(handle.max_offset().x);
        if w>0.0&&max>0.0 { let fraction=(f32::from(x-bounds.origin.x)/w).clamp(0.0,1.0);
            handle.set_offset(gpui::point(px(-max*fraction),px(0.0)));cx.refresh_windows(); }
    };
    gpui::div().id(SharedString::from(format!("__gpuix_diff_scrollbar_{id}"))).h(px(height)).w_full().flex_none()
        .on_mouse_down(gpui::MouseButton::Left,move |e,_,cx|set(&click,e.position.x,cx))
        .on_mouse_move(move |e,_,cx|if e.pressed_button==Some(gpui::MouseButton::Left){set(&drag,e.position.x,cx)})
        .child(gpui::canvas(|_,_,_|{},move |bounds,_,window,_| {
            let width=f32::from(bounds.size.width);let max=f32::from(paint.max_offset().x);
            if max<=0.0||width<=0.0{return;}
            let thumb=(width*width/(width+max)).max(18.0).min(width);
            let x=(-f32::from(paint.offset().x)/max).clamp(0.0,1.0)*(width-thumb);
            let rect=gpui::Bounds::new(bounds.origin+gpui::point(px(x+2.0),px(2.0)),gpui::size(px((thumb-4.0).max(0.0)),px((height-4.0).max(0.0))));
            window.paint_quad(gpui::quad(rect,px(height/2.0),tint,px(0.0),gpui::transparent_black(),BorderStyle::default()));
        }).size_full()).into_any_element()
}
