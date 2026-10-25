//! Inline SVG icons drawn by the text elements (link chips, composer tokens).
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

thread_local! {
    static SVG_RENDERER: RefCell<Option<gpui::SvgRenderer>> = const { RefCell::new(None) };
    static ICONS: RefCell<HashMap<String, Arc<gpui::RenderImage>>> = RefCell::new(HashMap::new());
}

pub fn set_renderer(renderer: gpui::SvgRenderer) {
    SVG_RENDERER.with(|slot| *slot.borrow_mut() = Some(renderer));
}

/// The SVG with `currentColor` replaced, rasterized at GPUI's smooth scale; cached and bounded.
pub fn image(svg: &str, color: &str) -> Option<Arc<gpui::RenderImage>> {
    let key = format!("{color}\u{1}{svg}");
    if let Some(hit) = ICONS.with(|cache| cache.borrow().get(&key).cloned()) { return Some(hit); }
    let renderer = SVG_RENDERER.with(|slot| slot.borrow().clone())?;
    let bytes = svg.replace("currentColor", color).into_bytes();
    let image = gpui::Image::from_bytes(gpui::ImageFormat::Svg, bytes).to_image_data(renderer).ok()?;
    ICONS.with(|cache| {
        let mut cache = cache.borrow_mut();
        if cache.len() >= 64 { cache.clear(); }
        cache.insert(key, image.clone());
    });
    Some(image)
}

pub fn paint(window: &mut gpui::Window, image: &Arc<gpui::RenderImage>, bounds: gpui::Bounds<gpui::Pixels>) {
    let _ = window.paint_image(bounds, bounds, gpui::Corners::default(), image.clone(), 0, false);
}
