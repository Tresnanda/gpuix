// Included into input.rs. All stored offsets are UTF-8; public offsets are UTF-16.
#[derive(Clone, PartialEq)]
struct InputDecoration {
    range: Range<usize>,
    style: DecorationStyle,
    atomic: bool,
    id: Option<String>,
    icon: Option<IconDeco>,
}
/// A glyph drawn over the first two characters of a decoration (spaces in a monospace font that
/// reserve its room); the icon sits at the range start, vertically centred in the line.
#[derive(Clone)]
struct IconDeco { key: String, image: std::sync::Arc<gpui::RenderImage>, font: String, size: f32 }
impl PartialEq for IconDeco { fn eq(&self, other: &Self) -> bool { self.key == other.key && self.font == other.font && self.size == other.size } }
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct IconRequest { svg: String, color: String, font: String, size: f32 }
#[derive(Clone, Default, PartialEq, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DecorationStyle {
    color: Option<String>,
    background_color: Option<String>,
    font_weight: Option<f32>,
    font_style: Option<String>,
    underline: Option<bool>,
    border_radius: Option<f32>,
    padding_x: Option<f32>,
    background_outset_x: Option<f32>,
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct DecorationRequest {
    start: u32,
    end: u32,
    #[serde(default)]
    style: DecorationStyle,
    #[serde(default)]
    atomic: bool,
    id: Option<String>,
    #[serde(default)]
    icon: Option<IconRequest>,
}

pub(crate) fn set_input_decorations(id: u64, value: serde_json::Value, expected: String, cx: &mut gpui::App) -> napi::Result<()> {
    crate::kybern_icon::set_renderer(cx.svg_renderer());
    let requests: Vec<DecorationRequest> = serde_json::from_value(value).map_err(|e| napi::Error::from_reason(e.to_string()))?;
    if requests.len() > 4096 { return Err(napi::Error::from_reason("At most 4096 decorations are supported")); }
    editor_for(id)?.update(cx, |state, cx| {
        if state.secure || state.marked_range.is_some() { return Err(napi::Error::from_reason("Input is secure or composing")); }
        if state.content != expected { return Err(napi::Error::from_reason("Input changed before decorations")); }
        let mut decorations = Vec::with_capacity(requests.len());
        let mut ids = std::collections::HashSet::new();
        for request in requests {
            let range = checked_range(&state.content, request.start, request.end)?;
            if range.is_empty() { return Err(napi::Error::from_reason("Decoration ranges must be nonempty")); }
            let style = request.style;
            for color in [&style.color, &style.background_color].into_iter().flatten() {
                if crate::color::parse_color(color).is_none() { return Err(napi::Error::from_reason("Invalid decoration color")); }
            }
            if style.font_weight.is_some_and(|v| !v.is_finite() || !(100.0..=900.0).contains(&v)) ||
                style.font_style.as_ref().is_some_and(|v| v != "normal" && v != "italic") {
                return Err(napi::Error::from_reason("Invalid decoration font style"));
            }
            if style.padding_x.is_some_and(|v| v != 0.0) {
                return Err(napi::Error::from_reason("Input runs do not support layout paddingX"));
            }
            if [style.border_radius, style.background_outset_x].into_iter().flatten().any(|v| !v.is_finite() || v < 0.0) {
                return Err(napi::Error::from_reason("Decoration radius and outset must be finite and nonnegative"));
            }
            if let Some(id) = &request.id {
                if id.is_empty() || !ids.insert(id.clone()) { return Err(napi::Error::from_reason("Decoration IDs must be unique and nonempty")); }
            }
            let icon = match request.icon {
                Some(spec) => {
                    if !spec.size.is_finite() || spec.size <= 0.0 || spec.size > 64.0 { return Err(napi::Error::from_reason("Invalid icon size")); }
                    let image = crate::kybern_icon::image(&spec.svg, &spec.color).ok_or_else(|| napi::Error::from_reason("The icon could not be drawn"))?;
                    Some(IconDeco { key: format!("{}\u{1}{}", spec.color, spec.svg), image, font: spec.font, size: spec.size })
                }
                None => None,
            };
            decorations.push(InputDecoration { range, style, atomic: request.atomic, id: request.id, icon });
        }
        let atoms: Vec<_> = decorations.iter().filter(|d| d.atomic).collect();
        for (i, atom) in atoms.iter().enumerate() {
            if atoms[..i].iter().any(|other| atom.range.start < other.range.end && other.range.start < atom.range.end) {
                return Err(napi::Error::from_reason("Atomic decoration ranges must not overlap"));
            }
        }
        if state.decorations == decorations { return Ok(()); }
        state.decorations = decorations;
        state.selected_range = state.expand_atoms(state.selected_range.clone());
        cx.notify();
        Ok(())
    })
}

pub(crate) fn hit_test_input_decoration(id: u64, x: f64, y: f64, cx: &gpui::App) -> napi::Result<Option<String>> {
    if !x.is_finite() || !y.is_finite() { return Err(napi::Error::from_reason("Point must be finite")); }
    let editor = editor_for(id)?;
    let state = editor.read(cx);
    let p = point(px(x as f32), px(y as f32));
    if state.secure || state.last_bounds.is_none_or(|b| !b.contains(&p)) { return Ok(None); }
    let index = state.index_for_mouse_position(p);
    Ok(state.decorations.iter().rev().find(|d| d.range.contains(&index) && d.id.is_some()).and_then(|d| d.id.clone()))
}

impl TextEditorState {
    fn snap_atom(&self, offset: usize, forward: bool) -> usize {
        if self.marked_range.is_some() { return offset; }
        self.decorations.iter().find(|d| d.atomic && d.range.start < offset && offset < d.range.end)
            .map(|d| if forward { d.range.end } else { d.range.start }).unwrap_or(offset)
    }
    fn expand_atoms(&self, range: Range<usize>) -> Range<usize> {
        if range.is_empty() { let at = self.snap_atom(range.start, true); return at..at; }
        self.snap_atom(range.start, false)..self.snap_atom(range.end, true)
    }
    // Decorations intersected by editing are discarded; untouched spans retain identity.
    // Insertion at either boundary stays outside the span. Undo restores the exact spans.
    fn rebase_decorations(&mut self, range: &Range<usize>, inserted: usize) {
        self.decorations.retain_mut(|d| {
            if d.range.end <= range.start { return true; }
            if d.range.start >= range.end {
                d.range.start = d.range.start - range.len() + inserted;
                d.range.end = d.range.end - range.len() + inserted;
                return true;
            }
            false
        });
    }
    fn decorated_runs(&self, style: &TextStyle, color: gpui::Hsla) -> Vec<TextRun> {
        let mut edges = vec![0, self.content.len()];
        for d in &self.decorations { edges.extend([d.range.start, d.range.end]); if d.icon.is_some() && d.range.start + 2 < d.range.end { edges.push(d.range.start + 2); } }
        if let Some(marked) = &self.marked_range { edges.extend([marked.start, marked.end]); }
        edges.sort_unstable(); edges.dedup();
        edges.windows(2).filter(|r| r[0] < r[1]).map(|r| {
            let mut run = TextRun { len: r[1] - r[0], font: style.font(), color, background_color: None, underline: None, strikethrough: None };
            for d in self.decorations.iter().filter(|d| d.range.start <= r[0] && r[0] < d.range.end) {
                if let Some(c) = &d.style.color { run.color = crate::color::parse_color_rgba(c).unwrap().into(); }
                if let Some(icon) = &d.icon { if r[0] < d.range.start + 2 { run.font.family = icon.font.clone().into(); } }
                // Backgrounds are painted locally, retaining decoration identity across font runs.
                if let Some(weight) = d.style.font_weight { run.font.weight = gpui::FontWeight(weight); }
                if let Some(s) = &d.style.font_style { run.font.style = if s == "italic" { gpui::FontStyle::Italic } else { gpui::FontStyle::Normal }; }
                if let Some(underline) = d.style.underline { run.underline = underline.then_some(UnderlineStyle { color: Some(run.color), thickness: px(1.0), wavy: false }); }
            }
            if self.marked_range.as_ref().is_some_and(|m| m.contains(&r[0])) {
                run.underline = Some(UnderlineStyle { color: Some(run.color), thickness: px(1.0), wavy: false });
            }
            run
        }).collect()
    }
}

// Uses the exact shaped advances and wrap boundaries; never changes editor geometry.
impl TextEditorState {
    fn paint_decoration_backgrounds(&self, bounds: Bounds<Pixels>, window: &mut Window) {
        if self.secure || self.display_is_placeholder || self.decorations.is_empty() { return; }
        let mut edges = Vec::new();
        for d in &self.decorations { if d.style.background_color.is_some() { edges.extend([d.range.start, d.range.end]); } }
        edges.sort_unstable(); edges.dedup();
        let mut spans: Vec<(Range<usize>, usize)> = Vec::new();
        for edge in edges.windows(2) {
            if let Some(owner) = self.decorations.iter().rposition(|d| d.style.background_color.is_some() && d.range.contains(&edge[0])) {
                if let Some((range, previous)) = spans.last_mut() {
                    if *previous == owner && range.end == edge[0] { range.end = edge[1]; continue; }
                }
                spans.push((edge[0]..edge[1], owner));
            }
        }
        let occupied = |index| spans.iter().any(|(r, _)| r.contains(&index));
        let mut y = bounds.top() - px(self.scroll_top);
        for (line_ix, line) in self.last_lines.iter().enumerate() {
            let base = self.line_starts[line_ix];
            let layout = &line.unwrapped_layout;
            let boundaries: Vec<usize> = std::iter::once(0).chain(line.wrap_boundaries.iter().map(|b| layout.runs[b.run_ix].glyphs[b.glyph_ix].index)).chain(std::iter::once(line.len())).collect();
            for fragment in boundaries.windows(2) {
                let row = base + fragment[0]..base + fragment[1];
                let row_x = layout.x_for_index(fragment[0]);
                let x_at = |index| bounds.left() - px(self.scroll_left) + layout.x_for_index(index - base) - row_x;
                for d in self.decorations.iter().filter(|d| d.icon.is_some()) {
                    if d.range.start >= row.start && d.range.start < row.end {
                        let icon = d.icon.as_ref().unwrap();
                        let size = px(icon.size);
                        let top = y + (self.line_height - size) / 2.0 + px(icon.size * 0.02);
                        crate::kybern_icon::paint(window, &icon.image, Bounds::new(point(x_at(d.range.start), top), gpui::size(size, size)));
                    }
                }
                for (span, owner) in &spans {
                    let start = span.start.max(row.start);
                    let end = span.end.min(row.end);
                    if start >= end { continue; }
                    let d = &self.decorations[*owner];
                    let mut left_gap = start;
                    for (offset, c) in self.content[row.start..start].char_indices().rev() {
                        let index = row.start + offset;
                        if !c.is_whitespace() || occupied(index) { break; }
                        left_gap = index;
                    }
                    let mut right_gap = end;
                    for (offset, c) in self.content[end..row.end].char_indices() {
                        let index = end + offset;
                        if !c.is_whitespace() || occupied(index) { break; }
                        right_gap = index + c.len_utf8();
                    }
                    // Each side owns at most half the whitespace: adjacent pills cannot collide.
                    let outset = px(d.style.background_outset_x.unwrap_or(0.0));
                    let left = (x_at(start) - outset.min((x_at(start) - x_at(left_gap)) / 2.0)).max(bounds.left());
                    let right = (x_at(end) + outset.min((x_at(right_gap) - x_at(end)) / 2.0)).min(bounds.right());
                    if right <= left { continue; }
                    let radius = px(d.style.border_radius.unwrap_or(0.0)).min(self.line_height / 2.0).min((right - left) / 2.0);
                    let mut quad = fill(Bounds::from_corners(point(left, y), point(right, y + self.line_height)), crate::color::parse_color_rgba(d.style.background_color.as_ref().unwrap()).unwrap());
                    quad.corner_radii = gpui::Corners {
                        top_left: if start == span.start { radius } else { px(0.0) },
                        bottom_left: if start == span.start { radius } else { px(0.0) },
                        top_right: if end == span.end { radius } else { px(0.0) },
                        bottom_right: if end == span.end { radius } else { px(0.0) },
                    };
                    window.paint_quad(quad);
                }
                y += self.line_height;
            }
        }
    }
}
