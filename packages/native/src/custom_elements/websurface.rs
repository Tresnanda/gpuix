//! `<websurface doc lib slot>` (Kybern, WP10 Linux): paints an embedded web document that Kybern's web view companion
//! (`apps/gpuix/native/webview-linux.c`, `libkybern-webview.so`) renders offscreen with WPE WebKit, and forwards
//! pointer, wheel and key input to it.
//!
//! The page is a texture in GPUI's scene (`Window::paint_surface`), so it sits in the element tree like an image:
//! popups and dialogs drawn later cover it, ancestors' rounded clips and opacity apply, and hit-testing is GPUI's.
//! The companion is the library the app already loaded through `bun:ffi`; `lib` is its path, opened again here with
//! `RTLD_NOLOAD` so both sides share one instance. `slot` names the placeholder this element fills: a document is
//! painted only by the element whose slot matches its last `place` (a preview has a dock and a floating slot).
//!
//! Off Linux the element is an empty box: macOS uses native web views (`webview.m`).

use super::{CustomElement, CustomElementFactory, CustomRenderContext};

pub struct WebSurfaceFactory;

impl CustomElementFactory for WebSurfaceFactory {
    fn element_type(&self) -> &str {
        "websurface"
    }

    fn create(&self, id: u64) -> Box<dyn CustomElement> {
        Box::new(WebSurfaceElement {
            id,
            doc: String::new(),
            lib: String::new(),
            slot: String::new(),
            #[cfg(any(target_os = "linux", target_os = "freebsd"))]
            state: None,
        })
    }
}

pub struct WebSurfaceElement {
    id: u64,
    doc: String,
    lib: String,
    slot: String,
    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    state: Option<std::rc::Rc<std::cell::RefCell<linux::Surface>>>,
}

impl CustomElement for WebSurfaceElement {
    fn render(
        &mut self,
        ctx: CustomRenderContext,
        window: &mut gpui::Window,
        cx: &mut gpui::Context<crate::renderer::GpuixView>,
    ) -> gpui::AnyElement {
        use gpui::prelude::*;
        let root = super::custom_surface(
            gpui::div().id(gpui::SharedString::from(format!("__gpuix_websurface_{}", ctx.id))),
            &ctx,
        );
        #[cfg(any(target_os = "linux", target_os = "freebsd"))]
        {
            let state = self
                .state
                .get_or_insert_with(|| linux::Surface::new(self.id, window, cx))
                .clone();
            state.borrow_mut().configure(&self.doc, &self.lib, &self.slot);
            return linux::render(root, state, window, cx);
        }
        #[allow(unreachable_code)]
        {
            let _ = (window, cx);
            root.into_any_element()
        }
    }

    fn set_prop(&mut self, key: &str, value: serde_json::Value) {
        let text = value.as_str().unwrap_or_default().to_string();
        match key {
            "doc" => self.doc = text,
            "lib" => self.lib = text,
            "slot" => self.slot = text,
            _ => {}
        }
    }

    fn supported_props(&self) -> &'static [&'static str] {
        &["doc", "lib", "slot"]
    }

    fn supported_events(&self) -> &'static [&'static str] {
        &[]
    }

    fn destroy(&mut self) {
        #[cfg(any(target_os = "linux", target_os = "freebsd"))]
        if let Some(state) = self.state.take() {
            state.borrow_mut().release();
        }
    }
}

#[cfg(any(target_os = "linux", target_os = "freebsd"))]
mod linux {
    use std::cell::{Cell, RefCell};
    use std::collections::HashMap;
    use std::ffi::{CStr, CString, c_char, c_int, c_void};
    use std::rc::{Rc, Weak};
    use std::sync::{Arc, Mutex, OnceLock};
    use std::time::Instant;

    use futures::StreamExt;
    use futures::channel::mpsc;
    use gpui::prelude::*;
    use gpui::{
        Bounds, CursorStyle, DevicePixels, FocusHandle, Modifiers, MouseButton, Pixels, Point, ScrollDelta, point,
        px, size,
    };

    // ── The companion's C API (keep in step with webview-linux.c) ─────────────────────────────────────────────────

    #[repr(C)]
    struct Frame {
        width: u32,
        height: u32,
        stride: u32,
        pixels: *const u8,
        serial: u64,
        dirty_x: i32,
        dirty_y: i32,
        dirty_width: i32,
        dirty_height: i32,
        frame_x: f64,
        frame_y: f64,
        frame_width: f64,
        frame_height: f64,
        visible: i32,
        slot: *const c_char,
    }

    #[repr(C)]
    #[derive(Default, Clone, Copy)]
    struct Input {
        kind: i32,
        x: f64,
        y: f64,
        dx: f64,
        dy: f64,
        button: u32,
        press_count: u32,
        modifiers: u32,
        keyval: u32,
        keycode: u32,
        precise: i32,
    }

    const INPUT_DOWN: i32 = 1;
    const INPUT_UP: i32 = 2;
    const INPUT_MOVE: i32 = 3;
    const INPUT_LEAVE: i32 = 4;
    const INPUT_SCROLL: i32 = 5;
    const INPUT_KEY_DOWN: i32 = 6;
    const INPUT_KEY_UP: i32 = 7;
    const INPUT_FOCUS: i32 = 8;
    const INPUT_BLUR: i32 = 9;
    const INPUT_SCALE: i32 = 10;
    const INPUT_OUTSIDE_PRESS: i32 = 11;

    const NOTIFY_FRAME: i32 = 1;
    const NOTIFY_CURSOR: i32 = 2;
    const NOTIFY_BLUR: i32 = 3;
    const NOTIFY_FOCUS: i32 = 4;
    const NOTIFY_GONE: i32 = 5;
    const NOTIFY_PRESENT: i32 = 6;

    type Notify = extern "C" fn(*mut c_void, *const c_char, i32, *const c_char);

    struct Api {
        lock: unsafe extern "C" fn(*const c_char, *mut Frame) -> i32,
        unlock: unsafe extern "C" fn(*const c_char, i32),
        input: unsafe extern "C" fn(*const c_char, *const Input),
        listen: unsafe extern "C" fn(Notify, *mut c_void),
    }

    unsafe extern "C" {
        fn dlopen(filename: *const c_char, flags: c_int) -> *mut c_void;
        fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
    }
    const RTLD_NOW: c_int = 2;
    const RTLD_NOLOAD: c_int = 4;

    static API: OnceLock<Option<Api>> = OnceLock::new();

    fn api(lib: &str) -> Option<&'static Api> {
        API.get_or_init(|| {
            let path = CString::new(lib).ok()?;
            // SAFETY: dlopen/dlsym with valid C strings; the symbols have the signatures webview-linux.c exports.
            unsafe {
                let mut handle = dlopen(path.as_ptr(), RTLD_NOW | RTLD_NOLOAD);
                if handle.is_null() {
                    handle = dlopen(path.as_ptr(), RTLD_NOW);
                }
                if handle.is_null() {
                    log::warn!("websurface: cannot open {lib}");
                    return None;
                }
                let symbol = |name: &CStr| {
                    let address = dlsym(handle, name.as_ptr());
                    (!address.is_null()).then_some(address)
                };
                let api = Api {
                    lock: std::mem::transmute(symbol(c"kybern_web_surface_lock")?),
                    unlock: std::mem::transmute(symbol(c"kybern_web_surface_unlock")?),
                    input: std::mem::transmute(symbol(c"kybern_web_surface_input")?),
                    listen: std::mem::transmute(symbol(c"kybern_web_surface_listen")?),
                };
                (api.listen)(on_notify, std::ptr::null_mut());
                Some(api)
            }
        })
        .as_ref()
    }

    // ── Notifications from the WebKit thread to the UI thread ──────────────────────────────────────────────────────

    type Message = (String, i32, Option<String>);
    static SENDER: Mutex<Option<mpsc::UnboundedSender<Message>>> = Mutex::new(None);

    extern "C" fn on_notify(_context: *mut c_void, doc: *const c_char, kind: i32, arg: *const c_char) {
        if doc.is_null() {
            return;
        }
        // SAFETY: the companion passes NUL-terminated strings that live for the call.
        let doc = unsafe { CStr::from_ptr(doc) }.to_string_lossy().into_owned();
        let arg = (!arg.is_null()).then(|| unsafe { CStr::from_ptr(arg) }.to_string_lossy().into_owned());
        if let Ok(sender) = SENDER.lock() {
            if let Some(sender) = sender.as_ref() {
                sender.unbounded_send((doc, kind, arg)).ok();
            }
        }
    }

    thread_local! {
        /// Elements on screen per document (UI thread).
        static SURFACES: RefCell<HashMap<String, Vec<Weak<RefCell<Surface>>>>> = RefCell::new(HashMap::new());
        static PUMP: RefCell<Option<gpui::Task<()>>> = const { RefCell::new(None) };
    }

    /// One task per process drains notifications on the UI thread: a new frame repaints the elements showing that
    /// document, a cursor change is stored, and focus requests move GPUI's focus.
    fn ensure_pump(window: &mut gpui::Window, cx: &mut gpui::Context<crate::renderer::GpuixView>) {
        if PUMP.with(|pump| pump.borrow().is_some()) {
            return;
        }
        let (sender, mut receiver) = mpsc::unbounded::<Message>();
        if let Ok(mut slot) = SENDER.lock() {
            *slot = Some(sender);
        }
        let task = cx.spawn_in(window, async move |view, cx| {
            while let Some(first) = receiver.next().await {
                let mut batch = vec![first];
                while let Ok(next) = receiver.try_recv() {
                    batch.push(next);
                }
                let result = view.update_in(cx, |view, window, cx| {
                    let mut changed = Vec::new();
                    for (doc, kind, arg) in batch {
                        for surface in surfaces_for(&doc) {
                            let mut surface = surface.borrow_mut();
                            match kind {
                                NOTIFY_CURSOR => surface.cursor = cursor_style(arg.as_deref().unwrap_or("default")),
                                NOTIFY_BLUR => {
                                    if surface.focus.is_focused(window) {
                                        window.blur();
                                    }
                                }
                                NOTIFY_FOCUS => window.focus(&surface.focus, cx),
                                NOTIFY_PRESENT => surface.present.start(arg.as_deref().unwrap_or("{}")),
                                NOTIFY_GONE => surface.texture = None,
                                NOTIFY_FRAME | _ => {}
                            }
                            changed.push(surface.element);
                        }
                    }
                    if let Ok(mut tree) = view.tree.lock() {
                        for id in &changed {
                            tree.mark_paint_fallback(*id);
                        }
                    }
                    if !changed.is_empty() {
                        crate::renderer::invalidate_islands(view, window, cx);
                        if debug() { eprintln!("[websurface] wake {} element(s)", changed.len()); }
                    }
                });
                if result.is_err() {
                    break;
                }
            }
        });
        PUMP.with(|pump| *pump.borrow_mut() = Some(task));
    }

    fn surfaces_for(doc: &str) -> Vec<Rc<RefCell<Surface>>> {
        SURFACES.with(|surfaces| {
            surfaces
                .borrow()
                .get(doc)
                .map(|list| list.iter().filter_map(Weak::upgrade).collect())
                .unwrap_or_default()
        })
    }

    // ── Present (fade and scale with the host, `webHost.present`) ─────────────────────────────────────────────────

    #[derive(Clone)]
    pub(super) struct Present {
        from_opacity: f32,
        to_opacity: f32,
        from_scale: f32,
        to_scale: f32,
        origin: (f32, f32),
        ease: [f32; 4],
        duration: f32,
        started: Option<Instant>,
    }

    impl Default for Present {
        fn default() -> Self {
            Self { from_opacity: 1., to_opacity: 1., from_scale: 1., to_scale: 1., origin: (0.5, 0.5), ease: [0.23, 1., 0.32, 1.], duration: 0., started: None }
        }
    }

    impl Present {
        fn start(&mut self, json: &str) {
            let value: serde_json::Value = serde_json::from_str(json).unwrap_or_default();
            let number = |key: &str| value.get(key).and_then(|v| v.as_f64()).map(|v| v as f32);
            let (opacity, scale) = self.current();
            self.from_opacity = number("fromOpacity").unwrap_or(opacity);
            self.from_scale = number("fromScale").unwrap_or(scale);
            self.to_opacity = number("opacity").unwrap_or(1.);
            self.to_scale = number("scale").unwrap_or(1.);
            self.origin = (number("originX").unwrap_or(0.5), number("originY").unwrap_or(0.5));
            self.duration = number("duration").unwrap_or(0.).max(0.);
            if let Some(ease) = value.get("ease").and_then(|v| v.as_array()).filter(|v| v.len() == 4) {
                for (index, item) in ease.iter().enumerate() {
                    self.ease[index] = item.as_f64().unwrap_or(0.) as f32;
                }
            }
            self.started = Some(Instant::now());
        }

        fn progress(&self) -> f32 {
            match self.started {
                Some(started) if self.duration > 0. => (started.elapsed().as_secs_f32() / self.duration).min(1.),
                _ => 1.,
            }
        }

        fn animating(&self) -> bool {
            self.progress() < 1.
        }

        fn current(&self) -> (f32, f32) {
            let t = cubic_bezier(self.ease, self.progress());
            (
                self.from_opacity + (self.to_opacity - self.from_opacity) * t,
                self.from_scale + (self.to_scale - self.from_scale) * t,
            )
        }
    }

    /// CSS cubic-bezier(x1, y1, x2, y2) at time `x`.
    fn cubic_bezier([x1, y1, x2, y2]: [f32; 4], x: f32) -> f32 {
        if x <= 0. || x >= 1. {
            return x.clamp(0., 1.);
        }
        let sample = |a: f32, b: f32, t: f32| 3. * a * t * (1. - t) * (1. - t) + 3. * b * t * t * (1. - t) + t * t * t;
        let (mut low, mut high) = (0f32, 1f32);
        for _ in 0..24 {
            let middle = (low + high) / 2.;
            if sample(x1, x2, middle) < x {
                low = middle;
            } else {
                high = middle;
            }
        }
        sample(y1, y2, (low + high) / 2.)
    }

    // ── The element ───────────────────────────────────────────────────────────────────────────────────────────────

    pub(super) struct Surface {
        element: u64,
        doc: String,
        doc_c: CString,
        lib: String,
        slot: String,
        focus: FocusHandle,
        cursor: CursorStyle,
        texture: Option<(Arc<wgpu::Texture>, u32, u32)>,
        serial: u64,
        scale_sent: f32,
        /// The page's top-left in window pixels and its size on screen, from the last paint (input mapping).
        page: Cell<Bounds<Pixels>>,
        /// Logical page width over on-screen width (present scale).
        page_scale: Cell<f32>,
        present: Present,
        buttons: u32,
    }

    impl Surface {
        pub(super) fn new(
            element: u64,
            window: &mut gpui::Window,
            cx: &mut gpui::Context<crate::renderer::GpuixView>,
        ) -> Rc<RefCell<Self>> {
            ensure_pump(window, cx);
            Rc::new(RefCell::new(Self {
                element,
                doc: String::new(),
                doc_c: CString::default(),
                lib: String::new(),
                slot: String::new(),
                focus: cx.focus_handle(),
                cursor: CursorStyle::Arrow,
                texture: None,
                serial: 0,
                scale_sent: 0.,
                page: Cell::new(Bounds::default()),
                page_scale: Cell::new(1.),
                present: Present::default(),
                buttons: 0,
            }))
        }

        pub(super) fn configure(&mut self, doc: &str, lib: &str, slot: &str) {
            if self.doc != doc {
                self.doc = doc.to_string();
                self.doc_c = CString::new(doc).unwrap_or_default();
                self.texture = None;
                self.serial = 0;
                self.scale_sent = 0.;
            }
            self.lib = lib.to_string();
            self.slot = slot.to_string();
        }

        pub(super) fn release(&mut self) {
            self.texture = None;
        }

        fn send(&self, input: Input) {
            if let Some(api) = api(&self.lib) {
                // SAFETY: valid C string and a live Input; the companion copies both.
                unsafe { (api.input)(self.doc_c.as_ptr(), &input) };
            }
        }

        fn local(&self, position: Point<Pixels>) -> (f64, f64) {
            let page = self.page.get();
            let scale = self.page_scale.get();
            (
                f64::from(f32::from(position.x - page.origin.x) * scale),
                f64::from(f32::from(position.y - page.origin.y) * scale),
            )
        }

        /// Uploads the companion's latest frame and paints it at `bounds` (the element's box). Returns whether the
        /// document had anything to show.
        fn paint(&mut self, bounds: Bounds<Pixels>, window: &mut gpui::Window) -> bool {
            let Some(api) = api(&self.lib) else { return false };
            let scale_factor = window.scale_factor();
            if (scale_factor - self.scale_sent).abs() > f32::EPSILON {
                self.scale_sent = scale_factor;
                self.send(Input { kind: INPUT_SCALE, dx: f64::from(scale_factor), ..Default::default() });
            }
            let mut frame = std::mem::MaybeUninit::<Frame>::zeroed();
            // SAFETY: `lock` fills `frame` and keeps the companion's frame lock until `unlock`.
            if unsafe { (api.lock)(self.doc_c.as_ptr(), frame.as_mut_ptr()) } == 0 {
                return false;
            }
            let frame = unsafe { frame.assume_init() };
            let slot = if frame.slot.is_null() { "" } else { unsafe { CStr::from_ptr(frame.slot) }.to_str().unwrap_or("") };
            let shown = frame.visible != 0 && slot == self.slot && frame.width > 0 && frame.height > 0 && !frame.pixels.is_null();
            let mut consumed = false;
            if shown {
                consumed = self.upload(&frame, window);
            }
            // SAFETY: pairs the successful `lock` above.
            unsafe { (api.unlock)(self.doc_c.as_ptr(), i32::from(consumed)) };
            let Some((texture, width, height)) = self.texture.clone().filter(|_| shown) else {
                return false;
            };
            let (_, scale) = self.present.current();
            let full = Bounds::new(
                point(bounds.origin.x + px(frame.frame_x as f32), bounds.origin.y + px(frame.frame_y as f32)),
                size(px(frame.frame_width.max(1.) as f32), px(frame.frame_height.max(1.) as f32)),
            );
            let origin = point(
                full.origin.x + full.size.width * self.present.origin.0 * (1. - scale),
                full.origin.y + full.size.height * self.present.origin.1 * (1. - scale),
            );
            let page = Bounds::new(origin, size(full.size.width * scale, full.size.height * scale));
            self.page.set(page);
            self.page_scale.set(if scale > 0. { 1. / scale } else { 1. });
            window.paint_surface(page, texture, size(DevicePixels(width as i32), DevicePixels(height as i32)));
            true
        }

        fn upload(&mut self, frame: &Frame, window: &mut gpui::Window) -> bool {
            let Some(context) = window.gpu_context() else { return false };
            let Some((device, queue)) = context.downcast_ref::<(Arc<wgpu::Device>, Arc<wgpu::Queue>)>() else {
                return false;
            };
            let resized = !matches!(&self.texture, Some((_, w, h)) if *w == frame.width && *h == frame.height);
            if !resized && frame.serial == self.serial {
                return false;
            }
            if resized {
                let texture = device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("kybern_websurface"),
                    size: wgpu::Extent3d { width: frame.width, height: frame.height, depth_or_array_layers: 1 },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: wgpu::TextureFormat::Bgra8Unorm,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                    view_formats: &[],
                });
                self.texture = Some((Arc::new(texture), frame.width, frame.height));
            }
            let (x, y, width, height) = if resized || frame.dirty_width <= 0 || frame.dirty_height <= 0 {
                (0, 0, frame.width, frame.height)
            } else {
                (frame.dirty_x as u32, frame.dirty_y as u32, frame.dirty_width as u32, frame.dirty_height as u32)
            };
            let width = width.min(frame.width.saturating_sub(x));
            let height = height.min(frame.height.saturating_sub(y));
            if let (Some((texture, _, _)), true) = (&self.texture, width > 0 && height > 0) {
                let start = y as usize * frame.stride as usize + x as usize * 4;
                let length = (height as usize - 1) * frame.stride as usize + width as usize * 4;
                // SAFETY: the companion's buffer is `stride * height` bytes and stays locked during this call.
                let data = unsafe { std::slice::from_raw_parts(frame.pixels.add(start), length) };
                queue.write_texture(
                    wgpu::TexelCopyTextureInfo { texture, mip_level: 0, origin: wgpu::Origin3d { x, y, z: 0 }, aspect: wgpu::TextureAspect::All },
                    data,
                    wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(frame.stride), rows_per_image: Some(height) },
                    wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
                );
            }
            if debug() { eprintln!("[websurface] upload {}x{} at {x},{y} {width}x{height} serial {}", frame.width, frame.height, frame.serial); }
            self.serial = frame.serial;
            true
        }
    }

    fn debug() -> bool {
        static DEBUG: OnceLock<bool> = OnceLock::new();
        *DEBUG.get_or_init(|| std::env::var_os("KYBERN_DEBUG_WEBSURFACE").is_some())
    }

    fn modifier_bits(modifiers: &Modifiers) -> u32 {
        u32::from(modifiers.shift) | u32::from(modifiers.control) << 1 | u32::from(modifiers.alt) << 2 | u32::from(modifiers.platform) << 3
    }

    fn button_number(button: MouseButton) -> u32 {
        match button {
            MouseButton::Left => 1,
            MouseButton::Middle => 2,
            MouseButton::Right => 3,
            _ => 0,
        }
    }

    /// Keys Kybern keeps as app shortcuts while a page has the keyboard (webview.m's ⌘ list; Ctrl on Linux).
    fn is_app_shortcut(keystroke: &gpui::Keystroke) -> bool {
        let modifiers = &keystroke.modifiers;
        if !(modifiers.control || modifiers.platform) {
            return false;
        }
        if modifiers.alt {
            return keystroke.key == "p";
        }
        matches!(keystroke.key.as_str(), "l" | "r" | "n" | "b" | "k" | "j" | "[" | "]" | "up" | "," | "\\")
    }

    /// X keysym for a GPUI keystroke (WPE's keymap turns it into the DOM key and text).
    fn keysym(keystroke: &gpui::Keystroke) -> u32 {
        let named = match keystroke.key.as_str() {
            "enter" => 0xff0d,
            "escape" => 0xff1b,
            "backspace" => 0xff08,
            "tab" => if keystroke.modifiers.shift { 0xfe20 } else { 0xff09 },
            "space" => 0x20,
            "left" => 0xff51,
            "up" => 0xff52,
            "right" => 0xff53,
            "down" => 0xff54,
            "home" => 0xff50,
            "end" => 0xff57,
            "pageup" => 0xff55,
            "pagedown" => 0xff56,
            "delete" => 0xffff,
            "insert" => 0xff63,
            "shift" => 0xffe1,
            "control" => 0xffe3,
            "alt" => 0xffe9,
            "platform" => 0xffeb,
            key if key.len() > 1 && key.starts_with('f') => match key[1..].parse::<u32>() {
                Ok(number @ 1..=24) => 0xffbe + number - 1,
                _ => 0,
            },
            _ => 0,
        };
        if named != 0 {
            return named;
        }
        let text = keystroke.key_char.as_deref().filter(|text| text.chars().count() == 1).unwrap_or(&keystroke.key);
        let mut chars = text.chars();
        match (chars.next(), chars.next()) {
            (Some(character), None) => {
                let code = character as u32;
                if (0x20..=0x7e).contains(&code) || (0xa0..=0xff).contains(&code) { code } else { 0x0100_0000 | code }
            }
            _ => 0,
        }
    }

    fn cursor_style(name: &str) -> CursorStyle {
        match name {
            "pointer" | "hand" => CursorStyle::PointingHand,
            "text" => CursorStyle::IBeam,
            "vertical-text" => CursorStyle::IBeamCursorForVerticalLayout,
            "crosshair" => CursorStyle::Crosshair,
            "grab" => CursorStyle::OpenHand,
            "grabbing" => CursorStyle::ClosedHand,
            "not-allowed" | "no-drop" => CursorStyle::OperationNotAllowed,
            "ew-resize" => CursorStyle::ResizeLeftRight,
            "ns-resize" => CursorStyle::ResizeUpDown,
            "e-resize" => CursorStyle::ResizeRight,
            "w-resize" => CursorStyle::ResizeLeft,
            "n-resize" => CursorStyle::ResizeUp,
            "s-resize" => CursorStyle::ResizeDown,
            "nesw-resize" | "ne-resize" | "sw-resize" => CursorStyle::ResizeUpRightDownLeft,
            "nwse-resize" | "nw-resize" | "se-resize" => CursorStyle::ResizeUpLeftDownRight,
            "col-resize" => CursorStyle::ResizeColumn,
            "row-resize" => CursorStyle::ResizeRow,
            "context-menu" => CursorStyle::ContextualMenu,
            "copy" => CursorStyle::DragCopy,
            "alias" => CursorStyle::DragLink,
            _ => CursorStyle::Arrow,
        }
    }

    pub(super) fn render(
        root: gpui::Stateful<gpui::Div>,
        state: Rc<RefCell<Surface>>,
        window: &mut gpui::Window,
        _cx: &mut gpui::Context<crate::renderer::GpuixView>,
    ) -> gpui::AnyElement {
        let (doc, focus, cursor, opacity, animating) = {
            let surface = state.borrow();
            let (opacity, _) = surface.present.current();
            (surface.doc.clone(), surface.focus.clone(), surface.cursor.clone(), opacity, surface.present.animating())
        };
        SURFACES.with(|surfaces| {
            let mut surfaces = surfaces.borrow_mut();
            let list = surfaces.entry(doc).or_default();
            list.retain(|weak| weak.strong_count() > 0);
            if !list.iter().any(|weak| weak.as_ptr() == Rc::as_ptr(&state)) {
                list.push(Rc::downgrade(&state));
            }
        });
        if animating {
            window.request_animation_frame();
        }

        let paint_state = state.clone();
        let canvas = gpui::canvas(
            |_, _, _| {},
            move |bounds, _, window, _| {
                paint_state.borrow_mut().paint(bounds, window);
            },
        )
        .size_full();

        let down = state.clone();
        let up = state.clone();
        let moved = state.clone();
        let hover = state.clone();
        let wheel = state.clone();
        let key_down = state.clone();
        let key_up = state.clone();
        let outside = state.clone();
        let blurred = state.clone();
        let mut root = root
            .track_focus(&focus)
            .cursor(cursor)
            .opacity(opacity)
            .on_any_mouse_down(move |event, window, cx| {
                let mut surface = down.borrow_mut();
                let (x, y) = surface.local(event.position);
                let button = button_number(event.button);
                surface.buttons |= 1 << button;
                if !surface.focus.is_focused(window) {
                    window.focus(&surface.focus, cx);
                }
                surface.send(Input { kind: INPUT_DOWN, x, y, button, press_count: event.click_count as u32, modifiers: modifier_bits(&event.modifiers), ..Default::default() });
                cx.stop_propagation();
            })
            .on_mouse_down_out(move |event, _window, _cx| {
                let surface = outside.borrow();
                surface.send(Input {
                    kind: INPUT_OUTSIDE_PRESS,
                    x: f64::from(f32::from(event.position.x)),
                    y: f64::from(f32::from(event.position.y)),
                    ..Default::default()
                });
            })
            .on_mouse_move(move |event, _window, _cx| {
                let surface = moved.borrow();
                let (x, y) = surface.local(event.position);
                surface.send(Input { kind: INPUT_MOVE, x, y, modifiers: modifier_bits(&event.modifiers), ..Default::default() });
            })
            .on_hover(move |hovered, _window, _cx| {
                if !*hovered {
                    hover.borrow().send(Input { kind: INPUT_LEAVE, ..Default::default() });
                }
            })
            .on_scroll_wheel(move |event, window, cx| {
                let surface = wheel.borrow();
                let (x, y) = surface.local(event.position);
                let (dx, dy, precise) = match event.delta {
                    ScrollDelta::Pixels(delta) => (f32::from(delta.x), f32::from(delta.y), 1),
                    ScrollDelta::Lines(delta) => (delta.x * 40., delta.y * 40., 0),
                };
                let _ = window;
                surface.send(Input { kind: INPUT_SCROLL, x, y, dx: f64::from(dx), dy: f64::from(dy), precise, modifiers: modifier_bits(&event.modifiers), ..Default::default() });
                cx.stop_propagation();
            })
            .on_key_down(move |event, _window, cx| {
                if is_app_shortcut(&event.keystroke) {
                    return;
                }
                let surface = key_down.borrow();
                surface.send(Input { kind: INPUT_KEY_DOWN, keyval: keysym(&event.keystroke), modifiers: modifier_bits(&event.keystroke.modifiers), ..Default::default() });
                cx.stop_propagation();
            })
            .on_key_up(move |event, _window, cx| {
                let surface = key_up.borrow();
                surface.send(Input { kind: INPUT_KEY_UP, keyval: keysym(&event.keystroke), modifiers: modifier_bits(&event.keystroke.modifiers), ..Default::default() });
                cx.stop_propagation();
            });
        for button in [MouseButton::Left, MouseButton::Middle, MouseButton::Right] {
            let up = up.clone();
            root = root.on_mouse_up(button, move |event, _window, cx| {
                let mut surface = up.borrow_mut();
                let (x, y) = surface.local(event.position);
                let number = button_number(event.button);
                surface.buttons &= !(1 << number);
                surface.send(Input { kind: INPUT_UP, x, y, button: number, modifiers: modifier_bits(&event.modifiers), ..Default::default() });
                cx.stop_propagation();
            });
        }
        // The page keeps the keyboard until GPUI moves focus (a click elsewhere, Escape the page left unhandled).
        let focused = focus.is_focused(window);
        let surface = blurred.borrow();
        if !focused && surface.buttons == 0 {
            surface.send(Input { kind: INPUT_BLUR, ..Default::default() });
        } else if focused {
            surface.send(Input { kind: INPUT_FOCUS, ..Default::default() });
        }
        drop(surface);
        root.child(canvas).into_any_element()
    }
}
