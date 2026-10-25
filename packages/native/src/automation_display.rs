//! Automated runs (`GPUIX_BACKGROUND=1`) open their windows on the hidden test
//! display when it is up (its id is in `/tmp/kybern-gpuix-test-display`, written
//! by the `kybern-test-display` helper), else on the Mac's built-in display.
//! GPUI otherwise centers a new window on the primary display, which follows
//! the screen the user is focused on, so checks and benchmarks would open in
//! front of the user's work. `KYBERN_TEST_DISPLAY=any` opts out. Without either
//! display (lid closed, desktop Mac) nothing changes.

#[cfg(target_os = "macos")]
#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGGetActiveDisplayList(max: u32, displays: *mut u32, count: *mut u32) -> i32;
    fn CGDisplayIsBuiltin(display: u32) -> u32;
    fn CGDisplayBounds(display: u32) -> CGRect;
}

#[cfg(target_os = "macos")]
#[repr(C)]
#[derive(Clone, Copy)]
#[allow(dead_code)]
struct CGRect {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

/// The display for an automated run, else `None` (GPUI's default).
pub(crate) fn display() -> Option<gpui::DisplayId> {
    if std::env::var("GPUIX_BACKGROUND").as_deref() != Ok("1")
        || std::env::var("KYBERN_TEST_DISPLAY").as_deref() == Ok("any")
    {
        return None;
    }
    #[cfg(target_os = "macos")]
    unsafe {
        let mut ids = [0u32; 16];
        let mut count = 0u32;
        if CGGetActiveDisplayList(ids.len() as u32, ids.as_mut_ptr(), &mut count) != 0 {
            return None;
        }
        let active = &ids[..count as usize];
        let test = std::fs::read_to_string("/tmp/kybern-gpuix-test-display").ok().and_then(|text| text.trim().parse::<u32>().ok());
        return test
            .filter(|id| active.contains(id))
            .or_else(|| active.iter().copied().find(|&id| CGDisplayIsBuiltin(id) != 0))
            .map(|id| gpui::DisplayId::new(id as u64));
    }
    #[allow(unreachable_code)]
    None
}

/// `Bounds::centered(None, ..)`, but on the automation display when there is
/// one. The platform window places these bounds relative to that display, in
/// points (CGDisplayBounds; GPUI's own display size is not in points here).
pub(crate) fn centered(size: gpui::Size<gpui::Pixels>, cx: &gpui::App) -> gpui::Bounds<gpui::Pixels> {
    #[cfg(target_os = "macos")]
    if let Some(id) = display() {
        let area = unsafe { CGDisplayBounds(u64::from(id) as u32) };
        let inset = |outer: f64, inner: gpui::Pixels| gpui::px(((outer as f32 - inner.as_f32()) / 2.0).max(0.0));
        return gpui::Bounds { origin: gpui::point(inset(area.width, size.width), inset(area.height, size.height)), size };
    }
    gpui::Bounds::centered(None, size, cx)
}
