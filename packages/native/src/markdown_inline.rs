// Mixed-size inline flow. Measurement retains ranges and advances, never a
// flex node for every word. Only viewport-intersecting fragments get GPUI text
// elements. Original bytes, selection groups, links and paragraph-wide search
// are shared across fragments; geometry never enters the copied text.
use gpui::{App, AvailableSpace, Bounds, Element, ElementId, GlobalElementId, InspectorElementId, IntoElement, LayoutId, Pixels};
use std::cell::RefCell;
use std::rc::Rc;

#[derive(Clone)]
struct InlineToken {
    range: Range<usize>,
    code: bool,
}
struct InlineData {
    flat: FlatText,
    tokens: Vec<InlineToken>,
    font_sans: String,
    size: f32,
    line: f32,
    padding: f32,
    inset: f32,
    ratio: f32,
    radius: f32,
    wash: Hsla,
    align: TableAlign,
    element_id: u64,
    base: usize,
    group: u64,
    selection: SharedSelection,
    selectable: bool,
    selection_wash: Hsla,
    hover_link_color: Option<Hsla>,
    on_link: Option<Arc<dyn Fn(&str)>>,
    highlight: Option<Arc<crate::text::HighlightContext>>,
    icons: Vec<(usize, Arc<gpui::RenderImage>)>,
    caret: Option<Hsla>,
}
impl InlineData {
    fn runs(&self, range: &Range<usize>) -> Vec<TextRun> {
        let mut out = Vec::new();
        let mut offset = 0;
        for run in &self.flat.runs {
            let a = range.start.max(offset);
            let b = range.end.min(offset + run.len);
            if a < b {
                let mut clipped = run.clone();
                clipped.len = b - a;
                out.push(clipped);
            }
            offset += run.len;
            if offset >= range.end {
                break;
            }
        }
        out
    }
    fn layout(&self, width: f32, window: &Window) -> InlineLayout {
        let mut positions: Vec<InlinePosition> = Vec::with_capacity(self.tokens.len());
        let (mut x, mut y) = (0.0, 0.0);
        let mut line_start = 0;
        let strut = window.text_system().shape_line(
            "M".into(),
            px(self.size),
            &[TextRun {
                len: 1,
                font: font(self.font_sans.clone()),
                color: self.wash,
                background_color: None,
                underline: None,
                strikethrough: None,
            }],
            None,
        );
        let base_baseline = (self.line + f32::from(strut.ascent) - f32::from(strut.descent)) / 2.0;
        let align_line = |positions: &mut [InlinePosition], used: f32| {
            let baseline = positions.iter().map(|p| p.baseline).fold(base_baseline, f32::max);
            let descent = positions.iter().map(|p| p.height - p.baseline).fold(self.line - base_baseline, f32::max);
            let offset = match self.align {
                TableAlign::Left => 0.0,
                TableAlign::Center => (width - used).max(0.0) / 2.0,
                TableAlign::Right => (width - used).max(0.0),
            };
            for position in positions {
                position.x += offset;
                position.y += baseline - position.baseline;
            }
            baseline + descent
        };
        for (index, token) in self.tokens.iter().enumerate() {
            let forced = self.flat.text[token.range.clone()].ends_with('\n');
            let measure_range = token.range.start..token.range.end - usize::from(forced);
            let runs = self.runs(&measure_range);
            let text: SharedString = self.flat.text[measure_range].to_string().into();
            let size = if token.code { self.size * self.ratio } else { self.size };
            let pad = if token.code { self.padding } else { 0.0 };
            let shaped = window.text_system().shape_line(text.clone(), px(size), &runs, None);
            let natural = f32::from(shaped.width());
            let baseline = (self.line + f32::from(shaped.ascent) - f32::from(shaped.descent)) / 2.0;
            let token_width = (natural + 2.0 * pad).min(width).max(0.0);
            if x > 0.0 && x + token_width > width + 0.01 {
                y += align_line(&mut positions[line_start..], x);
                line_start = positions.len();
                x = 0.0;
            }
            let inner_width = (token_width - 2.0 * pad).max(1.0);
            let height = if natural > inner_width + 0.01 {
                window
                    .text_system()
                    .shape_text(text, px(size), &runs, Some(px(inner_width)), None)
                    .map(|lines| lines.iter().map(|line| f32::from(line.size(px(self.line)).height)).sum::<f32>())
                    .unwrap_or(self.line)
            } else {
                self.line
            };
            positions.push(InlinePosition { index, x, y, width: token_width, height, baseline });
            x += token_width;

            if forced {
                y += align_line(&mut positions[line_start..], x);
                line_start = positions.len();
                x = 0.0;
            }
        }
        y += align_line(&mut positions[line_start..], x);
        InlineLayout { width, height: y, positions }
    }
}
struct InlinePosition {
    baseline: f32,
    index: usize,
    x: f32,
    y: f32,
    width: f32,
    height: f32,
}
struct InlineLayout {
    width: f32,
    height: f32,
    positions: Vec<InlinePosition>,
}
struct InlineFlow {
    data: Rc<InlineData>,
}
impl IntoElement for InlineFlow {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}
impl Element for InlineFlow {
    type RequestLayoutState = Rc<RefCell<Option<InlineLayout>>>;
    type PrepaintState = Vec<AnyElement>;
    fn id(&self) -> Option<ElementId> {
        None
    }
    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }
    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        _: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let data = self.data.clone();
        let state = Rc::new(RefCell::new(None));
        let output = state.clone();
        let mut style = gpui::Style::default();
        style.size.width = gpui::relative(1.0).into();
        let layout = window.request_measured_layout(style, move |known, available, window, _| {
            let width = f32::from(known.width.unwrap_or(match available.width {
                AvailableSpace::Definite(w) => w,
                _ => px(320.0),
            }))
            .max(1.0);
            let layout = data.layout(width, window);
            let height = layout.height;
            *output.borrow_mut() = Some(layout);
            gpui::size(px(width), px(height))
        });
        (layout, state)
    }
    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        state: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Vec<AnyElement> {
        use gpui::prelude::*;
        let data = &self.data;
        let width = f32::from(bounds.size.width);
        let mut layout = state.borrow_mut();
        if layout.as_ref().is_none_or(|l| (l.width - width).abs() > 0.1) {
            *layout = Some(data.layout(width, window));
        }
        let layout = layout.as_ref().unwrap();
        let clip = window.content_mask().bounds;
        let washes = Rc::new(RefCell::new(None));
        let search_key = crate::text::selection_key(data.element_id, data.base);
        let mut children = Vec::new();
        for position in &layout.positions {
            let token = &data.tokens[position.index];
            let origin = bounds.origin + gpui::point(px(position.x), px(position.y));
            let piece = Bounds { origin, size: gpui::size(px(position.width), px(position.height)) };
            if piece.bottom() < clip.top() || piece.top() > clip.bottom() || piece.right() < clip.left() || piece.left() > clip.right() {
                continue;
            }
            let pad = if token.code { data.padding } else { 0.0 };
            let range = &token.range;
            let len = range.len();
            let wash = data.wash;
            let radius = data.radius;
            let inset = data.inset;
            let is_code = token.code;
            let icon_here: Vec<(usize, Arc<gpui::RenderImage>)> = data.icons.iter().filter(|(at, _)| *at == range.start).map(|(_, image)| (0, image.clone())).collect();
            let caret_here = if position.index + 1 == data.tokens.len() { data.caret } else { None };
            let size_here = data.size;
            let extra_wash: Option<Box<dyn Fn(&gpui::TextLayout, &mut Window)>> = if is_code || !icon_here.is_empty() || caret_here.is_some() {
                Some(Box::new(move |layout, window| {
                    if is_code {
                        for rect in range_rects(layout, &(0..len), pad, inset) {
                            window.paint_quad(gpui::quad(rect, px(radius), wash, px(0.0), gpui::transparent_black(), BorderStyle::default()));
                        }
                    }
                    paint_decorations(layout, window, &icon_here, len.min(2), caret_here, len, size_here);
                }))
            } else {
                None
            };
            let links = data
                .flat
                .links
                .iter()
                .filter_map(|(r, url)| {
                    let a = range.start.max(r.start);
                    let b = range.end.min(r.end);
                    (a < b).then(|| (a - range.start..b - range.start, url.clone()))
                })
                .collect();
            let text = crate::text::selectable_text(crate::text::SelectableText {
                selectable: data.selectable,
                group: Some(data.group),
                links,
                hover_link_color: data.hover_link_color,
                on_link: data.on_link.clone(),
                extra_wash,
                highlight: data.highlight.clone().map(|context| crate::text::HighlightSource::NativeFragment {
                    context,
                    key: search_key.clone(),
                    text: data.flat.text.clone(),
                    range: range.clone(),
                    washes: washes.clone(),
                }),
                ..crate::text::SelectableText::new(
                    data.element_id,
                    data.base + 1 + range.start,
                    data.flat.text[range.clone()].to_string().into(),
                    Some(data.runs(range)),
                    data.selection.clone(),
                    data.selection_wash,
                )
            });
            let mut child = div()
                .w(px((position.width - 2.0 * pad).max(1.0)))
                .h(px(position.height))
                .text_size(px(if token.code { data.size * data.ratio } else { data.size }))
                .line_height(px(data.line))
                .when(position.height > data.line + 0.01, |el| el.whitespace_normal())
                .when(position.height <= data.line + 0.01, |el| el.whitespace_nowrap())
                .child(text)
                .into_any_element();
            child.prepaint_as_root(
                origin + gpui::point(px(pad), px(0.0)),
                gpui::size(
                    AvailableSpace::Definite(px((position.width - 2.0 * pad).max(1.0))),
                    AvailableSpace::Definite(px(position.height)),
                ),
                window,
                cx,
            );
            children.push(child);
        }
        children
    }
    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        children: &mut Vec<AnyElement>,
        window: &mut Window,
        cx: &mut App,
    ) {
        for child in children {
            child.paint(window, cx)
        }
    }
}
fn inline_element(flat: &FlatText, size: f32, line: f32, align: Option<TableAlign>, ctx: &mut MdContext) -> AnyElement {
    let style = &ctx.theme.markdown;
    let padding = number(style.inline_code_padding_x, 0.0);
    let ratio = number(style.inline_code_size_ratio, 1.0).max(0.01);
    // Preserve GPUI's whole-paragraph bidirectional shaping until inline flow
    // has a bidi-aware visual-order map; never reverse RTL text word-by-word.
    let bidi =
        flat.text.chars().any(|c| matches!(c as u32,0x0590..=0x08ff|0xfb1d..=0xfdff|0xfe70..=0xfefc|0x202a..=0x202e|0x2066..=0x2069));
    if bidi || flat.code_ranges.is_empty() || (padding == 0.0 && ratio == 1.0) {
        return flat_text_element(flat, size, ctx);
    }
    let mut tokens = Vec::new();
    let mut start = 0;
    while start < flat.text.len() {
        let code = flat.code_ranges.iter().find(|r| r.contains(&start));
        let end = if let Some(code) = code {
            code.end
        } else {
            let limit = flat.code_ranges.iter().find(|r| r.start > start).map_or(flat.text.len(), |r| r.start);
            flat.text[start..limit].char_indices().find(|(_, c)| c.is_whitespace()).map_or(limit, |(i, c)| start + i + c.len_utf8())
        };
        tokens.push(InlineToken { range: start..end, code: code.is_some() });
        start = end;
    }
    let base = ctx.next_sub;
    ctx.next_sub += flat.text.len() + 2;
    InlineFlow {
        data: Rc::new(InlineData {
            flat: FlatText {
                text: flat.text.clone(),
                runs: flat.runs.clone(),
                links: flat.links.clone(),
                code_ranges: flat.code_ranges.clone(),
                icons: flat.icons.clone(),
            },
            tokens,
            font_sans: ctx.theme.font_sans.clone(),
            size,
            line,
            padding,
            ratio,
            inset: ((line - size * ratio * 1.2) / 2.0 - number(style.inline_code_padding_y, 0.0)).max(0.0),
            radius: ctx.theme.metrics.md_inline_code_radius,
            wash: ctx.theme.code_wash,
            align: align.unwrap_or_default(),
            element_id: ctx.element_id,
            base,
            group: ctx.element_id.rotate_left(32) ^ base as u64,
            selection: ctx.selection.clone(),
            selectable: ctx.selectable,
            selection_wash: ctx.selection_wash,
            hover_link_color: (ctx.theme.markdown.link.underline.as_deref() == Some("hover")).then(||color(&ctx.theme.markdown.link.color, ctx.theme.text)),
            on_link: ctx.on_link.clone(),
            highlight: ctx.highlight_set.clone(),
            icons: flat.icons.clone(),
            caret: ctx.caret.take(),
        }),
    }
    .into_any_element()
}
