// Included into markdown/render.rs. The canonical parser and original block
// renderer remain the formatting authority; culling changes only instantiation.
fn document_slots(block: &Block) -> usize {
    crate::markdown_model::slots(block)
}
fn document_gaps(blocks: &[&Block], theme: &Theme) -> Vec<f32> {
    let mut previous_bottom: f32 = 0.0;
    let mut previous_heading = false;
    blocks
        .iter()
        .enumerate()
        .map(|(index, block)| {
            let (top, bottom) = if theme.markdown.heading_margin_top.is_none() && theme.markdown.heading_margin_bottom.is_none() {
                (theme.metrics.md_block_gap, theme.metrics.md_block_gap)
            } else if let Block::Heading { level, .. } = block {
                let (size, _) = heading(theme, *level);
                (
                    number(tier(&theme.markdown.heading_margin_tops, *level).or(theme.markdown.heading_margin_top), 0.0) * size,
                    number(theme.markdown.heading_margin_bottom, 0.0) * size,
                )
            } else if matches!(block, Block::Rule) {
                let gap = number(theme.markdown.rule_margin, theme.metrics.md_block_gap);
                (gap, gap)
            } else {
                (theme.metrics.md_block_gap, theme.metrics.md_block_gap)
            };
            let gap = if index == 0 {
                0.0
            } else if previous_heading && !matches!(block, Block::Heading { .. }) {
                number(theme.markdown.heading_following_gap, previous_bottom.max(top))
            } else {
                previous_bottom.max(top)
            };
            previous_bottom = bottom;
            previous_heading = matches!(block, Block::Heading { .. });
            gap
        })
        .collect()
}
pub fn render_document(
    document: Arc<crate::markdown_document::Document>,
    ctx: &mut MdContext,
    window: &Window,
    start: usize,
    end: Option<usize>,
) -> AnyElement {
    let cache = ctx.viewport.as_ref().unwrap().clone();
    cache.borrow_mut().prepare(window.scale_factor(), ctx.selectable);
    crate::markdown_view::selection_document(document.selection_id, &cache);
    let start = start.min(document.blocks.len());
    let end = end.unwrap_or(document.blocks.len()).min(document.blocks.len()).max(start);
    let meta = crate::markdown_view::cached(&cache, 0, || {
        let gaps = document_gaps(&document.blocks.iter().map(|b| b.as_ref()).collect::<Vec<_>>(), &ctx.theme);
        let mut bases = Vec::new();
        let mut base = 1;
        let mut texts = vec![0];
        for block in &document.blocks {
            bases.push(base);
            base += document_slots(block);
            texts.push(texts.last().unwrap() + count_text(std::slice::from_ref(block.as_ref())));
        }
        let final_text = document.blocks.last().is_some_and(|b| ends_with_text(std::slice::from_ref(b.as_ref())));
        DocumentMeta { bases, gaps, texts, final_text }
    });
    cache.borrow_mut().publish = document.layout.lock().unwrap().is_none();
    let measured = document.clone();
    cache.borrow_mut().measured = Some(Rc::new(move |width, scale, positions| {
        *measured.layout.lock().unwrap() =
            Some(crate::markdown_document::Layout { width, scale, start, positions: Arc::new(positions.to_vec()) });
    }));
    let gaps = crate::markdown_view::cached(&cache, 0, || meta.gaps[start..end].to_vec());
    let mut context = ctx.clone();
    context.element_id = document.selection_id;
    crate::markdown_view::flow(
        &cache,
        0,
        gaps,
        Rc::new(move |index, window| {
            let index = index + start;
            let mut context = context.clone();
            context.next_sub = meta.bases[index];
            context.text_total = *meta.texts.last().unwrap();
            context.text_seen = meta.texts[index];
            context.stream_final = context.theme.markdown.stream.is_some() && meta.final_text;
            context.document_node = Some(DocumentNode { document: document.clone(), top: index, path: Vec::new() });
            render_block(&document.blocks[index], &mut context, window)
        }),
    )
}

#[derive(Clone)]
pub struct DocumentNode {
    document: Arc<crate::markdown_document::Document>,
    top: usize,
    path: Vec<(usize, usize)>,
}
impl DocumentNode {
    fn block(&self) -> &Block {
        let mut block = self.document.blocks[self.top].as_ref();
        for &(item, child) in &self.path {
            block = match block {
                Block::List { items, .. } => &items[item][child],
                Block::BlockQuote { children } => &children[child],
                _ => unreachable!("document path"),
            };
        }
        block
    }
    fn child(&self, item: usize, child: usize) -> Self {
        let mut node = self.clone();
        node.path.push((item, child));
        node
    }
}
fn document_container(block: &Block, ctx: &mut MdContext, window: &Window) -> Option<AnyElement> {
    match block {
        Block::List { .. } => Some(document_list(ctx, window)),
        Block::BlockQuote { .. } => Some(document_quote(ctx)),
        Block::CodeBlock { .. } => Some(document_code(ctx, window)),
        Block::Table { .. } => Some(document_table(ctx, window)),
        _ => None,
    }
}
fn document_children(node: DocumentNode, item: usize, gap: f32, ctx: &mut MdContext) -> AnyElement {
    let blocks = match node.block() {
        Block::List { items, .. } => &items[item],
        Block::BlockQuote { children } => children,
        _ => unreachable!(),
    };
    let cache = ctx.viewport.as_ref().unwrap().clone();
    let key = ctx.take_sub();
    let meta = crate::markdown_view::cached(&cache, key, || {
        let mut base = ctx.next_sub;
        let mut bases = Vec::with_capacity(blocks.len());
        for block in blocks {
            bases.push(base);
            base += document_slots(block);
        }
        let gaps = Rc::new((0..blocks.len()).map(|i| if i == 0 { 0.0 } else { gap }).collect());
        SequenceMeta { bases, gaps }
    });

    let context = ctx.clone();
    crate::markdown_view::flow(
        &cache,
        key,
        meta.gaps.clone(),
        Rc::new(move |index, window| {
            let mut ctx = context.clone();
            ctx.next_sub = meta.bases[index];
            let child = node.child(item, index);
            ctx.document_node = Some(child.clone());
            render_block(child.block(), &mut ctx, window)
        }),
    )
}
fn document_quote(ctx: &mut MdContext) -> AnyElement {
    use gpui::prelude::*;
    let theme = ctx.theme.clone();
    let node = ctx.document_node.as_ref().unwrap().clone();
    let mut inner = ctx.clone();
    inner.theme.text = color(&theme.markdown.quote_text, theme.text_muted);
    let children = document_children(node, 0, number(theme.markdown.quote_gap, 8.0), &mut inner);
    div()
        .border_l(px(number(theme.markdown.quote_border_width, 2.0)))
        .border_color(color(&theme.markdown.quote_border, opacity(theme.accent, 0.6)))
        .bg(color(&theme.markdown.quote_background, opacity(theme.accent, 0.05)))
        .rounded_tr(px(if theme.markdown.quote_background.is_some() { 0.0 } else { 6.0 }))
        .rounded_br(px(if theme.markdown.quote_background.is_some() { 0.0 } else { 6.0 }))
        .pl(px(number(theme.markdown.quote_padding_left, 12.0)))
        .pr(px(number(theme.markdown.quote_padding_right, 10.0)))
        .py(px(number(theme.markdown.quote_padding_y, 6.0)))
        .flex()
        .flex_col()
        .child(children)
        .into_any_element()
}
fn document_list(ctx: &mut MdContext, window: &Window) -> AnyElement {
    let node = ctx.document_node.as_ref().unwrap().clone();
    let Block::List { items, .. } = node.block() else { unreachable!() };
    let cache = ctx.viewport.as_ref().unwrap().clone();
    let key = ctx.take_sub();
    let meta = crate::markdown_view::cached(&cache, key, || {
        let mut base = ctx.next_sub;
        let mut bases = Vec::with_capacity(items.len());
        for item in items {
            bases.push(base);
            base += 4 + item.iter().map(document_slots).sum::<usize>();
        }
        let gaps = Rc::new(
            (0..items.len())
                .map(|i| {
                    if i == 0 {
                        return 0.0;
                    }
                    let gap = number(ctx.theme.markdown.list_item_gap, 4.0);
                    let extra = if matches!(items[i - 1].last(), Some(Block::List { .. })) {
                        (number(ctx.theme.markdown.list_child_gap, 4.0) - gap).max(0.0)
                    } else {
                        0.0
                    };
                    f32::from(window.pixel_snap(px(gap))) + f32::from(window.pixel_snap(px(extra)))
                })
                .collect(),
        );
        SequenceMeta { bases, gaps }
    });
    let context = ctx.clone();
    crate::markdown_view::flow(
        &cache,
        key,
        meta.gaps.clone(),
        Rc::new(move |item_ix, window| {
            let mut ctx = context.clone();
            ctx.next_sub = meta.bases[item_ix];
            document_list_item(&node, item_ix, &mut ctx, window)
        }),
    )
}
fn document_list_item(node: &DocumentNode, item_ix: usize, ctx: &mut MdContext, _window: &Window) -> AnyElement {
    use gpui::prelude::*;
    let theme = ctx.theme.clone();
    let m = &theme.metrics;
    let Block::List { ordered_start, .. } = node.block() else { unreachable!() };
    // Ordered numbers are accent-tinted text; unordered markers are
    // a real 5px disc, because the glyph "•" reads too small at 14px.
    let marker: AnyElement = match ordered_start {
        Some(start) => div()
            .flex_none()
            .w(px((number(theme.markdown.list_indent, 26.0) - number(theme.markdown.list_marker_gap, 8.0)).max(0.0)))
            .text_size(px(m.md_text_size))
            .line_height(px(m.md_line_height))
            .when(theme.markdown.list_indent.is_some(), |el| el.text_right())
            .text_color(color(&theme.markdown.list_marker, opacity(theme.accent, 0.85)))
            .child(crate::text::chrome_text(SharedString::from(format!("{}.", start + item_ix as u64)), None))
            .into_any_element(),
        None => div()
            .flex_none()
            .w(px((number(theme.markdown.list_indent, 26.0) - number(theme.markdown.list_marker_gap, 8.0)).max(0.0)))
            // Centre the disc on the first text line's cap band.
            .h(px(m.md_line_height))
            .flex()
            .items_center()
            .when(theme.markdown.list_indent.is_some(), |el| el.justify_end())
            .child(
                div()
                    .ml(px(1.0))
                    .mr(px((number(theme.markdown.list_bullet_gap, number(theme.markdown.list_marker_gap, 8.0))
                        - number(theme.markdown.list_marker_gap, 8.0))
                    .max(0.0)))
                    .w(px(number(theme.markdown.list_marker_size, 5.0)))
                    .h(px(number(theme.markdown.list_marker_size, 5.0)))
                    .rounded(px(if theme.markdown.nested_list_markers && ctx.list_depth % 3 == 2 {
                        0.0
                    } else {
                        number(theme.markdown.list_marker_size, 5.0) / 2.0
                    }))
                    .bg(if ctx.list_depth % 3 == 1 && theme.markdown.nested_list_markers {
                        gpui::transparent_black()
                    } else {
                        color(&theme.markdown.list_marker, opacity(theme.accent, 0.85))
                    })
                    .border(px(if ctx.list_depth % 3 == 1 && theme.markdown.nested_list_markers { 1.0 } else { 0.0 }))
                    .border_color(color(&theme.markdown.list_marker, opacity(theme.accent, 0.85))),
            )
            .into_any_element(),
    };
    ctx.list_depth += 1;
    let children = document_children(node.clone(), item_ix, number(theme.markdown.list_child_gap, 4.0), ctx);
    ctx.list_depth -= 1;
    let row = div()
        .w_full()
        .min_w_0()
        .flex()
        .flex_row()
        .gap(px(number(theme.markdown.list_marker_gap, 8.0)))
        .child(marker)
        .child(div().flex_1().min_w_0().flex().flex_col().gap(px(number(theme.markdown.list_child_gap, 4.0))).child(children));
    row.into_any_element()
}
struct DocumentCodeData {
    node: DocumentNode,
    highlight: Option<Arc<crate::syntax::HighlightedDocument>>,
    offsets: Vec<usize>,
    width: f32,
    gaps: Rc<Vec<f32>>,
}
fn document_code(ctx: &mut MdContext, window: &Window) -> AnyElement {
    use gpui::prelude::*;

    let theme = ctx.theme.clone();
    let m = &theme.metrics;
    let mono = font(theme.font_mono.clone());
    let node = ctx.document_node.as_ref().unwrap().clone();
    let Block::CodeBlock { language, code } = node.block() else { unreachable!() };
    let language = language.as_deref();
    let cache = ctx.viewport.as_ref().unwrap().clone();
    let key = ctx.take_sub();
    let data = crate::markdown_view::cached(&cache, key, || {
        let mut path = node.top.to_string();
        for (item, child) in &node.path {
            path.push_str(&format!("/{item}/{child}"));
        }
        let highlight = node.document.highlights.get(&path).cloned();
        let mut offsets = vec![0];
        offsets.extend(code.match_indices('\n').map(|(i, _)| i + 1));
        let width = code
            .split('\n')
            .map(|line| {
                f32::from(
                    window
                        .text_system()
                        .shape_line(
                            line.to_string().into(),
                            px(m.code_text_size),
                            &[TextRun {
                                len: line.len(),
                                font: mono.clone(),
                                color: theme.text,
                                background_color: None,
                                underline: None,
                                strikethrough: None,
                            }],
                            None,
                        )
                        .width(),
                )
            })
            .fold(0.0, f32::max);
        let gaps = Rc::new(vec![0.0; offsets.len()]);
        DocumentCodeData { node: node.clone(), highlight, offsets, width, gaps }
    });

    // overflow-x only works as a flex *row* viewport. A flex_col scroller
    // stretches each nowrap row to the card width, so the line never overflows
    // and a horizontal wheel does nothing. Same pattern as host overflowX.
    let scroll_sub = ctx.take_sub();
    let mut lines = div()
        .flex_none()
        .flex()
        .flex_col()
        .px(px(m.md_code_padding_x))
        .py(px(m.md_code_padding_y))
        .font_family(theme.font_mono.clone())
        .text_size(px(m.code_text_size))
        .line_height(px(m.code_line_height))
        .whitespace_nowrap();

    if theme.markdown.code_wrap {
        lines = lines.w_full().min_w_0().flex_shrink_1().whitespace_normal();
    }
    let context = ctx.clone();
    let base = ctx.next_sub;
    ctx.next_sub += data.offsets.len();
    let gaps = data.gaps.clone();
    let natural = data.width;
    let rows = crate::markdown_view::flow(
        &cache,
        key,
        gaps,
        Rc::new(move |line_ix, _window| {
            let ctx = context.clone();
            let theme = ctx.theme.clone();
            let m = &theme.metrics;
            let mono = font(theme.font_mono.clone());
            let Block::CodeBlock { code, .. } = data.node.block() else { unreachable!() };
            let start = data.offsets[line_ix];
            let end = data.offsets.get(line_ix + 1).map_or(code.len(), |n| n - 1);
            let line = &code[start..end];
            let highlight = &data.highlight;
            let spans: Vec<(Range<usize>, Hsla)> = highlight
                .as_ref()
                .and_then(|doc: &Arc<_>| doc.lines.get(line_ix))
                .map(|spans| spans.iter().map(|span| (span.range.clone(), theme.syntax.color(span.kind))).collect())
                .unwrap_or_default();
            let runs = runs_for_spans(line, &spans, &mono, theme.text);
            let sub = base + line_ix;
            document_record(&ctx, sub, line.to_string().into(), None);
            // Content, not chrome: `userSelect: "none"` stops the drag, not the
            // find, and `chrome_text` cannot paint a highlight wash.
            let text: AnyElement = crate::text::selectable_text(crate::text::SelectableText {
                selectable: ctx.selectable,
                highlight: ctx.highlight_set.clone().map(crate::text::HighlightSource::Native),
                ..crate::text::SelectableText::new(
                    ctx.element_id,
                    sub,
                    SharedString::from(line.to_string()),
                    Some(runs),
                    ctx.selection.clone(),
                    ctx.selection_wash,
                )
            });
            let row = div()
                .h(px(m.code_line_height))
                .flex_none()
                .font_family(theme.font_mono.clone())
                .text_size(px(m.code_text_size))
                .line_height(px(m.code_line_height));
            crate::code_wrap::row(row, theme.markdown.code_wrap, m.code_line_height).child(text).into_any_element()
        }),
    );
    if !theme.markdown.code_wrap {
        lines = lines.w(px(natural + 2.0 * m.md_code_padding_x));
    }
    lines = lines.child(rows);

    let body = div()
        .id(SharedString::from(format!("__gpuix_md_code_{}_{scroll_sub}", ctx.element_id)))
        .flex()
        .min_w_0()
        .overflow_x_scroll()
        .restrict_scroll_to_axis()
        .child(lines);

    let mut block = div()
        .w_full()
        .min_w_0()
        .rounded(px(m.md_code_radius))
        .bg(color(&theme.markdown.code_background, ink(&theme, 0.035)))
        .border(px(number(theme.markdown.code_border_width, 1.0)))
        .border_color(color(&theme.markdown.code_border, theme.border))
        .overflow_hidden();

    if let Some(language) = language.filter(|_| theme.markdown.code_header.unwrap_or(true)) {
        block = block.child(
            div()
                .px(px(m.md_code_padding_x))
                .py(px(m.md_code_header_padding_y))
                .border_b(px(number(theme.markdown.code_header_border_width, 1.0)))
                .border_color(theme.border)
                .bg(color(&theme.markdown.code_header_background, ink(&theme, 0.02)))
                .line_height(px(number(theme.markdown.code_header_line_height, m.md_line_height)))
                .font_family(theme.font_mono.clone())
                .text_size(px(m.md_code_header_text_size))
                .text_color(theme.text_muted)
                .child(crate::text::chrome_text(SharedString::from(language.to_string()), None)),
        );
    }

    block.child(body).into_any_element()
}

struct DocumentTableData {
    flats: Vec<Vec<Option<FlatText>>>,
    geo: TableColumns,
    bases: Vec<usize>,
    gaps: Rc<Vec<f32>>,
}
fn document_table(ctx: &mut MdContext, window: &Window) -> AnyElement {
    use gpui::prelude::*;
    let node = ctx.document_node.as_ref().unwrap().clone();
    let Block::Table { header, rows, align } = node.block() else { unreachable!() };
    let theme = ctx.theme.clone();
    let m = &theme.metrics;
    let all: Vec<&[Vec<InlineRun>]> =
        std::iter::once(header).filter(|h| !h.is_empty()).map(|h| h.as_slice()).chain(rows.iter().map(|r| r.as_slice())).collect();
    let cols = all.iter().map(|r| r.len()).max().unwrap_or(0);
    if cols == 0 {
        return gpui::Empty.into_any_element();
    }
    let has_header = !header.is_empty();
    let cache = ctx.viewport.as_ref().unwrap().clone();
    let table_sub = ctx.take_sub();
    let hairline = color(&theme.markdown.table_border, hairline(&theme, 0.10));
    let border = number(theme.markdown.table_border_width, 1.0);
    let data = crate::markdown_view::cached(&cache, table_sub, || {
        // Flatten every cell once and take per-column max-content widths.
        let text_system = window.text_system();
        let mut flats: Vec<Vec<Option<FlatText>>> = Vec::with_capacity(all.len());
        let mut content = vec![0.0f32; cols];
        for (row_ix, row) in all.iter().enumerate() {
            let weight =
                if has_header && row_ix == 0 { FontWeight(number(theme.markdown.table_header_weight, 700.0)) } else { FontWeight::NORMAL };
            let mut out: Vec<Option<FlatText>> = Vec::with_capacity(cols);
            for (col, natural) in content.iter_mut().enumerate() {
                let Some(runs) = row.get(col) else {
                    out.push(None);
                    continue;
                };
                let flat = flatten_runs(runs, &theme, weight);
                if !flat.text.is_empty() {
                    // Cells are single-line; guard anyway, and keep the byte count
                    // identical so the runs still cover the text exactly.
                    let line: SharedString = if flat.text.contains('\n') { flat.text.replace('\n', " ").into() } else { flat.text.clone() };
                    let width = f32::from(text_system.shape_line(line, px(m.md_text_size), &flat.runs, None).width());
                    if width > *natural {
                        *natural = width;
                    }
                }
                out.push(Some(flat));
            }
            flats.push(out);
        }
        let mut table_metrics = *m;
        table_metrics.md_table_cell_padding = number(theme.markdown.table_padding_x, m.md_table_cell_padding);
        let geo = table_columns(&content, &table_metrics);

        let mut base = ctx.next_sub;
        let mut bases = Vec::new();
        for row in &all {
            bases.push(base);
            base += 4 + row.iter().flatten().map(|r| r.text.len() + 4).sum::<usize>();
        }
        let gaps = Rc::new(vec![0.0; flats.len()]);
        DocumentTableData { flats, geo, bases, gaps }
    });
    let minimum = data.geo.min_table_width;
    let context = ctx.clone();
    let align = align.clone();
    let rows_el = crate::markdown_view::flow(
        &cache,
        table_sub,
        data.gaps.clone(),
        Rc::new(move |row_ix, _window| {
            let mut ctx = context.clone();
            ctx.next_sub = data.bases[row_ix];
            let theme = ctx.theme.clone();
            let m = &theme.metrics;
            let row = &data.flats[row_ix];
            let geo = &data.geo;
            let mut inner = div().w_full().flex().flex_col();
            if row_ix > 0 {
                inner = inner.child(
                    div()
                        .flex_none()
                        .h(px(if row_ix == 1 && has_header { number(theme.markdown.table_header_rule_width, border) } else { border }))
                        .w_full()
                        .bg(hairline),
                );
            }
            let mut row_el = div().w_full().flex().flex_row();
            if row_ix == 0 && has_header {
                row_el = row_el.bg(color(&theme.markdown.table_header_background, gpui::transparent_black()));
            }
            for (col, cell_flat) in row.iter().enumerate() {
                let mut cell = div()
                    .flex_grow(geo.naturals[col])
                    .flex_shrink(geo.naturals[col])
                    .flex_basis(px(0.0))
                    .min_w(px(geo.minimums[col]))
                    .px(px(number(theme.markdown.table_padding_x, m.md_table_cell_padding)))
                    .py(px(number(theme.markdown.table_padding_y, m.md_table_cell_padding)))
                    .text_size(px(m.md_text_size))
                    .line_height(px(m.md_line_height));
                if theme.markdown.table_grid && col > 0 {
                    cell = cell.border_l(px(border)).border_color(hairline);
                }
                cell = match align.get(col).copied().unwrap_or_default() {
                    TableAlign::Left => cell,
                    TableAlign::Center => cell.text_center(),
                    TableAlign::Right => cell.text_right(),
                };
                if let Some(flat) = cell_flat {
                    if !flat.text.is_empty() {
                        cell = cell.child(inline_element(flat, m.md_text_size, m.md_line_height, align.get(col).copied(), &mut ctx));
                    }
                }
                row_el = row_el.child(cell);
            }
            inner.child(row_el).into_any_element()
        }),
    );

    let mut inner = div().flex().flex_col().w_full().min_w(px(minimum));
    if theme.markdown.table_grid {
        inner = inner.border(px(border)).border_color(hairline);
    }
    inner = inner.child(rows_el);
    div()
        .id(SharedString::from(format!("__gpuix_md_table_{}_{table_sub}", ctx.element_id)))
        .w_full()
        .flex()
        .min_w_0()
        .overflow_x_scroll()
        .restrict_scroll_to_axis()
        .child(inner.flex_none())
        .into_any_element()
}

fn document_record(ctx: &MdContext, sub: usize, text: SharedString, group: Option<u64>) {
    if let Some(cache) = &ctx.viewport {
        let len = text.len();
        crate::markdown_view::record(cache, ctx.element_id, sub, text, 0..len, group);
    }
}

pub(crate) fn invalidate_document(
    cache: &crate::markdown_view::Cache,
    doc: &crate::markdown_document::Document,
    start: usize,
    end: Option<usize>,
) {
    let prefix = doc.stable_prefix;
    if end.unwrap_or(doc.blocks.len()) <= prefix {
        return;
    }
    let sub = 1 + doc.blocks.iter().take(prefix).map(|b| document_slots(b)).sum::<usize>();
    cache.borrow_mut().invalidate_suffix(prefix.saturating_sub(start), sub);
}

struct DocumentMeta {
    bases: Vec<usize>,
    gaps: Vec<f32>,
    texts: Vec<usize>,
    final_text: bool,
}
struct SequenceMeta {
    bases: Vec<usize>,
    gaps: Rc<Vec<f32>>,
}

fn document_text(runs: &[InlineRun], size: f32, line_height: f32, weight: FontWeight, ctx: &mut MdContext) -> AnyElement {
    use gpui::prelude::*;
    let cache = ctx.viewport.as_ref().unwrap().clone();
    let flat = crate::markdown_view::cached(&cache, ctx.next_sub, || flatten_runs(runs, &ctx.theme, weight));
    let inner = inline_element(&flat, size, line_height, None, ctx);
    div().w_full().min_w_0().text_size(px(size)).line_height(px(line_height)).child(inner).into_any_element()
}
