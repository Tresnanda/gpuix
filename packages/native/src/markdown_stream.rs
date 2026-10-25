// Included into markdown/render.rs: link icons, the live-answer caret and the per-word fade.
fn epoch_ms() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64() * 1000.0).unwrap_or(0.0)
}
/// `cubic-bezier(x1, y1, x2, y2)` at progress `x` (the desktop's `--ease-smooth-out` is .22, 1, .36, 1).
fn cubic_bezier(x1: f64, y1: f64, x2: f64, y2: f64, x: f64) -> f64 {
    let at = |t: f64, a: f64, b: f64| 3.0 * a * (1.0 - t).powi(2) * t + 3.0 * b * (1.0 - t) * t * t + t.powi(3);
    let (mut lo, mut hi) = (0.0, 1.0);
    for _ in 0..24 {
        let mid = (lo + hi) / 2.0;
        if at(mid, x1, x2) < x { lo = mid } else { hi = mid }
    }
    at((lo + hi) / 2.0, y1, y2)
}
fn count_text(blocks: &[Block]) -> usize {
    blocks.iter().map(|block| match block {
        Block::Paragraph { .. } | Block::Heading { .. } => 1,
        Block::BlockQuote { children } => count_text(children),
        Block::List { items, .. } => items.iter().map(|item| count_text(item)).sum(),
        _ => 0,
    }).sum()
}
fn ends_with_text(blocks: &[Block]) -> bool {
    match blocks.last() {
        Some(Block::Paragraph { .. } | Block::Heading { .. }) => true,
        Some(Block::BlockQuote { children }) => ends_with_text(children),
        Some(Block::List { items, .. }) => items.last().is_some_and(|item| ends_with_text(item)),
        _ => false,
    }
}
/// Fade the newest words in: `born` are the birth times of the last words, oldest first.
/// Returns whether any word is still fading, so the caller keeps the frames coming.
fn apply_fade(flat: &mut FlatText, born: &[f64], fade_ms: f64) -> bool {
    let text = flat.text.as_ref();
    let mut words: Vec<Range<usize>> = Vec::new();
    let mut start: Option<usize> = None;
    for (index, ch) in text.char_indices() {
        if ch.is_whitespace() { if let Some(s) = start.take() { words.push(s..index); } } else if start.is_none() { start = Some(index); }
    }
    if let Some(s) = start { words.push(s..text.len()); }
    let now = epoch_ms();
    let mut fades: Vec<(Range<usize>, f32)> = Vec::new();
    let mut animating = false;
    for (n, born_at) in born.iter().rev().enumerate() {
        let Some(word) = words.len().checked_sub(n + 1).and_then(|i| words.get(i)) else { break };
        let age = now - born_at;
        if age >= fade_ms { continue; }
        animating = true;
        fades.push((word.clone(), cubic_bezier(0.22, 1.0, 0.36, 1.0, (age / fade_ms).clamp(0.0, 1.0)) as f32));
    }
    if fades.is_empty() { return animating; }
    fades.sort_by_key(|(range, _)| range.start);
    let mut runs: Vec<TextRun> = Vec::with_capacity(flat.runs.len() + fades.len() * 2);
    let mut offset = 0;
    for run in &flat.runs {
        let end = offset + run.len;
        let mut cursor = offset;
        for (range, alpha) in &fades {
            let a = range.start.max(cursor);
            let b = range.end.min(end);
            if a >= b { continue; }
            if a > cursor { let mut piece = run.clone(); piece.len = a - cursor; runs.push(piece); }
            let mut faded = run.clone();
            faded.len = b - a;
            faded.color.a *= *alpha;
            if let Some(underline) = faded.underline.as_mut() { if let Some(c) = underline.color.as_mut() { c.a *= *alpha; } }
            runs.push(faded);
            cursor = b;
        }
        if cursor < end { let mut piece = run.clone(); piece.len = end - cursor; runs.push(piece); }
        offset = end;
    }
    flat.runs = runs;
    animating
}
/// Link chips and the caret, painted over the shaped text: icons sit on the two placeholder
/// characters that reserve their room; the caret follows the last glyph.
fn paint_decorations(layout: &gpui::TextLayout, window: &mut Window, icons: &[(usize, Arc<gpui::RenderImage>)], span: usize, caret: Option<Hsla>, text_len: usize, size: f32) {
    for (at, image) in icons {
        if let Some(rect) = range_rects(layout, &(*at..*at + span), 0.0, 0.0).first() {
            let s = size * 0.95;
            let bounds = gpui::Bounds::new(
                gpui::point(rect.left(), rect.top() + (rect.size.height - px(s)) / 2.0 + px(size * 0.02)),
                gpui::size(px(s), px(s)),
            );
            crate::kybern_icon::paint(window, image, bounds);
        }
    }
    if let Some(color) = caret {
        if let Some(end) = layout.position_for_index(text_len) {
            let h = px(size * 0.95);
            let bounds = gpui::Bounds::new(gpui::point(end.x + px(3.0), end.y + (layout.line_height() - h) / 2.0), gpui::size(px(2.0), h));
            window.paint_quad(gpui::quad(bounds, px(1.0), color, px(0.0), gpui::transparent_black(), BorderStyle::default()));
        }
    }
}

fn stream_prepare(flat: &mut FlatText, ctx: &mut MdContext) {
    let Some(stream) = ctx.theme.markdown.stream.clone() else { return };
    if !stream.born.is_empty() && apply_fade(flat, &stream.born, stream.fade_ms.unwrap_or(350.0) as f64) { ctx.animating = true; }
    if stream.caret && stream.caret_visible {
        let mut c = color(&stream.caret_color, ctx.theme.text);
        c.a *= 0.7;
        ctx.caret = Some(c);
    }
}
