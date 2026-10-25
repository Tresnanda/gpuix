// Included into custom_elements/input.rs (Kybern patch): `<textarea codeEditor>`, the file-tab editor.
// The native edit model (selection, IME, clipboard, undo) is unchanged; this adds Syntect colours from a
// registered syntax document, a line-number gutter per logical line, optional no-wrap horizontal scrolling,
// viewport culling, and `revealLine`.
/// Props as the element received them; applied to the editor state on every render.
#[derive(Clone, Default, PartialEq)]
struct CodeEditorProps {
    enabled: bool,
    /// `softWrap={false}` turns wrapping off; absent means wrap.
    no_wrap: bool,
    line_numbers: bool,
    syntax_document: Option<String>,
    syntax_revision: u32,
    /// `"<line>:<nonce>"`; a new value scrolls that 1-based line near the top and puts the caret on it.
    reveal: Option<String>,
}

struct CodeRuns {
    content: String,
    revision: u32,
    theme_generation: u64,
    font: gpui::Font,
    plain: gpui::Hsla,
    runs: Vec<TextRun>,
}

#[derive(Default)]
struct CodeEditor {
    props: CodeEditorProps,
    theme: Theme,
    theme_generation: u64,
    lease: Option<std::sync::Arc<crate::syntax_document::Document>>,
    runs: Option<CodeRuns>,
    pending_reveal: Option<usize>,
    /// Gutter width of the last layout, 0 without line numbers.
    gutter: f32,
    font: Option<(gpui::Font, Pixels)>,
    /// What the last layout was made from; an unchanged frame does no text work at all.
    layout: Option<CodeLayout>,
    /// The hash (text and runs) of each line in `last_lines`, for the font and wrap width in `shaped_for`: the next layout moves
    /// unchanged lines over instead of shaping them again, so an edit reshapes only the lines it changed.
    line_ids: Vec<u64>,
    shaped_for: Option<(gpui::Font, Pixels, Option<Pixels>)>,
}

#[derive(PartialEq)]
struct CodeLayout {
    content: String,
    width: Pixels,
    line_height: Pixels,
    revision: Option<u32>,
    theme_generation: u64,
    marked: Option<Range<usize>>,
    decorations: usize,
}

const CODE_GUTTER_MIN: f32 = 44.0;
const CODE_GUTTER_PADDING_RIGHT: f32 = 10.0;

fn code_editor_prop(props: &mut CodeEditorProps, key: &str, value: &serde_json::Value) -> bool {
    match key {
        "codeEditor" => props.enabled = value.as_bool().unwrap_or(false),
        "softWrap" => props.no_wrap = value.as_bool() == Some(false),
        "showLineNumbers" => props.line_numbers = value.as_bool().unwrap_or(false),
        "syntaxDocument" => props.syntax_document = value.as_str().map(str::to_owned),
        "syntaxRevision" => props.syntax_revision = value.as_u64().unwrap_or(0) as u32,
        "revealLine" => props.reveal = value.as_str().map(str::to_owned),
        _ => return false,
    }
    true
}

impl TextEditorState {
    fn apply_code_props(&mut self, props: &CodeEditorProps, theme: &Theme, theme_generation: u64, cx: &mut Context<Self>) {
        let code = &mut self.code;
        if code.theme_generation != theme_generation {
            code.theme = theme.clone();
            code.theme_generation = theme_generation;
            cx.notify();
        }
        if code.props == *props {
            if code.lease.is_none() { code.lease = code_lookup(props); }
            return;
        }
        if code.props.reveal != props.reveal {
            code.pending_reveal = props.reveal.as_deref().and_then(|value| value.split(':').next()?.parse::<usize>().ok()).map(|line| line.saturating_sub(1));
        }
        if (&code.props.syntax_document, code.props.syntax_revision) != (&props.syntax_document, props.syntax_revision) {
            code.lease = code_lookup(props);
        }
        code.props = props.clone();
        cx.notify();
    }

    fn code_scrolls_x(&self) -> bool { self.code.props.enabled && self.code.props.no_wrap }

    /// Called at the start of every layout: the wrap width (the text column, right of the gutter).
    fn code_wrap_width(&mut self, width: Pixels, style: &TextStyle, font_size: Pixels, window: &mut Window) -> Option<Pixels> {
        if !self.code.props.enabled {
            return self.multiline.then_some(width);
        }
        let font = style.font();
        self.code.gutter = if self.code.props.line_numbers {
            let digit = window.text_system().shape_line("0".into(), font_size, &[TextRun { len: 1, font: font.clone(), color: style.color, background_color: None, underline: None, strikethrough: None }], None).width;
            let lines = self.content.bytes().filter(|b| *b == b'\n').count() + 1;
            let digits = lines.to_string().len() as f32;
            (digits * f32::from(digit) + CODE_GUTTER_PADDING_RIGHT + 6.0).max(CODE_GUTTER_MIN).ceil()
        } else { 0.0 };
        self.code.font = Some((font, font_size));
        (!self.code.props.no_wrap).then(|| (width - px(self.code.gutter)).max(px(1.0)))
    }

    /// The code editor's layout, at prepaint with its final width (measure never shapes text: the editor fills its box). Each
    /// logical line is shaped once per text, runs, font and wrap width, so typing in a 5,000-line file reshapes one line.
    fn code_prepare(&mut self, full_width: Pixels, window: &mut Window) {
        if !self.code.props.enabled { return; }
        use std::hash::{Hash, Hasher};
        let style = window.text_style();
        let rem = window.rem_size();
        let font_size = style.font_size.to_pixels(rem);
        let line_height = style.line_height_in_pixels(rem);
        let wrap = self.code_wrap_width(full_width, &style, font_size, window);
        let color = style.color;
        let syntax = self.code_runs(&style, color);
        let revision = if syntax { self.code.runs.as_ref().map(|runs| runs.revision) } else { None };
        if let Some(last) = &self.code.layout {
            if last.width == full_width && last.line_height == line_height && last.revision == revision && last.theme_generation == self.code.theme_generation
                && last.marked == self.marked_range && last.decorations == self.decorations.len() && last.content == self.content {
                return;
            }
        }
        let font = style.font();
        let mut previous = std::mem::take(&mut self.last_lines);
        let mut pool: std::collections::HashMap<u64, Vec<usize>> = std::collections::HashMap::new();
        if self.code.shaped_for.as_ref() == Some(&(font.clone(), font_size, wrap)) && previous.len() == self.code.line_ids.len() {
            for (index, id) in self.code.line_ids.iter().enumerate().rev() { pool.entry(*id).or_default().push(index); }
        }
        self.code.shaped_for = Some((font.clone(), font_size, wrap));
        let runs = if syntax { std::mem::take(&mut self.code.runs.as_mut().unwrap().runs) } else { self.decorated_runs(&style, color) };
        let mut ids = Vec::with_capacity(previous.len().max(1));
        let mut lines = Vec::with_capacity(previous.len().max(1));
        let mut line_starts = Vec::with_capacity(previous.len().max(1));
        let (mut run, mut run_left) = (0usize, runs.first().map(|r| r.len).unwrap_or(0));
        let mut start = 0;
        for text in self.content.split('\n') {
            let end = start + text.len();
            // This line's slice of the runs (they cover the whole buffer, newlines included).
            let mut own: Vec<TextRun> = Vec::new();
            let mut at = start;
            while at < end && run < runs.len() {
                let take = run_left.min(end - at);
                if take > 0 { let mut piece = runs[run].clone(); piece.len = take; own.push(piece); }
                at += take; run_left -= take;
                if run_left == 0 { run += 1; run_left = runs.get(run).map(|r| r.len).unwrap_or(0); }
            }
            if own.is_empty() { own.push(TextRun { len: 0, font: font.clone(), color, background_color: None, underline: None, strikethrough: None }); }
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            text.hash(&mut hasher);
            for piece in &own {
                piece.len.hash(&mut hasher);
                for v in [piece.color.h, piece.color.s, piece.color.l, piece.color.a] { v.to_bits().hash(&mut hasher); }
                piece.underline.is_some().hash(&mut hasher);
                piece.font.weight.0.to_bits().hash(&mut hasher);
            }
            let id = hasher.finish();
            let reused = pool.get_mut(&id).and_then(Vec::pop).map(|index| std::mem::take(&mut previous[index]));
            let line = reused.unwrap_or_else(|| {
                window.text_system().shape_text(SharedString::from(text.to_owned()), font_size, &own, wrap, None).ok()
                    .and_then(|mut shaped| (!shaped.is_empty()).then(|| shaped.remove(0))).unwrap_or_default()
            });
            lines.push(line);
            ids.push(id);
            line_starts.push(start);
            start = end + 1;
            // Skip the newline's run byte.
            if run < runs.len() && end < self.content.len() {
                run_left = run_left.saturating_sub(1);
                if run_left == 0 { run += 1; run_left = runs.get(run).map(|r| r.len).unwrap_or(0); }
            }
        }
        if syntax { if let Some(cache) = self.code.runs.as_mut() { cache.runs = runs; } }
        self.font_size = font_size;
        self.line_height = line_height;
        self.content_height = lines.iter().map(|line| f32::from(line.size(line_height).height)).sum::<f32>().max(f32::from(line_height));
        self.content_width = lines.iter().map(|line| f32::from(line.unwrapped_layout.width)).fold(0.0, f32::max);
        self.display_is_placeholder = false;
        self.last_lines = lines;
        self.line_starts = line_starts;
        self.code.line_ids = ids;
        self.code.layout = Some(CodeLayout { content: self.content.clone(), width: full_width, line_height, revision, theme_generation: self.code.theme_generation, marked: self.marked_range.clone(), decorations: self.decorations.len() });
    }

    fn code_text_bounds(&self, bounds: Bounds<Pixels>) -> Bounds<Pixels> {
        if self.code.gutter <= 0.0 { return bounds; }
        let gutter = px(self.code.gutter).min(bounds.size.width);
        Bounds::new(point(bounds.left() + gutter, bounds.top()), gpui::size(bounds.size.width - gutter, bounds.size.height))
    }

    /// Syntax runs for the live buffer. A pending reparse keeps the last revision's colours outside the
    /// edited span (common prefix and suffix); only the changed text is plain until the new revision lands.
    fn code_runs(&mut self, style: &TextStyle, plain: gpui::Hsla) -> bool {
        if !self.code.props.enabled || self.secure || self.marked_range.is_some() { return false; }
        let Some(document) = self.code.lease.clone() else { return false; };
        let Some(highlight) = document.highlight.clone() else { return false; };
        let font = style.font();
        if let Some(cache) = &self.code.runs {
            if cache.revision == document.revision && cache.theme_generation == self.code.theme_generation && cache.font == font
                && cache.plain == plain && cache.content == self.content {
                return true;
            }
        }
        let old = document.source.as_str();
        let new = self.content.as_str();
        let mut prefix = old.bytes().zip(new.bytes()).take_while(|(a, b)| a == b).count();
        while !old.is_char_boundary(prefix) || !new.is_char_boundary(prefix) { prefix -= 1; }
        let room = old.len().min(new.len()) - prefix;
        let mut suffix = old.bytes().rev().zip(new.bytes().rev()).take(room).take_while(|(a, b)| a == b).count();
        while !old.is_char_boundary(old.len() - suffix) || !new.is_char_boundary(new.len() - suffix) { suffix -= 1; }
        let old_tail = old.len() - suffix;
        let new_tail = new.len() - suffix;
        let mut runs: Vec<TextRun> = Vec::new();
        let push = |runs: &mut Vec<TextRun>, len: usize, color: gpui::Hsla| {
            if len == 0 { return; }
            if let Some(last) = runs.last_mut() { if last.color == color { last.len += len; return; } }
            runs.push(TextRun { len, font: font.clone(), color, background_color: None, underline: None, strikethrough: None });
        };
        let mut at = 0;
        for (line, spans) in highlight.lines.iter().enumerate() {
            let Some(base) = document.offsets.get(line).copied() else { break; };
            for span in spans {
                let (start, end) = (base + span.range.start, base + span.range.end);
                let (start, end) = if end <= prefix { (start, end) } else if start >= old_tail { (start - old_tail + new_tail, end - old_tail + new_tail) } else { continue };
                if start < at || end > new.len() || start >= end { continue; }
                push(&mut runs, start - at, plain);
                push(&mut runs, end - start, self.code.theme.syntax.color(span.kind));
                at = end;
            }
        }
        push(&mut runs, new.len() - at, plain);
        self.code.runs = Some(CodeRuns { content: self.content.clone(), revision: document.revision, theme_generation: self.code.theme_generation, font, plain, runs });
        true
    }

    fn code_apply_reveal(&mut self) {
        let Some(line) = self.code.pending_reveal.take() else { return; };
        if self.last_lines.is_empty() { return; }
        let line = line.min(self.last_lines.len() - 1);
        let top: f32 = self.last_lines.iter().take(line).map(|row| f32::from(row.size(self.line_height).height)).sum();
        self.scroll_top = (top - 3.0 * f32::from(self.line_height)).max(0.0);
        let offset = self.line_starts.get(line).copied().unwrap_or(0).min(self.content.len());
        self.selected_range = offset..offset;
        self.selection_reversed = false;
        self.follow_cursor = false;
    }

    /// Both axes when lines do not wrap; otherwise the stock vertical scroll.
    fn code_scroll_wheel(&mut self, event: &ScrollWheelEvent, cx: &mut Context<Self>) -> bool {
        if !self.code_scrolls_x() { return false; }
        let Some(bounds) = self.last_bounds else { return true; };
        let delta = event.delta.pixel_delta(self.line_height);
        let top = (self.scroll_top - f32::from(delta.y)).clamp(0.0, (self.content_height - f32::from(bounds.size.height)).max(0.0));
        let left = (self.scroll_left - f32::from(delta.x)).clamp(0.0, (self.content_width + 2.0 - f32::from(bounds.size.width)).max(0.0));
        cx.stop_propagation();
        if top != self.scroll_top || left != self.scroll_left {
            self.scroll_top = top;
            self.scroll_left = left;
            self.follow_cursor = false;
            cx.notify();
        }
        true
    }

    /// Line numbers to paint: the right edge of each visible logical line's first row, its number, its colour.
    fn code_gutter_rows(&self, full: Bounds<Pixels>) -> Option<(gpui::Font, Pixels, Pixels, Vec<(Point<Pixels>, String, gpui::Hsla)>)> {
        if !self.code.props.enabled || !self.code.props.line_numbers || self.code.gutter <= 0.0 { return None; }
        let (font, font_size) = self.code.font.clone()?;
        let right = full.left() + px(self.code.gutter - CODE_GUTTER_PADDING_RIGHT);
        let cursor = self.cursor_offset();
        let current = self.line_starts.partition_point(|start| *start <= cursor).saturating_sub(1);
        let mut rows = Vec::new();
        let mut y = full.top() - px(self.scroll_top);
        for (index, line) in self.last_lines.iter().enumerate() {
            let height = line.size(self.line_height).height;
            if y + height < full.top() { y += height; continue; }
            if y > full.bottom() { break; }
            let color = if index == current { self.code.theme.text_muted } else { self.code.theme.text_faint };
            rows.push((point(right, y), (index + 1).to_string(), color));
            y += height;
        }
        Some((font, font_size, self.line_height, rows))
    }
}

fn code_lookup(props: &CodeEditorProps) -> Option<std::sync::Arc<crate::syntax_document::Document>> {
    crate::syntax_document::lookup(props.syntax_document.as_deref()?, props.syntax_revision)
}

fn paint_code_gutter(rows: Option<(gpui::Font, Pixels, Pixels, Vec<(Point<Pixels>, String, gpui::Hsla)>)>, full: Bounds<Pixels>, window: &mut Window, cx: &mut App) {
    let Some((font, font_size, line_height, rows)) = rows else { return; };
    window.with_content_mask(Some(gpui::ContentMask { bounds: full, ..Default::default() }), |window| {
        for (right, text, color) in rows {
            let run = TextRun { len: text.len(), font: font.clone(), color, background_color: None, underline: None, strikethrough: None };
            let line = window.text_system().shape_line(text.into(), font_size, &[run], None);
            line.paint(point(right.x - line.width, right.y), line_height, gpui::TextAlign::Left, None, window, cx).ok();
        }
    });
}
