// Included in GPUI text_system/line.rs. Slice shaped glyphs, never source or
// Unicode text. The original TextLayout and its byte indices remain intact.
fn markdown_visible_line(
    origin: Point<Pixels>,
    layout: &LineLayout,
    line_height: Pixels,
    decorations: &[DecorationRun],
    boundaries: &[WrapBoundary],
    window: &Window,
) -> Option<(LineLayout, Vec<DecorationRun>, Vec<WrapBoundary>, Point<Pixels>)> {
    if boundaries.len() < 128 || line_height <= px(0.0) {
        return None;
    }
    let mask = window.content_mask().bounds;
    let rows = boundaries.len() + 1;
    let first = ((f32::from(mask.top() - origin.y) / f32::from(line_height)).floor() as isize - 1).max(0) as usize;
    let last = ((f32::from(mask.bottom() - origin.y) / f32::from(line_height)).ceil() as isize + 1).max(0) as usize;
    let first = first.min(rows);
    let last = last.min(rows).max(first);
    if first >= last {
        return Some((
            LineLayout {
                font_size: layout.font_size,
                width: px(0.0),
                ascent: layout.ascent,
                descent: layout.descent,
                runs: Vec::new(),
                len: layout.len,
            },
            Vec::new(),
            Vec::new(),
            origin,
        ));
    }
    if first == 0 && last == rows {
        return None;
    }
    let start = if first == 0 { WrapBoundary { run_ix: 0, glyph_ix: 0 } } else { boundaries[first - 1] };
    let end = if last < rows { Some(boundaries[last - 1]) } else { None };
    let start_glyph = layout.runs.get(start.run_ix)?.glyphs.get(start.glyph_ix)?;
    let start_x = start_glyph.position.x;
    let end_x = end.and_then(|b| layout.runs.get(b.run_ix)?.glyphs.get(b.glyph_ix)).map_or(layout.width, |g| g.position.x);
    let mut runs = Vec::new();
    let last_run = end.map_or(layout.runs.len(), |b| b.run_ix + 1);
    for run_ix in start.run_ix..last_run {
        let run = &layout.runs[run_ix];
        let a = if run_ix == start.run_ix { start.glyph_ix } else { 0 };
        let b = end.filter(|b| b.run_ix == run_ix).map_or(run.glyphs.len(), |b| b.glyph_ix);
        let glyphs = run.glyphs[a..b]
            .iter()
            .map(|glyph| {
                let mut glyph = glyph.clone();
                glyph.position.x -= start_x;
                glyph
            })
            .collect();
        runs.push(crate::ShapedRun { font_id: run.font_id, glyphs });
    }
    let wraps = boundaries
        .iter()
        .take(last.saturating_sub(1))
        .skip(first)
        .map(|boundary| WrapBoundary {
            run_ix: boundary.run_ix - start.run_ix,
            glyph_ix: boundary.glyph_ix - if boundary.run_ix == start.run_ix { start.glyph_ix } else { 0 },
        })
        .collect();
    // Preserve original byte indices, including shaping clusters and bidi runs.
    // The first style's length includes the skipped prefix so cumulative ends
    // in the original painter still compare against those absolute indices.
    let mut copied = Vec::new();
    let mut offset = 0;
    for decoration in decorations {
        offset += decoration.len;
        if offset as usize <= start_glyph.index {
            continue;
        }
        let mut decoration = decoration.clone();
        if copied.is_empty() {
            decoration.len = offset;
        }
        copied.push(decoration);
    }
    Some((
        LineLayout {
            font_size: layout.font_size,
            width: end_x - start_x,
            ascent: layout.ascent,
            descent: layout.descent,
            runs,
            len: layout.len,
        },
        copied,
        wraps,
        point(origin.x, origin.y + line_height * first as f32),
    ))
}
