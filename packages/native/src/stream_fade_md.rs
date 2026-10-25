// Included in markdown/render.rs (stream_fade_hooks.py): the live answer's per-word fade as paint-only
// scene work (gpui stream_fade.rs) instead of re-rendering the Markdown with faded run colours at every
// frame. `KYBERN_STREAM_FADE=render` keeps the previous run-colour path for measurements.
fn stream_fade(flat: &mut FlatText, born: &[f64], fade_ms: f64) -> bool {
    static RENDER: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if *RENDER.get_or_init(|| std::env::var("KYBERN_STREAM_FADE").as_deref() == Ok("render")) {
        return apply_fade(flat, born, fade_ms);
    }
    let text = flat.text.as_ref();
    let mut words: Vec<Range<usize>> = Vec::new();
    let mut start: Option<usize> = None;
    for (index, ch) in text.char_indices() {
        if ch.is_whitespace() { if let Some(s) = start.take() { words.push(s..index); } } else if start.is_none() { start = Some(index); }
    }
    if let Some(s) = start { words.push(s..text.len()); }
    let now = epoch_ms();
    flat.fades.clear();
    for (n, born_at) in born.iter().rev().enumerate() {
        let Some(word) = words.len().checked_sub(n + 1).and_then(|i| words.get(i)) else { break };
        if now - born_at >= fade_ms { continue; }
        flat.fades.push((word.clone(), *born_at, fade_ms as f32));
    }
    false
}

/// Hand the fading words of one shaped run (ranges relative to its layout) to the scene.
fn push_fade_words(layout: &gpui::TextLayout, window: &mut Window, fades: &[(Range<usize>, f64, f32)]) {
    if fades.is_empty() { return; }
    let now = epoch_ms();
    let instant = std::time::Instant::now();
    let mut words = Vec::new();
    for (range, born, fade_ms) in fades {
        let born = instant.checked_sub(std::time::Duration::from_secs_f64(((now - born) / 1000.0).max(0.0))).unwrap_or(instant);
        for rect in range_rects(layout, range, 0.0, 0.0) {
            words.push((rect, born, *fade_ms));
        }
    }
    window.push_fade_words(&words);
}

/// Ends the fade scope of the final paragraph after its glyphs are painted, and adds the blur.
struct FadeEnd { child: AnyElement }
impl IntoElement for FadeEnd { type Element = Self; fn into_element(self) -> Self { self } }
impl gpui::Element for FadeEnd {
    type RequestLayoutState = ();
    type PrepaintState = ();
    fn id(&self) -> Option<gpui::ElementId> { None }
    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> { None }
    fn request_layout(&mut self, _: Option<&gpui::GlobalElementId>, _: Option<&gpui::InspectorElementId>, window: &mut Window, cx: &mut gpui::App) -> (gpui::LayoutId, ()) {
        (self.child.request_layout(window, cx), ())
    }
    fn prepaint(&mut self, _: Option<&gpui::GlobalElementId>, _: Option<&gpui::InspectorElementId>, _: Bounds<Pixels>, _: &mut (), window: &mut Window, cx: &mut gpui::App) {
        self.child.prepaint(window, cx);
    }
    fn paint(&mut self, _: Option<&gpui::GlobalElementId>, _: Option<&gpui::InspectorElementId>, _: Bounds<Pixels>, _: &mut (), _: &mut (), window: &mut Window, cx: &mut gpui::App) {
        self.child.paint(window, cx);
        // `--stream-blur`: 1px at birth.
        window.end_fade_words(px(1.0));
    }
}
