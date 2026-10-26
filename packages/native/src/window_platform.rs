//! Window and shell services the desktop gets from Tauri: the window backing
//! switched at runtime (vibrancy on/off), the save panel, and an image on the
//! system pasteboard. The napi methods live in `renderer.rs` (inserted by
//! `window_platform_hooks.py`) because they use its private window helpers.
use napi::bindgen_prelude::*;

/// `"opaque"` | `"blurred"` | `"transparent"`, the same names as `WindowOptions.windowBackground`; on Windows also the
/// Windows 11 system backdrops `"mica"`, `"mica-alt"` and `"acrylic"` (DWM; Windows 10 ignores them, so the caller picks
/// them by build, `src/shell/windowMaterial.ts`).
pub(crate) fn background(mode: &str) -> Result<gpui::WindowBackgroundAppearance> {
    match background_name(mode) {
        Some(appearance) => Ok(appearance),
        None => Err(Error::from_reason(format!(
            "Unknown window background {mode:?}; use opaque, blurred or transparent{}",
            if cfg!(target_os = "windows") { ", or mica, mica-alt or acrylic" } else { "" }
        ))),
    }
}

/// The names `background` and the window options accept.
pub(crate) fn background_name(mode: &str) -> Option<gpui::WindowBackgroundAppearance> {
    match mode {
        "opaque" => Some(gpui::WindowBackgroundAppearance::Opaque),
        "blurred" => Some(gpui::WindowBackgroundAppearance::Blurred),
        "transparent" => Some(gpui::WindowBackgroundAppearance::Transparent),
        // GPUI's Windows backend draws Blurred as DWM Acrylic on Windows 11 22H2+.
        "acrylic" if cfg!(target_os = "windows") => Some(gpui::WindowBackgroundAppearance::Blurred),
        "mica" if cfg!(target_os = "windows") => Some(gpui::WindowBackgroundAppearance::MicaBackdrop),
        "mica-alt" if cfg!(target_os = "windows") => Some(gpui::WindowBackgroundAppearance::MicaAltBackdrop),
        _ => None,
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
