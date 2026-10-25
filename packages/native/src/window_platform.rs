//! Window and shell services the desktop gets from Tauri: the window backing
//! switched at runtime (vibrancy on/off), the save panel, and an image on the
//! system pasteboard. The napi methods live in `renderer.rs` (inserted by
//! `window_platform_hooks.py`) because they use its private window helpers.
use napi::bindgen_prelude::*;

/// `"opaque"` | `"blurred"` | `"transparent"`, the same names as `WindowOptions.windowBackground`.
pub(crate) fn background(mode: &str) -> Result<gpui::WindowBackgroundAppearance> {
    match mode {
        "opaque" => Ok(gpui::WindowBackgroundAppearance::Opaque),
        "blurred" => Ok(gpui::WindowBackgroundAppearance::Blurred),
        "transparent" => Ok(gpui::WindowBackgroundAppearance::Transparent),
        other => Err(Error::from_reason(format!("Unknown window background {other:?}; use opaque, blurred or transparent"))),
    }
}

/// An image clipboard entry. The media type must be one GPUI knows (PNG, JPEG, WebP, GIF, SVG, BMP, TIFF, ICO).
pub(crate) fn clipboard_image(data: &[u8], media_type: &str) -> Result<gpui::ClipboardItem> {
    if data.is_empty() {
        return Err(Error::from_reason("The image is empty"));
    }
    let format = gpui::ImageFormat::from_mime_type(media_type)
        .ok_or_else(|| Error::from_reason(format!("Images of type {media_type} cannot be copied")))?;
    Ok(gpui::ClipboardItem::new_image(&gpui::Image::from_bytes(format, data.to_vec())))
}
