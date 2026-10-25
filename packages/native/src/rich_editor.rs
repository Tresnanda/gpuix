//! `<rich-editor>`: one native editor for formatted Markdown documents, shared by
//! the note editor, task descriptions and the dock's Notes tab.
//!
//! The document model is `rich_doc.rs` (blocks of styled runs, ProseMirror-style
//! editing rules, history). This file lays it out, paints it the way the desktop's
//! `.note-prose` reads (desktop `styles/notes.css`), and connects it to keyboard,
//! mouse, clipboard and the platform input method. Markdown parsing and the
//! lossless write back are JavaScript's (`src/editor/markdown.ts`); the two sides
//! exchange the block JSON documented in `src/editor/README.md`.
//!
//! Layout is per block and cached by the block's revision, so a keystroke reshapes
//! one block; painting skips blocks outside the content mask.

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use gpui::{
    actions, div, fill, point, prelude::*, px, relative, size, App, Bounds, ClipboardEntry,
    ClipboardItem, Context, Corners, CursorStyle, DispatchPhase, ElementInputHandler, Entity,
    EntityInputHandler, FocusHandle, Font, FontStyle, FontWeight, GlobalElementId, Hitbox,
    HitboxBehavior, Hsla, KeyBinding, LayoutId, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, Pixels, Point, SharedString, StrikethroughStyle, Style, Task, TextRun,
    UTF16Selection, Window, WrappedLine,
};
use unicode_segmentation::UnicodeSegmentation;
use web_time::Instant;

use crate::custom_elements::{CustomElement, CustomElementFactory, CustomRenderContext};
use crate::renderer::{emit_event_full, EventCallback};
use crate::rich_doc::*;

actions!(
    gpuix_rich_editor,
    [
        Backspace,
        Delete,
        Left,
        Right,
        Up,
        Down,
        SelectLeft,
        SelectRight,
        SelectUp,
        SelectDown,
        SelectAll,
        Home,
        End,
        DocStart,
        DocEnd,
        SelectHome,
        SelectEnd,
        SelectDocStart,
        SelectDocEnd,
        WordLeft,
        WordRight,
        SelectWordLeft,
        SelectWordRight,
        DeleteWordLeft,
        DeleteWordRight,
        DeleteToLineStart,
        Copy,
        Cut,
        Paste,
        PastePlain,
        Undo,
        Redo,
        Enter,
        HardBreak,
        Indent,
        Outdent,
        Bold,
        Italic,
        Strike,
        InlineCode,
        Heading1,
        Heading2,
        Heading3,
        Paragraph,
        BulletList,
        OrderedList,
        TaskList,
        Blockquote,
        CodeBlock,
        MakeTask,
    ]
);

const KEY_CONTEXT: &str = "GpuixRichEditor";
const CARET_BLINK_MS: u64 = 500;
const CARET_WIDTH: f32 = 1.5;

thread_local! {
    static BOUND: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// The editor's keybindings, bound when the first editor appears. Bound at startup they
/// changed scoped-paint behaviour elsewhere (`native/paint-check.ts` saw ancestor island
/// renders on paint-only updates), so windows without an editor never carry them.
fn ensure_bindings(cx: &mut App) {
    if !BOUND.with(|b| b.replace(true)) {
        init(cx);
    }
}

pub fn init(cx: &mut App) {
    let c = Some(KEY_CONTEXT);
    let word = if cfg!(target_os = "macos") { "alt" } else { "ctrl" };
    let mut bindings = vec![
        KeyBinding::new("backspace", Backspace, c),
        KeyBinding::new("shift-backspace", Backspace, c),
        KeyBinding::new("delete", Delete, c),
        KeyBinding::new("left", Left, c),
        KeyBinding::new("right", Right, c),
        KeyBinding::new("up", Up, c),
        KeyBinding::new("down", Down, c),
        KeyBinding::new("shift-left", SelectLeft, c),
        KeyBinding::new("shift-right", SelectRight, c),
        KeyBinding::new("shift-up", SelectUp, c),
        KeyBinding::new("shift-down", SelectDown, c),
        KeyBinding::new("home", Home, c),
        KeyBinding::new("end", End, c),
        KeyBinding::new("shift-home", SelectHome, c),
        KeyBinding::new("shift-end", SelectEnd, c),
        KeyBinding::new("cmd-left", Home, c),
        KeyBinding::new("cmd-right", End, c),
        KeyBinding::new("ctrl-a", Home, c),
        KeyBinding::new("ctrl-e", End, c),
        KeyBinding::new("cmd-up", DocStart, c),
        KeyBinding::new("cmd-down", DocEnd, c),
        KeyBinding::new("shift-cmd-left", SelectHome, c),
        KeyBinding::new("shift-cmd-right", SelectEnd, c),
        KeyBinding::new("shift-cmd-up", SelectDocStart, c),
        KeyBinding::new("shift-cmd-down", SelectDocEnd, c),
        KeyBinding::new(&format!("{word}-left"), WordLeft, c),
        KeyBinding::new(&format!("{word}-right"), WordRight, c),
        KeyBinding::new(&format!("shift-{word}-left"), SelectWordLeft, c),
        KeyBinding::new(&format!("shift-{word}-right"), SelectWordRight, c),
        KeyBinding::new(&format!("{word}-backspace"), DeleteWordLeft, c),
        KeyBinding::new(&format!("{word}-delete"), DeleteWordRight, c),
        KeyBinding::new("cmd-backspace", DeleteToLineStart, c),
        KeyBinding::new("enter", Enter, c),
        KeyBinding::new("shift-enter", HardBreak, c),
        KeyBinding::new("cmd-enter", HardBreak, c),
        KeyBinding::new("tab", Indent, c),
        KeyBinding::new("shift-tab", Outdent, c),
        // TipTap StarterKit's shortcuts (desktop editor).
        KeyBinding::new("cmd-b", Bold, c),
        KeyBinding::new("cmd-i", Italic, c),
        KeyBinding::new("shift-cmd-s", Strike, c),
        KeyBinding::new("shift-cmd-x", Strike, c),
        KeyBinding::new("cmd-e", InlineCode, c),
        KeyBinding::new("alt-cmd-1", Heading1, c),
        KeyBinding::new("alt-cmd-2", Heading2, c),
        KeyBinding::new("alt-cmd-3", Heading3, c),
        KeyBinding::new("alt-cmd-0", Paragraph, c),
        KeyBinding::new("shift-cmd-8", BulletList, c),
        KeyBinding::new("shift-cmd-7", OrderedList, c),
        KeyBinding::new("shift-cmd-9", TaskList, c),
        KeyBinding::new("shift-cmd-b", Blockquote, c),
        KeyBinding::new("alt-cmd-c", CodeBlock, c),
        KeyBinding::new("shift-cmd-v", PastePlain, c),
        KeyBinding::new("shift-cmd-t", MakeTask, c),
    ];
    // ctrl-a/ctrl-e are Emacs line motion on macOS, so the shortcuts below take cmd there.
    let prefixes: &[&str] = if cfg!(target_os = "macos") { &["cmd"] } else { &["cmd", "ctrl"] };
    for prefix in prefixes {
        bindings.extend([
            KeyBinding::new(&format!("{prefix}-a"), SelectAll, c),
            KeyBinding::new(&format!("{prefix}-c"), Copy, c),
            KeyBinding::new(&format!("{prefix}-x"), Cut, c),
            KeyBinding::new(&format!("{prefix}-v"), Paste, c),
            KeyBinding::new(&format!("{prefix}-z"), Undo, c),
            KeyBinding::new(&format!("shift-{prefix}-z"), Redo, c),
            KeyBinding::new(&format!("{prefix}-y"), Redo, c),
        ]);
    }
    cx.bind_keys(bindings);
}

// ---------------------------------------------------------------- theme ----

#[derive(Clone, PartialEq)]
struct DocTheme {
    fg: Hsla,
    fg1: Hsla,
    fg2: Hsla,
    fg3: Hsla,
    fg4: Hsla,
    link: Hsla,
    fill: Hsla,
    hair: Hsla,
    hover: Hsla,
    sel: Hsla,
    focus: Hsla,
    check_on: Hsla,
    surface: Hsla,
    selection: Hsla,
    caret: Hsla,
    font: SharedString,
    mono: SharedString,
    size: f32,
    line: f32,
    compact: bool,
    min_height: f32,
    reduced_motion: bool,
    color_key: u64,
}

fn color(value: Option<&serde_json::Value>, fallback: Hsla) -> Hsla {
    value
        .and_then(|v| v.as_str())
        .and_then(crate::color::parse_color_rgba)
        .map(Hsla::from)
        .unwrap_or(fallback)
}

impl DocTheme {
    fn from_prop(value: &serde_json::Value) -> Self {
        let g = |k: &str| value.get(k);
        let gray = |a: f32| gpui::hsla(0.0, 0.0, 0.9, a);
        let fg = color(g("fg"), gpui::hsla(0.0, 0.0, 0.95, 1.0));
        let num = |k: &str, d: f32| g(k).and_then(|v| v.as_f64()).map(|v| v as f32).unwrap_or(d);
        let mut theme = DocTheme {
            fg,
            fg1: color(g("fg1"), gray(0.86)),
            fg2: color(g("fg2"), gray(0.6)),
            fg3: color(g("fg3"), gray(0.42)),
            fg4: color(g("fg4"), gray(0.22)),
            link: color(g("link"), gpui::hsla(0.6, 0.8, 0.65, 1.0)),
            fill: color(g("fill"), gray(0.05)),
            hair: color(g("hair"), gray(0.07)),
            hover: color(g("hover"), gray(0.035)),
            sel: color(g("sel"), gray(0.075)),
            focus: color(g("focus"), gpui::hsla(0.6, 0.8, 0.6, 1.0)),
            check_on: color(g("checkOn"), gray(0.42)),
            surface: color(g("surface"), gpui::hsla(0.0, 0.0, 0.1, 1.0)),
            selection: color(g("selection"), gpui::hsla(0.6, 0.8, 0.65, 0.28)),
            caret: color(g("caret"), fg),
            font: g("font").and_then(|v| v.as_str()).unwrap_or(".SystemUIFont").to_string().into(),
            mono: g("mono").and_then(|v| v.as_str()).unwrap_or("Menlo").to_string().into(),
            size: num("size", 16.1538).clamp(8.0, 40.0),
            line: num("line", 1.6).clamp(1.0, 3.0),
            compact: g("compact").and_then(|v| v.as_bool()).unwrap_or(false),
            min_height: num("minHeight", 0.0).max(0.0),
            reduced_motion: g("reducedMotion").and_then(|v| v.as_bool()).unwrap_or(false),
            color_key: 0,
        };
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        value.to_string().hash(&mut hasher);
        theme.color_key = hasher.finish();
        theme
    }
}

impl Default for DocTheme {
    fn default() -> Self {
        DocTheme::from_prop(&serde_json::Value::Null)
    }
}

/// Live task data for task references: `{ id: { key, status, title, state, label } }`.
#[derive(Clone, Default, PartialEq)]
struct TaskInfo {
    key: String,
    status: String,
    status_label: String,
    state: String,
}

#[derive(Clone)]
struct Glyph {
    image: Arc<gpui::RenderImage>,
}

/// A CSS color the SVG rasterizer understands (it has no `oklch()`).
fn svg_color(color: &str) -> String {
    match crate::color::parse_color_rgba(color) {
        Some(c) => format!("rgba({},{},{},{})", (c.r * 255.0).round(), (c.g * 255.0).round(), (c.b * 255.0).round(), c.a),
        None => color.to_string(),
    }
}

// ------------------------------------------------------------- doc JSON ----

fn marks_from(value: Option<&serde_json::Value>) -> u8 {
    let mut marks = 0;
    for m in value.and_then(|v| v.as_array()).into_iter().flatten() {
        match m.as_str() {
            Some("bold") => marks |= BOLD,
            Some("italic") => marks |= ITALIC,
            Some("strike") => marks |= STRIKE,
            Some("code") => marks |= CODE,
            _ => {}
        }
    }
    marks
}

fn marks_json(marks: u8) -> serde_json::Value {
    let mut out = Vec::new();
    for (bit, name) in [(BOLD, "bold"), (ITALIC, "italic"), (STRIKE, "strike"), (CODE, "code")] {
        if marks & bit != 0 {
            out.push(serde_json::Value::from(name));
        }
    }
    serde_json::Value::Array(out)
}

fn str_of(value: &serde_json::Value, key: &str) -> String {
    value.get(key).and_then(|v| v.as_str()).unwrap_or_default().to_string()
}

fn opt_str(value: &serde_json::Value, key: &str) -> Option<String> {
    value.get(key).and_then(|v| v.as_str()).map(str::to_string)
}

fn atom_from(value: &serde_json::Value) -> Option<Atom> {
    match value.get("type")?.as_str()? {
        "taskRef" => Some(Atom::TaskRef { id: str_of(value, "id"), label: str_of(value, "label") }),
        "image" => Some(Atom::Image { src: str_of(value, "src"), alt: str_of(value, "alt"), title: opt_str(value, "title") }),
        "raw" => Some(Atom::Raw { source: str_of(value, "source") }),
        _ => None,
    }
}

fn atom_json(atom: &Atom) -> serde_json::Value {
    match atom {
        Atom::TaskRef { id, label } => serde_json::json!({"type": "taskRef", "id": id, "label": label}),
        Atom::Image { src, alt, title } => serde_json::json!({"type": "image", "src": src, "alt": alt, "title": title}),
        Atom::Raw { source } => serde_json::json!({"type": "raw", "source": source}),
    }
}

fn link_from(value: Option<&serde_json::Value>) -> Option<Rc<Link>> {
    let value = value?;
    let href = value.get("href")?.as_str()?.to_string();
    Some(Rc::new(Link { href, title: opt_str(value, "title") }))
}

fn link_json(link: &Option<Rc<Link>>) -> serde_json::Value {
    match link {
        Some(link) => serde_json::json!({"href": link.href, "title": link.title}),
        None => serde_json::Value::Null,
    }
}

fn kind_from(value: &serde_json::Value) -> Kind {
    let n = |k: &str, d: u64| value.get(k).and_then(|v| v.as_u64()).unwrap_or(d);
    match value.get("type").and_then(|v| v.as_str()).unwrap_or("paragraph") {
        "heading" => Kind::Heading(n("level", 1).clamp(1, 6) as u8),
        "bullet" => Kind::Bullet,
        "ordered" => Kind::Ordered(n("start", 1) as u32),
        "task" => Kind::Task(value.get("checked").and_then(|v| v.as_bool()).unwrap_or(false)),
        "code" => Kind::Code(str_of(value, "lang")),
        "rule" => Kind::Rule,
        "image" => Kind::Image { src: str_of(value, "src"), alt: str_of(value, "alt"), title: opt_str(value, "title") },
        "raw" => Kind::Raw,
        _ => Kind::Paragraph,
    }
}

fn block_from(value: &serde_json::Value, fallback_id: u64) -> Block {
    let kind = kind_from(value);
    let id = value.get("id").and_then(|v| v.as_u64()).unwrap_or(fallback_id);
    let mut block = Block::new(id, kind.clone(), "");
    if kind.is_literal() {
        block = Block::new(id, kind, &str_of(value, "text"));
    } else if !kind.is_atom() {
        let mut text = String::new();
        let mut runs = Vec::new();
        for piece in value.get("content").and_then(|v| v.as_array()).into_iter().flatten() {
            let marks = marks_from(piece.get("marks"));
            let link = link_from(piece.get("link"));
            if let Some(atom) = piece.get("atom").and_then(atom_from) {
                text.push_str(ATOM_STR);
                runs.push(Run::atom(atom, marks, link));
            } else if let Some(t) = piece.get("text").and_then(|v| v.as_str()) {
                // U+FFFC is reserved for atoms.
                let t = t.replace(ATOM, "");
                if !t.is_empty() {
                    text.push_str(&t);
                    runs.push(Run::text(t.len(), marks, link));
                }
            }
        }
        block.text = text;
        block.runs = runs;
    }
    let n = |k: &str| value.get(k).and_then(|v| v.as_u64()).unwrap_or(0).min(16) as u8;
    block.depth = n("depth");
    block.quote = n("quote");
    block.cont = value.get("cont").and_then(|v| v.as_bool()).unwrap_or(false);
    block
}

fn doc_from_json(value: &serde_json::Value) -> Doc {
    let blocks = value
        .get("blocks")
        .and_then(|v| v.as_array())
        .map(|blocks| blocks.iter().enumerate().map(|(i, b)| block_from(b, i as u64 + 1)).collect())
        .unwrap_or_default();
    Doc::new(blocks)
}

fn block_json(block: &Block) -> serde_json::Value {
    let mut out = serde_json::Map::new();
    out.insert("id".into(), block.id.into());
    let (ty, extra): (&str, Vec<(&str, serde_json::Value)>) = match &block.kind {
        Kind::Paragraph => ("paragraph", vec![]),
        Kind::Heading(level) => ("heading", vec![("level", (*level).into())]),
        Kind::Bullet => ("bullet", vec![]),
        Kind::Ordered(start) => ("ordered", vec![("start", (*start).into())]),
        Kind::Task(checked) => ("task", vec![("checked", (*checked).into())]),
        Kind::Code(lang) => ("code", vec![("lang", lang.clone().into())]),
        Kind::Rule => ("rule", vec![]),
        Kind::Image { src, alt, title } => ("image", vec![("src", src.clone().into()), ("alt", alt.clone().into()), ("title", title.clone().into())]),
        Kind::Raw => ("raw", vec![]),
    };
    out.insert("type".into(), ty.into());
    for (k, v) in extra {
        out.insert(k.into(), v);
    }
    if block.depth > 0 {
        out.insert("depth".into(), block.depth.into());
    }
    if block.quote > 0 {
        out.insert("quote".into(), block.quote.into());
    }
    if block.cont {
        out.insert("cont".into(), true.into());
    }
    if block.kind.is_literal() {
        out.insert("text".into(), block.text.clone().into());
    } else if !block.kind.is_atom() {
        let mut content = Vec::new();
        let mut at = 0;
        for run in &block.runs {
            let mut piece = serde_json::Map::new();
            if let Some(atom) = &run.atom {
                piece.insert("atom".into(), atom_json(atom));
            } else {
                piece.insert("text".into(), block.text[at..at + run.len].to_string().into());
            }
            if run.marks != 0 {
                piece.insert("marks".into(), marks_json(run.marks));
            }
            if run.link.is_some() {
                piece.insert("link".into(), link_json(&run.link));
            }
            content.push(serde_json::Value::Object(piece));
            at += run.len;
        }
        out.insert("content".into(), content.into());
    }
    serde_json::Value::Object(out)
}

fn doc_json(doc: &Doc) -> serde_json::Value {
    serde_json::json!({ "blocks": doc.blocks.iter().map(|b| block_json(b)).collect::<Vec<_>>() })
}

/// A slice of the document for the clipboard: whole blocks, the first and last trimmed.
fn slice_json(doc: &Doc, start: Pos, end: Pos) -> serde_json::Value {
    let mut blocks = Vec::new();
    for index in start.block..=end.block {
        let block = doc.block(index);
        let a = if index == start.block { start.offset } else { 0 };
        let b = if index == end.block { end.offset } else { block.len() };
        let mut piece = block.clone();
        if !block.kind.is_atom() {
            let (text, runs) = block.slice(a, b);
            piece.text = text;
            piece.runs = runs;
        } else if a == b {
            continue;
        }
        blocks.push(block_json(&piece));
    }
    serde_json::json!({ "kybernRichSlice": 1, "blocks": blocks })
}

// ---------------------------------------------------------- image cache ----

enum ImageEntry {
    Ready(Arc<gpui::RenderImage>),
    Failed,
}

thread_local! {
    static IMAGES: RefCell<(HashMap<String, ImageEntry>, VecDeque<String>)> = RefCell::new((HashMap::new(), VecDeque::new()));
    static IMAGE_DROPS: RefCell<Vec<Arc<gpui::RenderImage>>> = const { RefCell::new(Vec::new()) };
    static IMAGE_GENERATION: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    static EDITORS: RefCell<HashMap<u64, gpui::WeakEntity<RichEditorState>>> = RefCell::new(HashMap::new());
}

const IMAGE_LIMIT: usize = 48;

fn image_for(src: &str) -> Option<Result<Arc<gpui::RenderImage>, ()>> {
    IMAGES.with(|cache| {
        let cache = cache.borrow();
        match cache.0.get(src)? {
            ImageEntry::Ready(image) => Some(Ok(image.clone())),
            ImageEntry::Failed => Some(Err(())),
        }
    })
}

fn store_image(src: String, entry: ImageEntry, cx: &App) {
    let protected = editor_image_sources(cx);
    IMAGES.with(|cache| {
        let mut cache = cache.borrow_mut();
        let (map, order) = &mut *cache;
        order.retain(|s| s != &src);
        order.push_back(src.clone());
        if let Some(ImageEntry::Ready(old)) = map.insert(src, entry) {
            IMAGE_DROPS.with(|d| d.borrow_mut().push(old));
        }
        let mut bytes: usize = map.iter().map(|(src, entry)| editor_image_cost(src, entry)).sum();
        let candidates: Vec<_> = order.iter().filter(|src| !protected.contains(*src)).cloned().collect();
        for oldest in candidates {
            if order.len() <= IMAGE_LIMIT && bytes <= EDITOR_IMAGE_BYTES { break; }
            order.retain(|src| src != &oldest);
            if let Some(entry) = map.remove(&oldest) {
                bytes = bytes.saturating_sub(editor_image_cost(&oldest, &entry));
                if let ImageEntry::Ready(old) = entry { IMAGE_DROPS.with(|d| d.borrow_mut().push(old)); }
            }
        }
    });
    IMAGE_GENERATION.with(|g| g.set(g.get() + 1));
}

/// The largest side an image keeps: the 640px column at Retina scale, so it samples 1:1.
/// GPUI samples images without mipmaps, so a screenshot shrunk 4× shimmers; WebKit does not.
const IMAGE_MAX_SIDE: u32 = 1280;

/// Area-average a large image down to `IMAGE_MAX_SIDE`, keeping its aspect ratio.
fn downscaled(image: Arc<gpui::RenderImage>) -> Arc<gpui::RenderImage> {
    let s = image.size(0);
    let (w, h) = (s.width.0.max(1) as u32, s.height.0.max(1) as u32);
    let factor = (w.max(h) as f32 / IMAGE_MAX_SIDE as f32).max(1.0);
    let Some(src) = image.as_bytes(0).filter(|_| factor > 1.0 && image.frame_count() == 1) else { return image };
    let (nw, nh) = (((w as f32 / factor).round() as u32).max(1), ((h as f32 / factor).round() as u32).max(1));
    let mut out = vec![0u8; (nw * nh * 4) as usize];
    for y in 0..nh {
        let (y0, y1) = ((y * h / nh) as usize, (((y + 1) * h / nh).max(y * h / nh + 1)) as usize);
        for x in 0..nw {
            let (x0, x1) = ((x * w / nw) as usize, (((x + 1) * w / nw).max(x * w / nw + 1)) as usize);
            let mut acc = [0u32; 4];
            for sy in y0..y1.min(h as usize) {
                let row = sy * w as usize * 4;
                for sx in x0..x1.min(w as usize) {
                    let i = row + sx * 4;
                    for c in 0..4 {
                        acc[c] += src[i + c] as u32;
                    }
                }
            }
            let n = ((y1.min(h as usize) - y0) * (x1.min(w as usize) - x0)).max(1) as u32;
            let o = ((y * nw + x) * 4) as usize;
            for c in 0..4 {
                out[o + c] = (acc[c] / n) as u8;
            }
        }
    }
    gpui::RenderImage::from_bgra(nw, nh, out).map(Arc::new).unwrap_or(image)
}

fn drain_image_drops(window: &mut Window) {
    let drops: Vec<_> = IMAGE_DROPS.with(|d| std::mem::take(&mut *d.borrow_mut()));
    for image in drops {
        window.drop_image(image).ok();
    }
}

/// `setRichEditorImage(src, bytes)`: a decoded image for every editor showing `src`.
/// Empty bytes record a failed load ("Image unavailable").
pub(crate) fn set_image(src: String, bytes: Vec<u8>, cx: &mut App) -> napi::Result<()> {
    if bytes.is_empty() {
        store_image(src, ImageEntry::Failed, cx);
    } else {
        let image = crate::custom_elements::img::render_image_from_encoded(bytes, cx.svg_renderer())
            .map_err(napi::Error::from_reason)?;
        store_image(src, ImageEntry::Ready(downscaled(image)), cx);
    }
    let editors: Vec<_> = EDITORS.with(|e| e.borrow().values().filter_map(|w| w.upgrade()).collect());
    for editor in editors {
        editor.update(cx, |state, cx| {
            state.layouts.clear();
            cx.notify();
        });
    }
    Ok(())
}

fn remember_editor(id: u64, state: &Entity<RichEditorState>) {
    EDITORS.with(|editors| {
        let mut editors = editors.borrow_mut();
        editors.retain(|_, editor| editor.upgrade().is_some());
        editors.insert(id, state.downgrade());
    });
}

fn editor_for(id: u64) -> napi::Result<Entity<RichEditorState>> {
    EDITORS
        .with(|editors| editors.borrow().get(&id).and_then(|e| e.upgrade()))
        .ok_or_else(|| napi::Error::from_reason("Rich editor is not mounted"))
}

// -------------------------------------------------------------- factory ----

pub struct RichEditorFactory;

impl CustomElementFactory for RichEditorFactory {
    fn element_type(&self) -> &str {
        "rich-editor"
    }

    fn create(&self, _id: u64) -> Box<dyn CustomElement> {
        Box::new(RichEditorElement::default())
    }
}

#[derive(Default)]
struct RichEditorElement {
    element_id: Option<u64>,
    doc: Option<serde_json::Value>,
    doc_epoch: Option<f64>,
    applied_epoch: Option<f64>,
    theme: DocTheme,
    placeholder: String,
    read_only: bool,
    tasks: HashMap<String, TaskInfo>,
    glyphs: HashMap<String, (String, String)>,
    task_keys: HashMap<String, String>,
    intercept_keys: Vec<String>,
    make_task: bool,
    state: Option<Entity<RichEditorState>>,
    key_interceptor: Option<gpui::Subscription>,
}

impl CustomElement for RichEditorElement {
    fn translate_subtree(&mut self, delta: Point<Pixels>, cx: &mut Context<crate::renderer::GpuixView>) {
        if let Some(state) = &self.state {
            state.update(cx, |state, _| {
                if let Some(bounds) = &mut state.last_bounds {
                    bounds.origin += delta;
                }
            });
        }
    }

    fn render(
        &mut self,
        ctx: CustomRenderContext,
        window: &mut Window,
        cx: &mut Context<crate::renderer::GpuixView>,
    ) -> gpui::AnyElement {
        crate::kybern_icon::set_renderer(cx.svg_renderer());
        ensure_bindings(cx);
        let focus_handle = ctx.focus_handle.cloned().unwrap_or_else(|| cx.focus_handle());
        let callback = ctx.event_callback.clone();
        let id = ctx.id;
        self.element_id = Some(id);
        let state = self
            .state
            .get_or_insert_with(|| {
                let focus = focus_handle.clone();
                let callback = callback.clone();
                cx.new(move |cx| RichEditorState::new(id, focus, callback, cx))
            })
            .clone();
        #[cfg(target_os = "macos")]
        if self.key_interceptor.is_none() {
            let weak = state.downgrade();
            self.key_interceptor = Some(cx.intercept_keystrokes(move |event, window, cx| {
                if crate::key_intercept::replaying() {
                    return;
                }
                let Some(state) = weak.upgrade() else { return };
                let state = state.read(cx);
                if state.emits_key_down
                    && state.core.marked.is_none()
                    && state.focus_handle.is_focused(window)
                    && (state.intercept_keys.contains(&event.keystroke.key) || crate::key_intercept::pending())
                {
                    crate::key_intercept::capture(
                        state.element_id,
                        state.focus_handle.clone(),
                        state.intercept_keys.contains(&event.keystroke.key),
                        window,
                        cx,
                    );
                }
            }));
        }
        remember_editor(id, &state);
        let epoch_changed = self.doc_epoch != self.applied_epoch;
        let doc = if epoch_changed { self.doc.take() } else { None };
        self.applied_epoch = self.doc_epoch;
        let emits_key_down = ctx.events.contains("keyDown");
        let emits_change = ctx.events.contains("change");
        let emits_link = ctx.events.contains("linkClick");
        let emits_drop = ctx.events.contains("fileDrop");
        state.update(cx, |state, cx| {
            state.callback = callback.clone();
            state.emits_key_down = emits_key_down;
            state.emits_change = emits_change;
            state.emits_link = emits_link;
            state.emits_drop = emits_drop;
            state.intercept_keys = self.intercept_keys.clone();
            if state.read_only != self.read_only {
                state.read_only = self.read_only;
                cx.notify();
            }
            if state.placeholder != self.placeholder {
                state.placeholder = self.placeholder.clone();
                cx.notify();
            }
            if state.theme != self.theme {
                state.theme = self.theme.clone();
                state.layouts.clear();
                cx.notify();
            }
            if state.tasks != self.tasks {
                state.tasks = self.tasks.clone();
                state.layouts.clear();
                cx.notify();
            }
            if state.glyph_source != self.glyphs {
                state.glyph_source = self.glyphs.clone();
                state.glyphs = self
                    .glyphs
                    .iter()
                    .filter_map(|(k, (svg, color))| crate::kybern_icon::image(svg, &svg_color(color)).map(|image| (k.clone(), Glyph { image })))
                    .collect();
                cx.notify();
            }
            state.core.task_keys = self.task_keys.clone();
            state.make_task = self.make_task;
            if let Some(doc) = doc {
                state.replace_doc(doc_from_json(&doc), cx);
            }
        });
        drain_image_drops(window);

        let element_id = SharedString::from(format!("__gpuix_rich_editor_{}", ctx.id));
        let mut editor = div().id(element_id).flex().flex_col().min_w_0().w_full().track_focus(&focus_handle).child(state);
        if let Some(style) = ctx.style {
            editor = crate::renderer::apply_interactive_styles(editor, style);
        }
        if ctx.style.and_then(|style| style.position.as_deref()).is_none() {
            editor = editor.relative();
        }
        editor = crate::accessibility::apply_accessibility(editor, ctx.props, Some(gpui::Role::MultilineTextInput));
        editor = editor.child(crate::automation::bounds_tracker(ctx.id, Some(false)));
        if emits_drop {
            let callback = ctx.event_callback.clone();
            editor = editor.on_drop(move |dropped: &gpui::ExternalPaths, window, _cx| {
                crate::renderer::emit_file_drop(&callback, id, dropped, window.mouse_position());
            });
        }
        editor.into_any_element()
    }

    fn set_prop(&mut self, key: &str, value: serde_json::Value) {
        match key {
            "doc" => {
                self.doc = match value {
                    serde_json::Value::String(s) => serde_json::from_str(&s).ok(),
                    serde_json::Value::Null => None,
                    other => Some(other),
                }
            }
            "docEpoch" => self.doc_epoch = value.as_f64(),
            "theme" => self.theme = DocTheme::from_prop(&value),
            "placeholder" => self.placeholder = value.as_str().unwrap_or_default().to_string(),
            "readOnly" => self.read_only = value.as_bool().unwrap_or(false),
            "makeTask" => self.make_task = value.as_bool().unwrap_or(false),
            "interceptKeys" => {
                self.intercept_keys = value
                    .as_array()
                    .map(|keys| keys.iter().filter_map(|k| k.as_str().map(str::to_owned)).collect())
                    .unwrap_or_default()
            }
            "tasks" => {
                self.tasks = value
                    .as_object()
                    .map(|map| {
                        map.iter()
                            .map(|(id, info)| {
                                (
                                    id.to_lowercase(),
                                    TaskInfo {
                                        key: str_of(info, "key"),
                                        status: str_of(info, "status"),
                                        status_label: str_of(info, "statusLabel"),
                                        state: info.get("state").and_then(|v| v.as_str()).unwrap_or("ok").to_string(),
                                    },
                                )
                            })
                            .collect()
                    })
                    .unwrap_or_default()
            }
            "glyphs" => {
                self.glyphs = value
                    .as_object()
                    .map(|map| map.iter().map(|(k, v)| (k.clone(), (str_of(v, "svg"), str_of(v, "color")))).collect())
                    .unwrap_or_default()
            }
            "taskKeys" => {
                self.task_keys = value
                    .as_object()
                    .map(|map| map.iter().filter_map(|(k, v)| v.as_str().map(|id| (k.clone(), id.to_string()))).collect())
                    .unwrap_or_default()
            }
            _ => {}
        }
    }

    fn supported_props(&self) -> &'static [&'static str] {
        &["doc", "docEpoch", "theme", "placeholder", "readOnly", "interceptKeys", "tasks", "glyphs", "taskKeys", "makeTask"]
    }

    fn supported_events(&self) -> &'static [&'static str] {
        &["change", "keyDown", "keyUp", "focus", "blur", "linkClick", "fileDrop"]
    }

    fn destroy(&mut self) {
        if let Some(id) = self.element_id.take() { EDITORS.with(|editors| { editors.borrow_mut().remove(&id); }); }
        self.state = None;
        self.key_interceptor = None;
    }
}

// --------------------------------------------------------------- layout ----

/// What one stretch of the shaped string stands for.
#[derive(Clone, Copy, Debug)]
enum Seg {
    /// Model text, one to one.
    Text { m: usize, d: usize, len: usize },
    /// An atom: `ATOM_LEN` model bytes shown as `dlen` display bytes.
    Atom { m: usize, d: usize, dlen: usize },
    /// Display-only room (inline code padding) at model offset `m`.
    Room { m: usize, d: usize, dlen: usize },
}

#[derive(Clone)]
struct Chip {
    /// Display range of the whole chip, glyph room included.
    range: Range<usize>,
    /// Display range of the glyph's room (the first two characters).
    glyph: Range<usize>,
    model: usize,
    glyph_key: String,
    task_id: String,
    clickable: bool,
}

#[derive(Default, Clone)]
struct Display {
    text: String,
    runs: Vec<TextRun>,
    segs: Vec<Seg>,
    chips: Vec<Chip>,
    codes: Vec<Range<usize>>,
    links: Vec<(Range<usize>, Rc<Link>)>,
}

impl Display {
    /// The display offset of a model offset. Room never owns a position: at a
    /// boundary the caret stays with the text or atom before it.
    fn m2d(&self, m: usize) -> usize {
        for seg in &self.segs {
            match *seg {
                Seg::Text { m: sm, d, len } if sm <= m && m <= sm + len => return d + (m - sm),
                Seg::Atom { m: sm, d, dlen } if sm <= m && m <= sm + ATOM_LEN => return if m == sm { d } else { d + dlen },
                _ => {}
            }
        }
        match self.segs.first() {
            Some(Seg::Room { d, .. }) if m == 0 => *d,
            _ => self.text.len(),
        }
    }

    /// The model offset for a display offset; inside an atom it snaps to the nearer edge.
    fn d2m(&self, d: usize) -> usize {
        let mut last = 0;
        for seg in &self.segs {
            match *seg {
                Seg::Text { m, d: sd, len } => {
                    if d >= sd && d <= sd + len {
                        return m + (d - sd);
                    }
                    last = m + len;
                }
                Seg::Atom { m, d: sd, dlen } => {
                    if d >= sd && d <= sd + dlen {
                        return if d - sd <= dlen / 2 { m } else { m + ATOM_LEN };
                    }
                    last = m + ATOM_LEN;
                }
                Seg::Room { m, d: sd, dlen } => {
                    if d >= sd && d <= sd + dlen {
                        return m;
                    }
                    last = m;
                }
            }
        }
        last
    }
}

/// How a block's text is set.
#[derive(Clone, Copy)]
struct BlockStyle {
    size: f32,
    line_h: f32,
    weight: f32,
    color: Hsla,
    mono: bool,
    wrap: bool,
}

fn block_style(block: &Block, theme: &DocTheme) -> BlockStyle {
    let s = theme.size;
    let base_lh = s * theme.line;
    let color = if block.quote > 0 { theme.fg2 } else { theme.fg1 };
    match &block.kind {
        Kind::Heading(1) => BlockStyle { size: s * 17.0 / 15.0, line_h: s * 17.0 / 15.0 * 1.3, weight: 650.0, color: theme.fg, mono: false, wrap: true },
        Kind::Heading(2) => BlockStyle { size: s * 17.0 / 15.0, line_h: s * 17.0 / 15.0 * 1.3, weight: 600.0, color: theme.fg, mono: false, wrap: true },
        Kind::Heading(_) => BlockStyle { size: s, line_h: s * 1.45, weight: 600.0, color: theme.fg, mono: false, wrap: true },
        Kind::Code(_) => BlockStyle { size: s * 12.5 / 15.0, line_h: s * 12.5 / 15.0 * 1.68, weight: 400.0, color: theme.fg1, mono: true, wrap: false },
        Kind::Raw => BlockStyle { size: s * 12.5 / 15.0, line_h: s * 12.5 / 15.0 * 1.68, weight: 400.0, color: theme.fg2, mono: true, wrap: true },
        Kind::Task(true) => BlockStyle { size: s, line_h: base_lh, weight: 400.0, color: theme.fg3, mono: false, wrap: true },
        _ => BlockStyle { size: s, line_h: base_lh, weight: 400.0, color, mono: false, wrap: true },
    }
}

fn font(family: &SharedString, weight: f32, italic: bool) -> Font {
    let mut font = gpui::font(family.clone());
    font.weight = FontWeight(weight);
    font.style = if italic { FontStyle::Italic } else { FontStyle::Normal };
    font
}

/// A font set at `per_mille`/1000 of the line's size (GPUI patch `text_scale_gpui.py`).
fn scaled(mut font: Font, per_mille: u32) -> Font {
    let mut features: Vec<(String, u32)> = font.features.0.iter().cloned().collect();
    features.push(("kszl".into(), per_mille));
    font.features = gpui::FontFeatures(Arc::new(features));
    font
}

/// `.note-prose code`: 0.833em; `.task-ref`: 13/15.
const CODE_SCALE: u32 = 833;
const CHIP_SCALE: u32 = 867;

fn run(len: usize, font: Font, color: Hsla) -> TextRun {
    TextRun { len, font, color, background_color: None, underline: None, strikethrough: None }
}

/// The shaped string for a block: model text with atoms expanded into their labels.
fn build_display(block: &Block, theme: &DocTheme, style: &BlockStyle, tasks: &HashMap<String, TaskInfo>, in_checklist: bool) -> Display {
    let mut out = Display::default();
    let base_font = if style.mono { font(&theme.mono, style.weight, false) } else { font(&theme.font, style.weight, false) };
    if block.kind.is_literal() || block.kind.is_atom() {
        if !block.kind.is_atom() && !block.text.is_empty() {
            out.text = block.text.clone();
            out.runs.push(run(block.text.len(), base_font, style.color));
            out.segs.push(Seg::Text { m: 0, d: 0, len: block.text.len() });
        }
        return out;
    }
    let mono_font = font(&theme.mono, 400.0, false);
    let mut m = 0;
    for r in &block.runs {
        let d = out.text.len();
        if let Some(atom) = &r.atom {
            match &**atom {
                Atom::TaskRef { id, label } => {
                    let info = tasks.get(&id.to_lowercase());
                    let state = info.map(|i| i.state.as_str()).unwrap_or("pending");
                    let key = info.map(|i| i.key.as_str()).filter(|k| !k.is_empty()).unwrap_or(label.as_str());
                    // Glyph room: two monospace spaces, then the key (and the status on checklist lines).
                    out.text.push_str("  ");
                    out.runs.push(run(2, mono_font.clone(), gpui::transparent_black()));
                    let key_color = if state == "ok" { theme.fg2 } else { theme.fg3 };
                    out.text.push_str(key);
                    let mut key_run = run(key.len(), scaled(font(&theme.font, style.weight, false), CHIP_SCALE), key_color);
                    if state == "missing" {
                        key_run.strikethrough = Some(StrikethroughStyle { thickness: px(1.0), color: Some(theme.fg4) });
                    }
                    out.runs.push(key_run);
                    if in_checklist && state == "ok" {
                        if let Some(label) = info.map(|i| i.status_label.as_str()).filter(|l| !l.is_empty()) {
                            let text = format!(" {label}");
                            out.text.push_str(&text);
                            out.runs.push(run(text.len(), scaled(font(&theme.font, style.weight, false), CHIP_SCALE), theme.fg3));
                        }
                    }
                    let glyph_key = if state == "ok" { info.map(|i| i.status.clone()).unwrap_or_default() } else { "pending".to_string() };
                    out.chips.push(Chip { range: d..out.text.len(), glyph: d..d + 2, model: m, glyph_key, task_id: id.clone(), clickable: state == "ok" });
                }
                Atom::Image { alt, .. } => {
                    let label = if alt.is_empty() { "Image".to_string() } else { alt.clone() };
                    out.text.push_str(&label);
                    out.runs.push(run(label.len(), font(&theme.font, style.weight, false), theme.fg2));
                }
                Atom::Raw { source } => {
                    let source = source.replace('\n', " ");
                    let source = if source.is_empty() { " ".to_string() } else { source };
                    out.text.push_str(&source);
                    out.runs.push(run(source.len(), mono_font.clone(), theme.fg3));
                }
            }
            let dlen = out.text.len() - d;
            out.segs.push(Seg::Atom { m, d, dlen });
            m += ATOM_LEN;
            continue;
        }
        let text = &block.text[m..m + r.len];
        let code = r.marks & CODE != 0;
        if code {
            // Room for the inline code's padding (`padding: 0.13em 0.33em`).
            let room = out.text.len();
            out.text.push(' ');
            out.runs.push(run(1, font(&theme.font, 400.0, false), gpui::transparent_black()));
            out.segs.push(Seg::Room { m, d: room, dlen: 1 });
        }
        let d = out.text.len();
        let bold = r.marks & BOLD != 0;
        let weight = if bold { style.weight.max(600.0) } else { style.weight };
        let f = if code { scaled(font(&theme.mono, 400.0, r.marks & ITALIC != 0), CODE_SCALE) } else { font(&theme.font, weight, r.marks & ITALIC != 0) };
        let mut c = if r.link.is_some() {
            theme.link
        } else if code || (bold && matches!(block.kind, Kind::Paragraph | Kind::Bullet | Kind::Ordered(_) | Kind::Task(false))) {
            if block.quote > 0 && !code { style.color } else { theme.fg }
        } else {
            style.color
        };
        if matches!(block.kind, Kind::Task(true)) && r.link.is_none() {
            c = theme.fg3;
        }
        let mut tr = run(text.len(), f, c);
        if r.marks & STRIKE != 0 || matches!(block.kind, Kind::Task(true)) {
            tr.strikethrough = Some(StrikethroughStyle { thickness: px(1.0), color: Some(if matches!(block.kind, Kind::Task(true)) { theme.fg4 } else { theme.fg3 }) });
        }
        out.text.push_str(text);
        out.runs.push(tr);
        out.segs.push(Seg::Text { m, d, len: text.len() });
        if code {
            out.codes.push(d..out.text.len());
            let room = out.text.len();
            out.text.push(' ');
            out.runs.push(run(1, font(&theme.font, 400.0, false), gpui::transparent_black()));
            out.segs.push(Seg::Room { m: m + r.len, d: room, dlen: 1 });
        }
        if let Some(link) = &r.link {
            let merge = out.links.last().is_some_and(|(range, last)| range.end == d && **last == **link);
            if merge {
                out.links.last_mut().unwrap().0.end = out.text.len();
            } else {
                out.links.push((d..out.text.len(), link.clone()));
            }
        }
        m += r.len;
    }
    // Hard breaks are model "\n"; the shaper splits lines on them.
    out
}

struct Lay {
    rev: u64,
    width: u32,
    theme_key: u64,
    image_gen: u64,
    disp: Display,
    lines: Vec<WrappedLine>,
    /// Display offset where each hard line starts.
    starts: Vec<usize>,
    style: BlockStyle,
    text_h: f32,
    /// Box height for atom blocks (images, rules).
    atom_h: f32,
    image: Option<Arc<gpui::RenderImage>>,
    image_size: (f32, f32),
    image_failed: bool,
}

/// Where a block sits, relative to the element.
#[derive(Clone, Copy, Default)]
struct Place {
    top: f32,
    /// Height of the block box (padding included).
    height: f32,
    /// Text origin.
    tx: f32,
    ty: f32,
    /// Text width.
    tw: f32,
    /// Start of the list item's own indent (marker area).
    list_x: f32,
    /// Left edge of the innermost quote, and the quote levels.
    quote_x: f32,
}

// ---------------------------------------------------------------- state ----

#[derive(Clone, Copy, PartialEq)]
enum Hover {
    None,
    Chip(usize, usize),
    Link(usize, usize),
    Check(usize),
}

pub(crate) struct RichEditorState {
    element_id: u64,
    callback: Option<EventCallback>,
    emits_key_down: bool,
    emits_change: bool,
    emits_link: bool,
    emits_drop: bool,
    intercept_keys: Vec<String>,
    focus_handle: FocusHandle,
    pub(crate) core: Core,
    theme: DocTheme,
    placeholder: String,
    read_only: bool,
    tasks: HashMap<String, TaskInfo>,
    glyph_source: HashMap<String, (String, String)>,
    glyphs: HashMap<String, Glyph>,
    layouts: HashMap<u64, Lay>,
    image_sources: RefCell<(u64, std::collections::HashSet<String>)>,
    places: Vec<Place>,
    width: f32,
    height: f32,
    last_bounds: Option<Bounds<Pixels>>,
    blink_anchor: Instant,
    blink_task: Option<Task<()>>,
    selecting: Option<SelectMode>,
    goal_x: Option<f32>,
    hover: Hover,
    hitboxes: Vec<(Hitbox, CursorStyle)>,
    emitted_rev: u64,
    emitted_state: String,
    pending_events: Vec<String>,
    focused_before: bool,
    started: Instant,
    /// Blocks shaped since the counter was last read (`stats` command).
    reshaped: u64,
    /// "Make task" is offered on checklist lines (a note with a task context).
    make_task: bool,
    /// The checklist line under the pointer that can become a task.
    hover_line: Option<usize>,
    /// Checked state per checklist block as last painted, and running tick transitions.
    checks_seen: HashMap<u64, bool>,
    check_anims: HashMap<u64, (Instant, bool)>,
    /// `.note-prose … input:active { scale: .92 }` (N6): the checklist box under a press, since when, and whether it is
    /// still held (80 ms `--ease-out` each way).
    check_press: Option<(usize, Instant, bool)>,
}

#[derive(Clone, Copy)]
enum SelectMode {
    Char,
    Word(Pos, Pos),
    Block(Pos, Pos),
}

impl RichEditorState {
    fn new(element_id: u64, focus_handle: FocusHandle, callback: Option<EventCallback>, cx: &mut Context<Self>) -> Self {
        RichEditorState {
            element_id,
            callback,
            emits_key_down: false,
            emits_change: false,
            emits_link: false,
            emits_drop: false,
            intercept_keys: Vec::new(),
            focus_handle,
            core: Core::new(Doc::new(Vec::new())),
            theme: DocTheme::default(),
            placeholder: String::new(),
            read_only: false,
            tasks: HashMap::new(),
            glyph_source: HashMap::new(),
            glyphs: HashMap::new(),
            layouts: HashMap::new(),
            image_sources: RefCell::new((u64::MAX, Default::default())),
            places: Vec::new(),
            width: 0.0,
            height: 0.0,
            last_bounds: None,
            blink_anchor: cx.background_executor().now(),
            blink_task: None,
            selecting: None,
            goal_x: None,
            hover: Hover::None,
            hitboxes: Vec::new(),
            emitted_rev: 0,
            emitted_state: String::new(),
            pending_events: Vec::new(),
            focused_before: false,
            started: Instant::now(),
            reshaped: 0,
            make_task: false,
            hover_line: None,
            checks_seen: HashMap::new(),
            check_anims: HashMap::new(),
            check_press: None,
        }
    }

    fn now(&self) -> u64 {
        self.started.elapsed().as_millis() as u64
    }

    fn replace_doc(&mut self, doc: Doc, cx: &mut Context<Self>) {
        let sel = self.core.sel;
        self.core.doc = doc;
        *self.image_sources.borrow_mut() = (u64::MAX, Default::default());
        self.core.history = History::default();
        self.core.stored = None;
        self.core.marked = None;
        self.core.rule_undo = None;
        self.core.sel = Sel { anchor: self.core.doc.clamp(sel.anchor), head: self.core.doc.clamp(sel.head) };
        self.emitted_rev = self.core.doc.rev();
        self.layouts.clear();
        self.goal_x = None;
        cx.notify();
    }

    fn reset_blink(&mut self, cx: &Context<Self>) {
        self.blink_anchor = cx.background_executor().now();
    }

    fn caret_shown(&mut self, window: &Window, cx: &mut Context<Self>) -> bool {
        if !self.focus_handle.is_focused(window) || !window.is_window_active() {
            self.blink_task = None;
            return false;
        }
        if self.blink_task.is_none() {
            self.reset_blink(cx);
            self.blink_task = Some(cx.spawn(async move |this, cx| loop {
                cx.background_executor().timer(Duration::from_millis(CARET_BLINK_MS)).await;
                if this.update(cx, |_, cx| cx.notify()).is_err() {
                    break;
                }
            }));
        }
        (self.blink_anchor.elapsed().as_millis() as u64 / CARET_BLINK_MS) % 2 == 0
    }

    /// After any edit or caret move.
    fn touched(&mut self, cx: &mut Context<Self>) {
        self.reset_blink(cx);
        cx.notify();
    }

    // ---- layout ----

    /// A checklist line can become a task when it has words, is not done and links no task.
    fn can_make_task(&self, index: usize) -> bool {
        let block = self.core.doc.block(index);
        matches!(block.kind, Kind::Task(false))
            && !block.text.replace(ATOM, "").trim().is_empty()
            && !block.runs.iter().any(|r| matches!(r.atom.as_deref(), Some(Atom::TaskRef { .. })))
    }

    /// What "Make task" needs: the line's place among the document's checklist items and its words.
    fn make_task_info(&self, index: usize) -> serde_json::Value {
        let doc = &self.core.doc;
        let ordinal = (0..index).filter(|&i| matches!(doc.block(i).kind, Kind::Task(_))).count();
        let block = doc.block(index);
        let title = doc.text_between(Pos::new(index, 0), Pos::new(index, block.len()), "", true);
        serde_json::json!({"block": index, "index": ordinal, "title": title.split_whitespace().collect::<Vec<_>>().join(" ")})
    }

    fn make_task(&mut self, _: &MakeTask, _: &mut Window, cx: &mut Context<Self>) {
        let index = self.core.sel.head.block;
        if self.make_task && matches!(self.core.doc.block(index).kind, Kind::Task(_)) {
            let mut info = self.make_task_info(index);
            info["t"] = "makeTask".into();
            self.emit(info);
            cx.notify();
        }
    }

    fn in_checklist(&self, index: usize) -> bool {
        matches!(self.core.doc.block(index).kind, Kind::Task(_))
    }

    fn layout_block(&mut self, index: usize, width: f32, window: &mut Window) {
        let block = self.core.doc.blocks[index].clone();
        let image_gen = IMAGE_GENERATION.with(|g| g.get());
        let w = width.max(1.0).round() as u32;
        if let Some(lay) = self.layouts.get(&block.id) {
            if lay.rev == block.rev && lay.width == w && lay.theme_key == self.theme.color_key && (lay.image_gen == image_gen || !matches!(block.kind, Kind::Image { .. })) {
                return;
            }
        }
        self.reshaped += 1;
        let style = block_style(&block, &self.theme);
        let disp = build_display(&block, &self.theme, &style, &self.tasks, self.in_checklist(index));
        let mut lines = Vec::new();
        let mut starts = Vec::new();
        let mut text_h = style.line_h;
        if !block.kind.is_atom() {
            let text: SharedString = disp.text.clone().into();
            let runs = if disp.runs.is_empty() { vec![run(0, font(&self.theme.font, style.weight, false), style.color)] } else { disp.runs.clone() };
            lines = window
                .text_system()
                .shape_text(text, px(style.size), &runs, style.wrap.then(|| px(width.max(1.0))), None)
                .map(|l| l.into_vec())
                .unwrap_or_default();
            let mut at = 0;
            for line in &lines {
                starts.push(at);
                at += line.len() + 1;
            }
            if starts.is_empty() {
                starts.push(0);
            }
            text_h = lines.iter().map(|l| f32::from(l.size(px(style.line_h)).height)).sum::<f32>().max(style.line_h);
        }
        let mut atom_h = 0.0;
        let mut image = None;
        let mut image_size = (0.0, 0.0);
        let mut image_failed = false;
        match &block.kind {
            Kind::Rule => atom_h = 1.0,
            Kind::Image { src, .. } => match image_for(src) {
                Some(Ok(img)) => {
                    let s = img.size(0);
                    let (iw, ih) = (s.width.0 as f32, s.height.0 as f32);
                    let max_h = f32::from(window.viewport_size().height) * 0.7;
                    let scale = (width / iw.max(1.0)).min(max_h / ih.max(1.0)).min(1.0);
                    image_size = (iw * scale, ih * scale);
                    atom_h = image_size.1;
                    image = Some(img);
                }
                Some(Err(())) => {
                    image_failed = true;
                    image_size = (width.min(320.0), 120.0);
                    atom_h = 120.0;
                }
                None => {
                    image_size = (width.min(320.0), 120.0);
                    atom_h = 120.0;
                }
            },
            _ => {}
        }
        self.layouts.insert(
            block.id,
            Lay { rev: block.rev, width: w, theme_key: self.theme.color_key, image_gen, disp, lines, starts, style, text_h, atom_h, image, image_size, image_failed },
        );
    }

    /// The vertical gap above block `index` (desktop `.note-prose` margins, collapsed).
    fn gap_before(&self, index: usize) -> f32 {
        if index == 0 {
            return 0.0;
        }
        let em = self.theme.size;
        let doc = &self.core.doc;
        let a = doc.block(index - 1);
        let b = doc.block(index);
        let heading_margin = |block: &Block| match block.kind {
            Kind::Heading(1) => Some(if self.theme.compact { 1.2 * em * 17.0 / 15.0 } else { 2.25 * em * 17.0 / 15.0 }),
            Kind::Heading(2) => Some(if self.theme.compact { 1.2 * em * 17.0 / 15.0 } else { 2.0 * em * 17.0 / 15.0 }),
            Kind::Heading(_) => Some(if self.theme.compact { 1.2 * em } else { 1.6 * em }),
            _ => None,
        };
        // Inside one list item: later paragraphs and nested blocks.
        if b.cont {
            return 0.0;
        }
        let a_list = a.kind.is_list() || a.cont;
        let b_list = b.kind.is_list();
        if a_list && b_list && a.quote == b.quote {
            let same_list = b.depth != a.depth || a.cont || std::mem::discriminant(&a.kind) == std::mem::discriminant(&b.kind);
            if !same_list && b.depth == 0 {
                // A checklist, bullet list and numbered list next to each other are separate lists.
                return 0.93 * em;
            }
            if matches!(b.kind, Kind::Task(_)) && b.depth == a.depth && matches!(a.kind, Kind::Task(_)) {
                return 0.0;
            }
            return 0.27 * em;
        }
        if a.quote > 0 && b.quote > 0 && a.quote == b.quote && !a_list && !b_list {
            return 0.27 * em;
        }
        if let Some(margin) = heading_margin(b) {
            return margin;
        }
        if matches!(a.kind, Kind::Heading(_)) {
            return 0.53 * em;
        }
        if matches!(a.kind, Kind::Rule) || matches!(b.kind, Kind::Rule) {
            return 1.6 * em;
        }
        0.93 * em
    }

    fn layout_all(&mut self, width: f32, window: &mut Window) -> f32 {
        self.width = width;
        let em = self.theme.size;
        let count = self.core.doc.len();
        let live: std::collections::HashSet<u64> = self.core.doc.blocks.iter().map(|b| b.id).collect();
        self.layouts.retain(|id, _| live.contains(id));
        self.places.clear();
        let mut y = 0.0;
        let mut stack: Vec<f32> = Vec::new();
        let check = 1.067 * em;
        let check_gap = if self.theme.compact { 9.0 } else { 12.0 };
        let task_pad = if self.theme.compact { 2.0 } else { 4.0 };
        for index in 0..count {
            let (kind, depth, quote, cont) = {
                let b = self.core.doc.block(index);
                (b.kind.clone(), b.depth as usize, b.quote as f32, b.cont)
            };
            let quote_x = quote * 16.0;
            let mut list_x = quote_x;
            let mut tx = quote_x;
            if kind.is_list() {
                stack.truncate(depth);
                while stack.len() < depth {
                    stack.push(1.4 * em);
                }
                list_x = quote_x + stack.iter().sum::<f32>();
                let own = if matches!(kind, Kind::Task(_)) { check + check_gap } else { 1.4 * em };
                stack.push(own);
                tx = list_x + own;
            } else if cont {
                let levels = (depth + 1).min(stack.len());
                tx = quote_x + stack[..levels].iter().sum::<f32>();
                list_x = tx;
            } else {
                stack.clear();
            }
            let mut tw = (width - tx).max(1.0);
            let code = matches!(kind, Kind::Code(_));
            if code {
                tw = (tw - 32.0).max(1.0);
            }
            self.layout_block(index, tw, window);
            y += self.gap_before(index);
            let lay = &self.layouts[&self.core.doc.block(index).id];
            let (height, ty) = match &kind {
                Kind::Rule | Kind::Image { .. } => (lay.atom_h, y),
                Kind::Code(_) => (lay.text_h + 28.0, y + 14.0),
                Kind::Task(_) => (lay.text_h + 2.0 * task_pad, y + task_pad),
                _ => (lay.text_h, y),
            };
            let tx = if code { tx + 16.0 } else { tx };
            self.places.push(Place { top: y, height, tx, ty, tw, list_x, quote_x });
            y += height;
        }
        let min = self.theme.min_height.max(self.theme.size * self.theme.line);
        self.height = y.max(min);
        self.height
    }

    // ---- geometry ----

    fn lay(&self, index: usize) -> Option<&Lay> {
        self.layouts.get(&self.core.doc.block(index).id)
    }

    /// Caret point (relative to the element) and line height for a position.
    fn point_for(&self, pos: Pos) -> Option<(Point<Pixels>, f32)> {
        let place = *self.places.get(pos.block)?;
        let lay = self.lay(pos.block)?;
        let block = self.core.doc.block(pos.block);
        if block.kind.is_atom() {
            let x = if pos.offset == 0 { place.tx } else { place.tx + lay.image_size.0.max(0.0) };
            return Some((point(px(x), px(place.top)), place.height.max(lay.style.line_h)));
        }
        let d = lay.disp.m2d(pos.offset);
        let lh = lay.style.line_h;
        let mut y = place.ty;
        for (i, line) in lay.lines.iter().enumerate() {
            let start = lay.starts[i];
            if d >= start && d <= start + line.len() {
                let local = line.position_for_index(d - start, px(lh))?;
                return Some((point(px(place.tx) + local.x, px(y) + local.y), lh));
            }
            y += f32::from(line.size(px(lh)).height);
        }
        Some((point(px(place.tx), px(place.ty)), lh))
    }

    /// Which block a y (relative to the element) falls in; gaps go to the nearer block.
    fn block_at_y(&self, y: f32) -> usize {
        let count = self.places.len();
        if count == 0 {
            return 0;
        }
        for i in 0..count {
            let p = self.places[i];
            if y < p.top + p.height {
                if i > 0 && y < p.top {
                    let prev = self.places[i - 1];
                    return if y - (prev.top + prev.height) < p.top - y { i - 1 } else { i };
                }
                return i;
            }
        }
        count - 1
    }

    fn pos_at_point(&self, p: Point<Pixels>) -> Pos {
        let (x, y) = (f32::from(p.x), f32::from(p.y));
        let index = self.block_at_y(y);
        self.pos_in_block(index, x, y)
    }

    fn pos_in_block(&self, index: usize, x: f32, y: f32) -> Pos {
        let Some(place) = self.places.get(index).copied() else { return Pos::default() };
        let Some(lay) = self.lay(index) else { return Pos::new(index, 0) };
        let block = self.core.doc.block(index);
        if block.kind.is_atom() {
            return Pos::new(index, if x > place.tx + lay.image_size.0 / 2.0 && !matches!(block.kind, Kind::Rule) { ATOM_LEN } else { 0 });
        }
        let lh = lay.style.line_h;
        let mut ly = place.ty;
        let count = lay.lines.len();
        for (i, line) in lay.lines.iter().enumerate() {
            let h = f32::from(line.size(px(lh)).height);
            if y < ly + h || i + 1 == count {
                let local = point(px(x - place.tx), px((y - ly).clamp(0.0, (h - 1.0).max(0.0))));
                let index_in_line = line.closest_index_for_position(local, px(lh)).unwrap_or_else(|i| i);
                let d = lay.starts[i] + index_in_line;
                return Pos::new(index, self.core.doc.clamp(Pos::new(index, lay.disp.d2m(d))).offset);
            }
            ly += h;
        }
        Pos::new(index, 0)
    }

    /// Rectangles covering `a..b` inside one block (relative to the element).
    /// `full_end`: the range runs on past the block, so its last row shows a sliver for the break.
    fn range_rects(&self, index: usize, a: usize, b: usize, out: &mut Vec<Bounds<Pixels>>, full_end: bool) {
        let Some(place) = self.places.get(index).copied() else { return };
        let Some(lay) = self.lay(index) else { return };
        let block = self.core.doc.block(index);
        if block.kind.is_atom() {
            if a < b {
                let w = if lay.image_size.0 > 0.0 { lay.image_size.0 } else { place.tw };
                out.push(Bounds::new(point(px(place.tx), px(place.top)), size(px(w), px(place.height.max(2.0)))));
            }
            return;
        }
        self.display_rects(index, lay.disp.m2d(a), lay.disp.m2d(b), out, full_end);
    }

    /// `range_rects` over display offsets. Inline code washes use it directly: a code run's model start maps to the end of
    /// the text before its padding room, which a wrap can leave alone on the previous line as an empty chip (ADE-71).
    fn display_rects(&self, index: usize, da: usize, db: usize, out: &mut Vec<Bounds<Pixels>>, full_end: bool) {
        let Some(place) = self.places.get(index).copied() else { return };
        let Some(lay) = self.lay(index) else { return };
        let lh = lay.style.line_h;
        let sliver = self.theme.size * 0.3;
        let mut y = place.ty;
        let count = lay.lines.len();
        for (i, line) in lay.lines.iter().enumerate() {
            let start = lay.starts[i];
            let end = start + line.len();
            if db >= start && da <= end {
                let rows = row_spans(line);
                let last_row = rows.len() - 1;
                for (row, (rs, re, row_x)) in rows.iter().copied().enumerate() {
                    let s0 = da.max(start) - start;
                    let e0 = db.min(end) - start;
                    let (sa, sb) = (s0.max(rs), e0.min(re));
                    let crosses_break = (db > end || (full_end && i + 1 == count)) && row == last_row && e0 == line.len();
                    if sa > sb || (sa == sb && !crosses_break) {
                        continue;
                    }
                    let x0 = f32::from(line.unwrapped_layout.x_for_index(sa)) - row_x;
                    // A wrapped row keeps the space it wrapped at, as WebKit underlines and selects it.
                    let mut x1 = f32::from(line.unwrapped_layout.x_for_index(sb.max(sa))) - row_x;
                    if crosses_break {
                        x1 = x1.max(x0) + sliver;
                    }
                    if x1 > x0 {
                        out.push(Bounds::new(point(px(place.tx + x0), px(y + row as f32 * lh)), size(px(x1 - x0), px(lh))));
                    }
                }
            }
            y += f32::from(line.size(px(lh)).height);
        }
        if lay.lines.is_empty() && (full_end || db > 0) && da <= db {
            out.push(Bounds::new(point(px(place.tx), px(place.ty)), size(px(sliver), px(lh))));
        }
    }

    fn selection_rects(&self) -> Vec<Bounds<Pixels>> {
        let mut out = Vec::new();
        let sel = self.core.sel;
        if sel.is_empty() {
            return out;
        }
        let (start, end) = (sel.start(), sel.end());
        for index in start.block..=end.block.min(self.core.doc.len().saturating_sub(1)) {
            let block = self.core.doc.block(index);
            let a = if index == start.block { start.offset } else { 0 };
            let b = if index == end.block { end.offset } else { block.len() };
            if block.kind.is_atom() {
                continue;
            }
            let first = out.len();
            self.range_rects(index, a, b, &mut out, index != end.block);
            // WebKit fills a selected row to the column edge when the selection runs on past it.
            let place = self.places[index];
            let rows = out.len() - first;
            for (n, r) in out[first..].iter_mut().enumerate() {
                if n + 1 < rows || index != end.block {
                    r.size.width = px((place.tx + place.tw - f32::from(r.origin.x)).max(f32::from(r.size.width)));
                }
            }
            // …and the space between two selected blocks.
            if index < end.block {
                let next = self.places[index + 1];
                let bottom = place.top + place.height;
                if next.top > bottom {
                    out.push(Bounds::new(point(px(0.0), px(bottom)), size(px(self.width), px(next.top - bottom))));
                }
            }
        }
        out
    }

    // ---- navigation ----

    fn cursor(&self) -> Pos {
        self.core.sel.head
    }

    fn move_to(&mut self, pos: Pos, cx: &mut Context<Self>) {
        let pos = self.core.doc.clamp(pos);
        self.core.set_selection(Sel::caret(pos));
        self.touched(cx);
    }

    fn select_to(&mut self, pos: Pos, cx: &mut Context<Self>) {
        let pos = self.core.doc.clamp(pos);
        let anchor = self.core.sel.anchor;
        self.core.set_selection(Sel { anchor, head: pos });
        self.touched(cx);
    }

    fn prev_pos(&self, pos: Pos) -> Pos {
        if pos.offset > 0 {
            let block = self.core.doc.block(pos.block);
            if block.kind.is_atom() {
                return Pos::new(pos.block, 0);
            }
            return Pos::new(pos.block, previous_cluster(&block.text, pos.offset));
        }
        if pos.block == 0 {
            return pos;
        }
        let prev = pos.block - 1;
        Pos::new(prev, self.core.doc.block(prev).len())
    }

    fn next_pos(&self, pos: Pos) -> Pos {
        let block = self.core.doc.block(pos.block);
        if pos.offset < block.len() {
            if block.kind.is_atom() {
                return Pos::new(pos.block, ATOM_LEN);
            }
            return Pos::new(pos.block, next_cluster(&block.text, pos.offset));
        }
        if pos.block + 1 >= self.core.doc.len() {
            return pos;
        }
        Pos::new(pos.block + 1, 0)
    }

    fn word_left(&self, pos: Pos) -> Pos {
        if pos.offset == 0 {
            return self.prev_pos(pos);
        }
        let text = &self.core.doc.block(pos.block).text;
        let at = text
            .split_word_bound_indices()
            .rev()
            .find_map(|(i, w)| (i < pos.offset && w.chars().any(char::is_alphanumeric)).then_some(i))
            .unwrap_or(0);
        Pos::new(pos.block, at)
    }

    fn word_right(&self, pos: Pos) -> Pos {
        let text = &self.core.doc.block(pos.block).text;
        if pos.offset >= text.len() {
            return self.next_pos(pos);
        }
        let at = text
            .split_word_bound_indices()
            .find_map(|(i, w)| {
                let end = i + w.len();
                (end > pos.offset && w.chars().any(char::is_alphanumeric)).then_some(end)
            })
            .unwrap_or(text.len());
        Pos::new(pos.block, at)
    }

    fn line_edge(&self, pos: Pos, end: bool) -> Pos {
        let Some((p, lh)) = self.point_for(pos) else { return pos };
        let place = self.places[pos.block];
        let x = if end { place.tx + place.tw + 10_000.0 } else { place.tx - 10_000.0 };
        let y = f32::from(p.y) + lh / 2.0;
        let target = self.pos_in_block(pos.block, x, y);
        // A wrapped line's end sits before its trailing space.
        target
    }

    fn vertical(&self, pos: Pos, down: bool) -> Option<(Pos, f32)> {
        let (p, lh) = self.point_for(pos)?;
        let x = self.goal_x.unwrap_or(f32::from(p.x));
        let y = f32::from(p.y);
        let place = self.places[pos.block];
        let target_y = if down { y + lh * 1.5 } else { y - lh * 0.5 };
        if target_y >= place.ty && target_y < place.ty + self.lay(pos.block).map(|l| l.text_h).unwrap_or(0.0) && !self.core.doc.block(pos.block).kind.is_atom() {
            return Some((self.pos_in_block(pos.block, x, target_y), x));
        }
        let next = if down { pos.block + 1 } else { pos.block.checked_sub(1)? };
        if next >= self.places.len() {
            return Some((self.core.doc.end(), x));
        }
        let np = self.places[next];
        let lay = self.lay(next)?;
        if self.core.doc.block(next).kind.is_atom() {
            return Some((Pos::new(next, 0), x));
        }
        let ty = if down { np.ty + lay.style.line_h / 2.0 } else { np.ty + lay.text_h - lay.style.line_h / 2.0 };
        Some((self.pos_in_block(next, x, ty), x))
    }

    // ---- events ----

    fn emit(&mut self, value: serde_json::Value) {
        self.pending_events.push(value.to_string());
    }

    fn flush_events(&mut self) {
        if !self.emits_change {
            self.pending_events.clear();
            return;
        }
        for value in std::mem::take(&mut self.pending_events) {
            emit_event_full(&self.callback, self.element_id, "change", |payload| payload.value = Some(value));
        }
    }

    fn state_json(&self, window: &Window) -> serde_json::Value {
        let core = &self.core;
        let sel = core.sel;
        // Rectangles are relative to the element, so scrolling does not change the state.
        let rect = |b: Bounds<Pixels>| serde_json::json!([f32::from(b.origin.x), f32::from(b.origin.y), f32::from(b.size.width), f32::from(b.size.height)]);
        let caret = self.point_for(sel.head).map(|(p, lh)| rect(Bounds::new(p, size(px(1.0), px(lh)))));
        let selection = if sel.is_empty() {
            serde_json::Value::Null
        } else {
            let rects = self.selection_rects();
            if rects.is_empty() {
                // A selected image or rule.
                let mut out = Vec::new();
                self.range_rects(sel.start().block, 0, ATOM_LEN, &mut out, false);
                out.first().map(|b| rect(*b)).unwrap_or(serde_json::Value::Null)
            } else {
                let mut b = rects[0];
                for r in &rects[1..] {
                    b = b.union(r);
                }
                rect(b)
            }
        };
        let link = core.active_link().map(|(a, b, link)| {
            let mut rects = Vec::new();
            self.range_rects(a.block, a.offset, b.offset, &mut rects, false);
            serde_json::json!({"href": link.href, "rect": rects.first().map(|r| rect(*r))})
        });
        let slash = self.slash_query();
        let head_block = core.doc.block(sel.head.block);
        let headings: Vec<serde_json::Value> = core
            .doc
            .blocks
            .iter()
            .enumerate()
            .filter_map(|(i, b)| match b.kind {
                Kind::Heading(level) => Some(serde_json::json!({
                    "level": level,
                    "text": core.doc.text_between(Pos::new(i, 0), Pos::new(i, b.len()), "", true),
                    "y": self.places.get(i).map(|p| p.top).unwrap_or(0.0),
                    "index": i,
                })),
                _ => None,
            })
            .collect();
        serde_json::json!({
            "t": "state",
            "focused": self.focus_handle.is_focused(window),
            "collapsed": sel.is_empty(),
            "block": core.current_block(),
            "inCode": head_block.kind.is_literal(),
            "quote": head_block.quote > 0,
            "marks": {
                "bold": core.mark_active(BOLD),
                "italic": core.mark_active(ITALIC),
                "strike": core.mark_active(STRIKE),
                "code": core.mark_active(CODE),
            },
            "link": link,
            "caret": caret,
            "selection": selection,
            "slash": slash.map(|(from, query)| {
                let r = self.point_for(Pos::new(sel.head.block, from)).map(|(p, lh)| rect(Bounds::new(p, size(px(1.0), px(lh)))));
                serde_json::json!({"from": from, "block": sel.head.block, "query": query, "rect": r})
            }),
            "canUndo": !core.history.undo.is_empty(),
            "canRedo": !core.history.redo.is_empty(),
            "empty": core.doc.len() == 1 && core.doc.block(0).is_empty() && core.doc.block(0).kind == Kind::Paragraph,
            "headings": headings,
            "height": self.height,
            "hoverTask": self.hover_line.filter(|&i| i < core.doc.len() && self.can_make_task(i)).map(|i| {
                let place = self.places[i];
                let lh = self.lay(i).map(|l| l.style.line_h).unwrap_or(self.theme.size * self.theme.line);
                let mut info = self.make_task_info(i);
                info["rect"] = serde_json::json!([place.tx, place.ty, place.tw, lh]);
                info
            }),
        })
    }

    /// "/" at the start of a text block or after a space, and what follows it up to the caret.
    fn slash_query(&self) -> Option<(usize, String)> {
        let sel = self.core.sel;
        if !sel.is_empty() || self.read_only {
            return None;
        }
        let block = self.core.doc.block(sel.head.block);
        if block.kind.is_literal() || block.kind.is_atom() {
            return None;
        }
        let before = &block.text[..sel.head.offset];
        let slash = before.rfind('/')?;
        let query = &before[slash + 1..];
        if query.contains(char::is_whitespace) || query.contains(ATOM) {
            return None;
        }
        let lead = &before[..slash];
        if !(lead.is_empty() || lead.ends_with(' ') || lead.ends_with('\u{a0}')) {
            return None;
        }
        Some((slash, query.to_string()))
    }

    fn emit_link(&self, href: String) {
        if self.emits_link {
            emit_event_full(&self.callback, self.element_id, "linkClick", |payload| payload.value = Some(href));
        }
    }

    // ---- editing actions ----

    fn editable(&self) -> bool {
        !self.read_only
    }

    fn after_edit(&mut self, cx: &mut Context<Self>) {
        self.goal_x = None;
        self.touched(cx);
    }

    fn backspace(&mut self, _: &Backspace, _: &mut Window, cx: &mut Context<Self>) {
        if !self.editable() {
            return;
        }
        let now = self.now();
        if self.core.backspace(now) == BackspaceResult::AtStart {
            self.emit(serde_json::json!({"t": "backspaceAtStart"}));
        }
        self.after_edit(cx);
    }

    fn delete(&mut self, _: &Delete, _: &mut Window, cx: &mut Context<Self>) {
        if !self.editable() {
            return;
        }
        let now = self.now();
        self.core.delete_forward(now);
        self.after_edit(cx);
    }

    fn delete_to(&mut self, target: Pos, cx: &mut Context<Self>) {
        if !self.editable() {
            return;
        }
        if self.core.sel.is_empty() {
            let head = self.core.sel.head;
            if target == head {
                return;
            }
            self.core.sel = Sel { anchor: head, head: target };
        }
        let now = self.now();
        self.core.delete_selection(now, None);
        self.core.history.seal();
        self.after_edit(cx);
    }

    fn delete_word_left(&mut self, _: &DeleteWordLeft, _: &mut Window, cx: &mut Context<Self>) {
        let target = self.word_left(self.cursor());
        self.delete_to(target, cx);
    }

    fn delete_word_right(&mut self, _: &DeleteWordRight, _: &mut Window, cx: &mut Context<Self>) {
        let target = self.word_right(self.cursor());
        self.delete_to(target, cx);
    }

    fn delete_to_line_start(&mut self, _: &DeleteToLineStart, _: &mut Window, cx: &mut Context<Self>) {
        let target = self.line_edge(self.cursor(), false);
        self.delete_to(target, cx);
    }

    fn left(&mut self, _: &Left, _: &mut Window, cx: &mut Context<Self>) {
        let sel = self.core.sel;
        let pos = if sel.is_empty() { self.prev_pos(sel.head) } else { sel.start() };
        self.goal_x = None;
        self.move_to(pos, cx);
    }

    fn right(&mut self, _: &Right, _: &mut Window, cx: &mut Context<Self>) {
        let sel = self.core.sel;
        let pos = if sel.is_empty() { self.next_pos(sel.head) } else { sel.end() };
        self.goal_x = None;
        self.move_to(pos, cx);
    }

    fn up(&mut self, _: &Up, _: &mut Window, cx: &mut Context<Self>) {
        let from = if self.core.sel.is_empty() { self.cursor() } else { self.core.sel.start() };
        if let Some((pos, x)) = self.vertical(from, false) {
            let target = if from.block == 0 && pos == from { Pos::new(0, 0) } else { pos };
            self.move_to(target, cx);
            self.goal_x = Some(x);
        }
    }

    fn down(&mut self, _: &Down, _: &mut Window, cx: &mut Context<Self>) {
        let from = if self.core.sel.is_empty() { self.cursor() } else { self.core.sel.end() };
        if let Some((pos, x)) = self.vertical(from, true) {
            self.move_to(pos, cx);
            self.goal_x = Some(x);
        }
    }

    fn select_left(&mut self, _: &SelectLeft, _: &mut Window, cx: &mut Context<Self>) {
        let pos = self.prev_pos(self.cursor());
        self.goal_x = None;
        self.select_to(pos, cx);
    }

    fn select_right(&mut self, _: &SelectRight, _: &mut Window, cx: &mut Context<Self>) {
        let pos = self.next_pos(self.cursor());
        self.goal_x = None;
        self.select_to(pos, cx);
    }

    fn select_up(&mut self, _: &SelectUp, _: &mut Window, cx: &mut Context<Self>) {
        if let Some((pos, x)) = self.vertical(self.cursor(), false) {
            let target = if self.cursor().block == 0 && pos == self.cursor() { Pos::new(0, 0) } else { pos };
            self.select_to(target, cx);
            self.goal_x = Some(x);
        }
    }

    fn select_down(&mut self, _: &SelectDown, _: &mut Window, cx: &mut Context<Self>) {
        if let Some((pos, x)) = self.vertical(self.cursor(), true) {
            self.select_to(pos, cx);
            self.goal_x = Some(x);
        }
    }

    fn select_all(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<Self>) {
        self.core.set_selection(Sel { anchor: Pos::new(0, 0), head: self.core.doc.end() });
        self.touched(cx);
    }

    fn home(&mut self, _: &Home, _: &mut Window, cx: &mut Context<Self>) {
        let pos = self.line_edge(self.cursor(), false);
        self.goal_x = None;
        self.move_to(pos, cx);
    }

    fn end(&mut self, _: &End, _: &mut Window, cx: &mut Context<Self>) {
        let pos = self.line_edge(self.cursor(), true);
        self.goal_x = None;
        self.move_to(pos, cx);
    }

    fn doc_start(&mut self, _: &DocStart, _: &mut Window, cx: &mut Context<Self>) {
        self.goal_x = None;
        self.move_to(Pos::new(0, 0), cx);
    }

    fn doc_end(&mut self, _: &DocEnd, _: &mut Window, cx: &mut Context<Self>) {
        self.goal_x = None;
        let end = self.core.doc.end();
        self.move_to(end, cx);
    }

    fn select_home(&mut self, _: &SelectHome, _: &mut Window, cx: &mut Context<Self>) {
        let pos = self.line_edge(self.cursor(), false);
        self.select_to(pos, cx);
    }

    fn select_end(&mut self, _: &SelectEnd, _: &mut Window, cx: &mut Context<Self>) {
        let pos = self.line_edge(self.cursor(), true);
        self.select_to(pos, cx);
    }

    fn select_doc_start(&mut self, _: &SelectDocStart, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(Pos::new(0, 0), cx);
    }

    fn select_doc_end(&mut self, _: &SelectDocEnd, _: &mut Window, cx: &mut Context<Self>) {
        let end = self.core.doc.end();
        self.select_to(end, cx);
    }

    fn word_left_action(&mut self, _: &WordLeft, _: &mut Window, cx: &mut Context<Self>) {
        let pos = self.word_left(self.cursor());
        self.move_to(pos, cx);
    }

    fn word_right_action(&mut self, _: &WordRight, _: &mut Window, cx: &mut Context<Self>) {
        let pos = self.word_right(self.cursor());
        self.move_to(pos, cx);
    }

    fn select_word_left(&mut self, _: &SelectWordLeft, _: &mut Window, cx: &mut Context<Self>) {
        let pos = self.word_left(self.cursor());
        self.select_to(pos, cx);
    }

    fn select_word_right(&mut self, _: &SelectWordRight, _: &mut Window, cx: &mut Context<Self>) {
        let pos = self.word_right(self.cursor());
        self.select_to(pos, cx);
    }

    fn plain_selection(&self) -> String {
        let sel = self.core.sel;
        self.core.doc.text_between(sel.start(), sel.end(), "\n\n", true)
    }

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        let sel = self.core.sel;
        if sel.is_empty() {
            return;
        }
        let text = self.plain_selection();
        let slice = slice_json(&self.core.doc, sel.start(), sel.end());
        cx.write_to_clipboard(ClipboardItem::new_string_with_metadata(text, slice.to_string()));
    }

    fn cut(&mut self, _: &Cut, window: &mut Window, cx: &mut Context<Self>) {
        if !self.editable() || self.core.sel.is_empty() {
            return;
        }
        self.copy(&Copy, window, cx);
        let now = self.now();
        self.core.delete_selection(now, None);
        self.core.history.seal();
        self.after_edit(cx);
    }

    fn paste_item(&mut self, item: ClipboardItem, plain: bool, cx: &mut Context<Self>) {
        // A rich slice copied from an editor: the blocks as they were.
        if !plain {
            if let Some(slice) = item.metadata().and_then(|m| serde_json::from_str::<serde_json::Value>(m).ok()).filter(|v| v.get("kybernRichSlice").is_some()) {
                let blocks: Vec<Block> = slice
                    .get("blocks")
                    .and_then(|v| v.as_array())
                    .map(|b| b.iter().enumerate().map(|(i, v)| block_from(v, i as u64 + 1)).collect())
                    .unwrap_or_default();
                if !blocks.is_empty() {
                    paste_blocks(&mut self.core, blocks);
                    self.after_edit(cx);
                    return;
                }
            }
        }
        for entry in item.entries() {
            if let ClipboardEntry::Image(image) = entry {
                if self.emits_drop && !plain {
                    // JS stores the image with the note's daemon and inserts it.
                    let ext = match image.format {
                        gpui::ImageFormat::Png => "png",
                        gpui::ImageFormat::Jpeg => "jpg",
                        gpui::ImageFormat::Gif => "gif",
                        gpui::ImageFormat::Webp => "webp",
                        gpui::ImageFormat::Tiff => "tiff",
                        _ => "png",
                    };
                    let path = std::env::temp_dir().join(format!("kybern-paste-{}-{}.{ext}", std::process::id(), self.now()));
                    if std::fs::write(&path, &image.bytes).is_ok() {
                        let callback = self.callback.clone();
                        let id = self.element_id;
                        emit_event_full(&callback, id, "fileDrop", |payload| {
                            payload.paths = Some(vec![path.to_string_lossy().into_owned()]);
                        });
                        return;
                    }
                }
            }
        }
        let has_paths = item.entries().iter().any(|e| matches!(e, ClipboardEntry::ExternalPaths(_)));
        if has_paths && self.emits_drop {
            let paths: Vec<String> = item
                .entries()
                .iter()
                .filter_map(|e| match e {
                    ClipboardEntry::ExternalPaths(paths) => Some(paths.paths().iter().map(|p| p.to_string_lossy().into_owned()).collect::<Vec<_>>()),
                    _ => None,
                })
                .flatten()
                .collect();
            emit_event_full(&self.callback, self.element_id, "fileDrop", |payload| payload.paths = Some(paths));
            return;
        }
        if let Some(text) = item.text() {
            self.core.paste_text(&text);
            self.after_edit(cx);
        }
    }

    fn paste(&mut self, _: &Paste, _: &mut Window, cx: &mut Context<Self>) {
        if !self.editable() {
            cx.propagate();
            return;
        }
        if let Some(item) = cx.read_from_clipboard() {
            self.paste_item(item, false, cx);
        }
    }

    fn paste_plain(&mut self, _: &PastePlain, _: &mut Window, cx: &mut Context<Self>) {
        if !self.editable() {
            return;
        }
        if let Some(item) = cx.read_from_clipboard() {
            self.paste_item(item, true, cx);
        }
    }

    fn undo(&mut self, _: &Undo, _: &mut Window, cx: &mut Context<Self>) {
        if self.editable() && self.core.marked.is_none() && self.core.undo() {
            self.after_edit(cx);
        }
    }

    fn redo(&mut self, _: &Redo, _: &mut Window, cx: &mut Context<Self>) {
        if self.editable() && self.core.marked.is_none() && self.core.redo() {
            self.after_edit(cx);
        }
    }

    fn enter(&mut self, _: &Enter, _: &mut Window, cx: &mut Context<Self>) {
        if !self.editable() {
            return;
        }
        let now = self.now();
        self.core.enter(now);
        self.after_edit(cx);
    }

    fn hard_break(&mut self, _: &HardBreak, _: &mut Window, cx: &mut Context<Self>) {
        if !self.editable() {
            return;
        }
        let now = self.now();
        self.core.hard_break(now);
        self.after_edit(cx);
    }

    fn indent(&mut self, _: &Indent, _: &mut Window, cx: &mut Context<Self>) {
        if !self.editable() || !self.core.indent(true) {
            cx.propagate();
            return;
        }
        self.after_edit(cx);
    }

    fn outdent(&mut self, _: &Outdent, _: &mut Window, cx: &mut Context<Self>) {
        if !self.editable() || !self.core.indent(false) {
            cx.propagate();
            return;
        }
        self.after_edit(cx);
    }

    fn mark(&mut self, mark: u8, cx: &mut Context<Self>) {
        if !self.editable() || self.core.doc.block(self.core.sel.head.block).kind.is_literal() {
            return;
        }
        self.core.toggle_mark(mark);
        self.after_edit(cx);
    }

    fn bold(&mut self, _: &Bold, _: &mut Window, cx: &mut Context<Self>) {
        self.mark(BOLD, cx);
    }
    fn italic(&mut self, _: &Italic, _: &mut Window, cx: &mut Context<Self>) {
        self.mark(ITALIC, cx);
    }
    fn strike(&mut self, _: &Strike, _: &mut Window, cx: &mut Context<Self>) {
        self.mark(STRIKE, cx);
    }
    fn inline_code(&mut self, _: &InlineCode, _: &mut Window, cx: &mut Context<Self>) {
        self.mark(CODE, cx);
    }

    fn turn_into(&mut self, id: &str, cx: &mut Context<Self>) {
        if !self.editable() {
            return;
        }
        turn_into(&mut self.core, id, false);
        self.after_edit(cx);
    }

    fn heading1(&mut self, _: &Heading1, _: &mut Window, cx: &mut Context<Self>) {
        self.toggle_heading(1, cx);
    }
    fn heading2(&mut self, _: &Heading2, _: &mut Window, cx: &mut Context<Self>) {
        self.toggle_heading(2, cx);
    }
    fn heading3(&mut self, _: &Heading3, _: &mut Window, cx: &mut Context<Self>) {
        self.toggle_heading(3, cx);
    }
    fn toggle_heading(&mut self, level: u8, cx: &mut Context<Self>) {
        if !self.editable() {
            return;
        }
        // toggleHeading: the same heading again turns back into text.
        self.core.set_block(Kind::Heading(level), self.core.doc.block(self.core.sel.head.block).kind == Kind::Heading(level));
        self.after_edit(cx);
    }
    fn paragraph(&mut self, _: &Paragraph, _: &mut Window, cx: &mut Context<Self>) {
        self.turn_into("text", cx);
    }
    fn bullet_list(&mut self, _: &BulletList, _: &mut Window, cx: &mut Context<Self>) {
        if self.editable() {
            self.core.set_block(Kind::Bullet, true);
            self.after_edit(cx);
        }
    }
    fn ordered_list(&mut self, _: &OrderedList, _: &mut Window, cx: &mut Context<Self>) {
        if self.editable() {
            self.core.set_block(Kind::Ordered(1), true);
            self.after_edit(cx);
        }
    }
    fn task_list(&mut self, _: &TaskList, _: &mut Window, cx: &mut Context<Self>) {
        if self.editable() {
            self.core.set_block(Kind::Task(false), true);
            self.after_edit(cx);
        }
    }
    fn blockquote(&mut self, _: &Blockquote, _: &mut Window, cx: &mut Context<Self>) {
        if self.editable() {
            self.core.toggle_quote();
            self.after_edit(cx);
        }
    }
    fn code_block(&mut self, _: &CodeBlock, _: &mut Window, cx: &mut Context<Self>) {
        if self.editable() {
            self.core.set_block(Kind::Code(String::new()), true);
            self.after_edit(cx);
        }
    }

    // ---- mouse ----

    fn local(&self, position: Point<Pixels>) -> Point<Pixels> {
        let origin = self.last_bounds.map(|b| b.origin).unwrap_or_default();
        position - origin
    }

    fn checkbox_at(&self, p: Point<Pixels>) -> Option<usize> {
        let (x, y) = (f32::from(p.x), f32::from(p.y));
        let index = self.block_at_y(y);
        let block = self.core.doc.block(index);
        if !matches!(block.kind, Kind::Task(_)) {
            return None;
        }
        let place = self.places[index];
        let lay = self.lay(index)?;
        let check = 1.067 * self.theme.size;
        let pad = 0.3 * self.theme.size;
        let line_top = place.ty;
        let within_x = x >= place.list_x - pad && x <= place.list_x + check + pad;
        let within_y = y >= line_top - pad && y <= line_top + lay.style.line_h + pad;
        (within_x && within_y).then_some(index)
    }

    fn chip_at(&self, p: Point<Pixels>) -> Option<(usize, usize)> {
        let index = self.block_at_y(f32::from(p.y));
        let lay = self.lay(index)?;
        for (n, chip) in lay.disp.chips.iter().enumerate() {
            let mut rects = Vec::new();
            self.range_rects(index, chip.model, chip.model + ATOM_LEN, &mut rects, false);
            if rects.iter().any(|r| r.contains(&p)) {
                return Some((index, n));
            }
        }
        None
    }

    fn link_at(&self, p: Point<Pixels>) -> Option<(usize, usize)> {
        let index = self.block_at_y(f32::from(p.y));
        let lay = self.lay(index)?;
        for (n, (range, _)) in lay.disp.links.iter().enumerate() {
            let (a, b) = (lay.disp.d2m(range.start), lay.disp.d2m(range.end));
            let mut rects = Vec::new();
            self.range_rects(index, a, b, &mut rects, false);
            if rects.iter().any(|r| r.contains(&p)) {
                return Some((index, n));
            }
        }
        None
    }

    fn on_mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if !self.read_only {
            window.request_text_input();
        }
        window.focus(&self.focus_handle, cx);
        let p = self.local(event.position);
        if let Some(index) = self.checkbox_at(p) {
            if !self.read_only {
                if !self.theme.reduced_motion { self.check_press = Some((index, Instant::now(), true)); }
                self.core.toggle_task(index);
                self.after_edit(cx);
            }
            return;
        }
        if let Some((index, n)) = self.chip_at(p) {
            if let Some(chip) = self.lay(index).and_then(|l| l.disp.chips.get(n)).cloned() {
                if chip.clickable && event.click_count == 1 && !event.modifiers.shift {
                    self.emit_link(format!("kybern://task/{}", chip.task_id));
                    return;
                }
            }
        }
        if event.modifiers.platform {
            if let Some((index, n)) = self.link_at(p) {
                if let Some((_, link)) = self.lay(index).and_then(|l| l.disp.links.get(n)).cloned() {
                    self.emit_link(link.href.clone());
                    return;
                }
            }
        }
        let pos = self.pos_at_point(p);
        self.goal_x = None;
        match event.click_count {
            n if n >= 3 => {
                let len = self.core.doc.block(pos.block).len();
                let (a, b) = (Pos::new(pos.block, 0), Pos::new(pos.block, len));
                self.core.set_selection(Sel { anchor: a, head: b });
                self.selecting = Some(SelectMode::Block(a, b));
            }
            2 => {
                let block = self.core.doc.block(pos.block);
                let (a, b) = if block.kind.is_atom() {
                    (Pos::new(pos.block, 0), Pos::new(pos.block, ATOM_LEN))
                } else {
                    let range = crate::text::selection::word_range(&block.text, pos.offset);
                    (Pos::new(pos.block, range.start), Pos::new(pos.block, range.end))
                };
                self.core.set_selection(Sel { anchor: a, head: b });
                self.selecting = Some(SelectMode::Word(a, b));
            }
            _ if event.modifiers.shift => {
                let anchor = self.core.sel.anchor;
                self.core.set_selection(Sel { anchor, head: pos });
                self.selecting = Some(SelectMode::Char);
            }
            _ => {
                let block = self.core.doc.block(pos.block);
                if block.kind.is_atom() {
                    // A click on an image or rule selects it.
                    self.core.set_selection(Sel { anchor: Pos::new(pos.block, 0), head: Pos::new(pos.block, ATOM_LEN) });
                } else {
                    self.core.set_selection(Sel::caret(pos));
                }
                self.selecting = Some(SelectMode::Char);
            }
        }
        self.touched(cx);
    }

    fn on_mouse_up(&mut self, _: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.selecting = None;
        if let Some((index, since, true)) = self.check_press {
            // Release from wherever the squash got to.
            let held = (since.elapsed().as_secs_f32() / 0.08).min(1.0);
            self.check_press = Some((index, Instant::now() - std::time::Duration::from_secs_f32(0.08 * (1.0 - held)), false));
            cx.notify();
        }
    }

    fn on_mouse_move(&mut self, event: &MouseMoveEvent, window: &mut Window, cx: &mut Context<Self>) {
        let p = self.local(event.position);
        if event.pressed_button == Some(MouseButton::Left) {
            if let Some(mode) = self.selecting {
                let bounds = self.last_bounds.map(|b| b.size).unwrap_or_default();
                let clamped = point(p.x.clamp(px(0.0), bounds.width), p.y.clamp(px(-1.0), bounds.height + px(1.0)));
                let pos = self.pos_at_point(clamped);
                let sel = match mode {
                    SelectMode::Char => Sel { anchor: self.core.sel.anchor, head: pos },
                    SelectMode::Word(a, b) | SelectMode::Block(a, b) => {
                        let (ext_a, ext_b) = if let SelectMode::Word(..) = mode {
                            let block = self.core.doc.block(pos.block);
                            if block.kind.is_atom() {
                                (Pos::new(pos.block, 0), Pos::new(pos.block, ATOM_LEN))
                            } else {
                                let r = crate::text::selection::word_range(&block.text, pos.offset);
                                (Pos::new(pos.block, r.start), Pos::new(pos.block, r.end))
                            }
                        } else {
                            (Pos::new(pos.block, 0), Pos::new(pos.block, self.core.doc.block(pos.block).len()))
                        };
                        if ext_a < a {
                            Sel { anchor: b, head: ext_a }
                        } else {
                            Sel { anchor: a, head: ext_b.max(b) }
                        }
                    }
                };
                if sel != self.core.sel {
                    self.core.set_selection(sel);
                    self.touched(cx);
                }
                return;
            }
        }
        let hover = if let Some(i) = self.checkbox_at(p) {
            Hover::Check(i)
        } else if let Some((i, n)) = self.chip_at(p) {
            Hover::Chip(i, n)
        } else if let Some((i, n)) = self.link_at(p) {
            Hover::Link(i, n)
        } else {
            Hover::None
        };
        let _ = window;
        let bounds = self.last_bounds.map(|b| b.size).unwrap_or_default();
        let inside = p.x >= px(-40.0) && p.x <= bounds.width + px(8.0) && p.y >= px(0.0) && p.y <= bounds.height;
        let line = if self.make_task && inside && !self.read_only {
            let index = self.block_at_y(f32::from(p.y));
            let place = self.places[index];
            (f32::from(p.y) >= place.top && f32::from(p.y) <= place.top + place.height && self.can_make_task(index)).then_some(index)
        } else {
            None
        };
        if hover != self.hover || line != self.hover_line {
            self.hover = hover;
            self.hover_line = line;
            cx.notify();
        }
    }
}

/// Each visual row of a shaped line: its byte range and its start x in the unwrapped layout.
fn row_spans(line: &WrappedLine) -> Vec<(usize, usize, f32)> {
    let layout = &line.unwrapped_layout;
    let mut rows = Vec::with_capacity(line.wrap_boundaries().len() + 1);
    let (mut start, mut start_x) = (0usize, 0.0f32);
    for boundary in line.wrap_boundaries() {
        let glyph = &layout.runs[boundary.run_ix].glyphs[boundary.glyph_ix];
        rows.push((start, glyph.index, start_x));
        start = glyph.index;
        start_x = f32::from(glyph.position.x);
    }
    rows.push((start, line.len(), start_x));
    rows
}

fn previous_cluster(text: &str, offset: usize) -> usize {
    text.grapheme_indices(true).rev().find_map(|(i, _)| (i < offset).then_some(i)).unwrap_or(0)
}

fn next_cluster(text: &str, offset: usize) -> usize {
    text.grapheme_indices(true).find_map(|(i, _)| (i > offset).then_some(i)).unwrap_or(text.len())
}

/// The desktop's slash and "Turn into" items: `text`, `h1`–`h3`, `bullet`, `number`,
/// `checklist`, `quote`, `code`, `divider`. `slash` runs them as the slash menu does
/// (lists and code toggle; the block keeps its quote); otherwise as "Turn into", which
/// first clears the blocks to plain paragraphs (`clearNodes`, quotes and lists lifted).
fn turn_into(core: &mut Core, id: &str, slash: bool) {
    let kind = match id {
        "text" => Kind::Paragraph,
        "h1" => Kind::Heading(1),
        "h2" => Kind::Heading(2),
        "h3" => Kind::Heading(3),
        "bullet" => Kind::Bullet,
        "number" => Kind::Ordered(1),
        "checklist" => Kind::Task(false),
        "code" => Kind::Code(String::new()),
        "divider" => {
            core.insert_block(Kind::Rule);
            return;
        }
        "quote" => Kind::Paragraph,
        _ => return,
    };
    let family = |k: &Kind| std::mem::discriminant(k);
    core.step(|core| {
        let (start, end) = (core.sel.start().block, core.sel.end().block);
        let range = start..=end;
        let all_quoted = range.clone().all(|i| core.doc.block(i).quote > 0);
        let same = range.clone().all(|i| family(&core.doc.block(i).kind) == family(&kind));
        for index in range {
            if core.doc.block(index).kind.is_atom() {
                continue;
            }
            if !slash {
                core.doc.set_kind(index, Kind::Paragraph);
                let b = core.doc.edit(index);
                b.quote = 0;
                b.depth = 0;
                b.cont = false;
            }
            if id == "quote" {
                let b = core.doc.edit(index);
                b.quote = if slash && all_quoted { b.quote - 1 } else { b.quote.max(1) };
                continue;
            }
            let toggle_off = slash && same && (kind.is_list() || kind.is_literal());
            let target = if toggle_off { Kind::Paragraph } else { kind.clone() };
            core.doc.set_kind(index, target);
        }
        let sel = core.sel;
        core.sel = Sel { anchor: core.doc.clamp(sel.anchor), head: core.doc.clamp(sel.head) };
    });
}

/// Insert copied blocks at the selection: the first merges into the caret's block, the
/// last takes the text after the caret, and the ones between stand as they were.
fn paste_blocks(core: &mut Core, blocks: Vec<Block>) {
    core.step(|core| {
        let start = core.doc.delete(core.sel.start(), core.sel.end());
        core.sel = Sel::caret(start);
        if blocks.len() == 1 && !blocks[0].kind.is_atom() && !core.doc.block(start.block).kind.is_atom() {
            let block = &blocks[0];
            let target = core.doc.block(start.block).kind.clone();
            if target.is_literal() || block.kind.is_literal() {
                let single = Doc::new(vec![block.clone()]);
                let label = single.text_between(Pos::new(0, 0), single.end(), "\n", true);
                core.insert_text(&label, 0, false);
                return;
            }
            let at = start.offset;
            let b = core.doc.edit(start.block);
            let (tail_text, tail_runs) = b.slice(at, b.len());
            let len = b.len();
            b.remove(at, len);
            b.append(&block.text, &block.runs);
            let caret = b.len();
            b.append(&tail_text, &tail_runs);
            core.sel = Sel::caret(Pos::new(start.block, caret));
            return;
        }
        // Split at the caret, then put the blocks between the halves.
        let at_block_start = start.offset == 0;
        let current_empty = core.doc.block(start.block).is_empty();
        let mut index = if current_empty {
            core.doc.remove_block(start.block);
            start.block
        } else if at_block_start {
            start.block
        } else {
            let after = core.doc.split(start);
            after.block
        };
        let mut last = Pos::new(index, 0);
        for mut block in blocks {
            block.id = core.doc.fresh_id();
            let len = block.len();
            core.doc.insert_block(index.min(core.doc.len()), block);
            last = Pos::new(index, len);
            index += 1;
        }
        core.sel = Sel::caret(core.doc.clamp(last));
    });
}

// ------------------------------------------------------------ input (IME) ----

impl EntityInputHandler for RichEditorState {
    fn text_for_range(&mut self, range: Range<usize>, actual: &mut Option<Range<usize>>, _: &mut Window, _: &mut Context<Self>) -> Option<String> {
        let doc = &self.core.doc;
        let start = doc.pos_from_flat_utf16(range.start);
        let end = doc.pos_from_flat_utf16(range.end.max(range.start));
        actual.replace(doc.flat_utf16(start)..doc.flat_utf16(end));
        Some(doc.text_between(start, end, "\n", false))
    }

    fn selected_text_range(&mut self, _: bool, _: &mut Window, _: &mut Context<Self>) -> Option<UTF16Selection> {
        let doc = &self.core.doc;
        let sel = self.core.sel;
        Some(UTF16Selection { range: doc.flat_utf16(sel.start())..doc.flat_utf16(sel.end()), reversed: sel.reversed() })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        let doc = &self.core.doc;
        self.core.marked.map(|(a, b)| doc.flat_utf16(a)..doc.flat_utf16(b))
    }

    fn unmark_text(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.core.unmark();
        self.core.history.seal();
        cx.notify();
    }

    fn replace_text_in_range(&mut self, range: Option<Range<usize>>, text: &str, _: &mut Window, cx: &mut Context<Self>) {
        if !self.editable() {
            return;
        }
        if let Some(range) = range.filter(|_| self.core.marked.is_none()) {
            let doc = &self.core.doc;
            let (a, b) = (doc.pos_from_flat_utf16(range.start), doc.pos_from_flat_utf16(range.end));
            if (a, b) != (self.core.sel.start(), self.core.sel.end()) {
                self.core.sel = Sel { anchor: a, head: b };
            }
        }
        let now = self.now();
        // A Return from the input method (Japanese commit keys reach here as text) is Enter.
        if text == "\n" && self.core.marked.is_none() {
            self.core.enter(now);
        } else {
            self.core.insert_text(text, now, true);
        }
        self.after_edit(cx);
    }

    fn replace_and_mark_text_in_range(&mut self, range: Option<Range<usize>>, text: &str, selected: Option<Range<usize>>, _: &mut Window, cx: &mut Context<Self>) {
        if !self.editable() {
            return;
        }
        if let Some(range) = range.filter(|_| self.core.marked.is_none()) {
            let doc = &self.core.doc;
            let (a, b) = (doc.pos_from_flat_utf16(range.start), doc.pos_from_flat_utf16(range.end));
            self.core.sel = Sel { anchor: a, head: b };
        }
        let now = self.now();
        self.core.set_marked(text, now);
        if let (Some(selected), Some((start, _))) = (selected, self.core.marked) {
            let single = text.replace('\n', " ");
            let a = start.offset + byte_from_utf16(&single, selected.start);
            let b = start.offset + byte_from_utf16(&single, selected.end);
            self.core.sel = Sel { anchor: Pos::new(start.block, a), head: Pos::new(start.block, b) };
        }
        self.after_edit(cx);
    }

    fn bounds_for_range(&mut self, range: Range<usize>, bounds: Bounds<Pixels>, _: &mut Window, _: &mut Context<Self>) -> Option<Bounds<Pixels>> {
        let pos = self.core.doc.pos_from_flat_utf16(range.start);
        let (p, lh) = self.point_for(pos)?;
        Some(Bounds::new(bounds.origin + p, size(px(CARET_WIDTH), px(lh))))
    }

    fn character_index_for_point(&mut self, position: Point<Pixels>, _: &mut Window, _: &mut Context<Self>) -> Option<usize> {
        let p = self.local(position);
        let pos = self.pos_at_point(p);
        Some(self.core.doc.flat_utf16(pos))
    }

    fn set_selected_text_range(&mut self, range: Range<usize>, _: &mut Window, cx: &mut Context<Self>) {
        let doc = &self.core.doc;
        let sel = Sel { anchor: doc.pos_from_flat_utf16(range.start), head: doc.pos_from_flat_utf16(range.end) };
        self.core.set_selection(sel);
        self.touched(cx);
    }

    fn text_length_utf16(&mut self, _: &mut Window, _: &mut Context<Self>) -> Option<usize> {
        Some(self.core.doc.flat_len_utf16())
    }

    fn accepts_text_input(&self, _: &mut Window, _: &mut Context<Self>) -> bool {
        !self.read_only
    }
}

// --------------------------------------------------------------- render ----

impl gpui::Render for RichEditorState {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let key_down_callback = self.callback.clone();
        let key_up_callback = self.callback.clone();
        let element_id = self.element_id;
        let emits_key_down = self.emits_key_down;
        div()
            .key_context(KEY_CONTEXT)
            .track_focus(&self.focus_handle)
            .cursor(CursorStyle::IBeam)
            .on_action(cx.listener(Self::backspace))
            .on_action(cx.listener(Self::delete))
            .on_action(cx.listener(Self::left))
            .on_action(cx.listener(Self::right))
            .on_action(cx.listener(Self::up))
            .on_action(cx.listener(Self::down))
            .on_action(cx.listener(Self::select_left))
            .on_action(cx.listener(Self::select_right))
            .on_action(cx.listener(Self::select_up))
            .on_action(cx.listener(Self::select_down))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(Self::home))
            .on_action(cx.listener(Self::end))
            .on_action(cx.listener(Self::doc_start))
            .on_action(cx.listener(Self::doc_end))
            .on_action(cx.listener(Self::select_home))
            .on_action(cx.listener(Self::select_end))
            .on_action(cx.listener(Self::select_doc_start))
            .on_action(cx.listener(Self::select_doc_end))
            .on_action(cx.listener(Self::word_left_action))
            .on_action(cx.listener(Self::word_right_action))
            .on_action(cx.listener(Self::select_word_left))
            .on_action(cx.listener(Self::select_word_right))
            .on_action(cx.listener(Self::delete_word_left))
            .on_action(cx.listener(Self::delete_word_right))
            .on_action(cx.listener(Self::delete_to_line_start))
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::cut))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::paste_plain))
            .on_action(cx.listener(Self::undo))
            .on_action(cx.listener(Self::redo))
            .on_action(cx.listener(Self::enter))
            .on_action(cx.listener(Self::hard_break))
            .on_action(cx.listener(Self::indent))
            .on_action(cx.listener(Self::outdent))
            .on_action(cx.listener(Self::bold))
            .on_action(cx.listener(Self::italic))
            .on_action(cx.listener(Self::strike))
            .on_action(cx.listener(Self::inline_code))
            .on_action(cx.listener(Self::heading1))
            .on_action(cx.listener(Self::heading2))
            .on_action(cx.listener(Self::heading3))
            .on_action(cx.listener(Self::paragraph))
            .on_action(cx.listener(Self::bullet_list))
            .on_action(cx.listener(Self::ordered_list))
            .on_action(cx.listener(Self::task_list))
            .on_action(cx.listener(Self::blockquote))
            .on_action(cx.listener(Self::code_block))
            .on_action(cx.listener(Self::make_task))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .when(emits_key_down, move |editor| {
                editor.on_key_down(move |event, _window, _cx| {
                    if crate::key_intercept::replaying() {
                        return;
                    }
                    emit_event_full(&key_down_callback, element_id, "keyDown", |payload| {
                        payload.key = Some(event.keystroke.key.clone());
                        payload.key_char = event.keystroke.key_char.clone();
                        payload.is_held = Some(event.is_held);
                        payload.modifiers = Some(event.keystroke.modifiers.into());
                    });
                })
            })
            .on_key_up(move |event, _window, _cx| {
                emit_event_full(&key_up_callback, element_id, "keyUp", |payload| {
                    payload.key = Some(event.keystroke.key.clone());
                    payload.modifiers = Some(event.keystroke.modifiers.into());
                });
            })
            .w_full()
            .min_w_0()
            .child(DocElement { editor: cx.entity() })
    }
}

struct DocElement {
    editor: Entity<RichEditorState>,
}

impl gpui::IntoElement for DocElement {
    type Element = Self;
    fn into_element(self) -> Self::Element {
        self
    }
}

impl gpui::Element for DocElement {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<gpui::ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(&mut self, _: Option<&GlobalElementId>, _: Option<&gpui::InspectorElementId>, window: &mut Window, _: &mut App) -> (LayoutId, ()) {
        let mut style = Style::default();
        style.size.width = relative(1.0).into();
        let editor = self.editor.clone();
        let layout = window.request_measured_layout(style, move |known, available, window, cx| {
            let width = known.width.unwrap_or(match available.width {
                gpui::AvailableSpace::Definite(width) => width,
                _ => px(640.0),
            });
            let height = editor.update(cx, |editor, _| editor.layout_all(f32::from(width), window));
            size(width, px(height))
        });
        (layout, ())
    }

    fn prepaint(&mut self, _: Option<&GlobalElementId>, _: Option<&gpui::InspectorElementId>, bounds: Bounds<Pixels>, _: &mut (), window: &mut Window, cx: &mut App) {
        self.editor.update(cx, |editor, _| {
            if (f32::from(bounds.size.width) - editor.width).abs() > 0.5 {
                editor.layout_all(f32::from(bounds.size.width), window);
            }
            editor.last_bounds = Some(bounds);
            editor.hitboxes.clear();
            // A checklist item that changed state since the last frame starts its tick transition.
            let now = Instant::now();
            let mut seen = HashMap::new();
            for block in editor.core.doc.blocks.iter() {
                if let Kind::Task(checked) = block.kind {
                    if let Some(&was) = editor.checks_seen.get(&block.id) {
                        if was != checked && !editor.theme.reduced_motion {
                            editor.check_anims.insert(block.id, (now, checked));
                        }
                    }
                    seen.insert(block.id, checked);
                }
            }
            editor.checks_seen = seen;
            let origin = bounds.origin;
            let mut boxes = Vec::new();
            for index in 0..editor.core.doc.len() {
                let block = editor.core.doc.block(index);
                if let Kind::Task(_) = block.kind {
                    let place = editor.places[index];
                    let check = 1.067 * editor.theme.size;
                    let pad = 0.3 * editor.theme.size;
                    boxes.push((Bounds::new(origin + point(px(place.list_x - pad), px(place.ty - pad)), size(px(check + 2.0 * pad), px(editor.lay(index).map(|l| l.style.line_h).unwrap_or(check) + 2.0 * pad))), CursorStyle::PointingHand));
                }
                if let Some(lay) = editor.lay(index) {
                    for chip in lay.disp.chips.iter().filter(|c| c.clickable) {
                        let mut rects = Vec::new();
                        editor.range_rects(index, chip.model, chip.model + ATOM_LEN, &mut rects, false);
                        for r in rects {
                            boxes.push((Bounds::new(origin + r.origin, r.size), CursorStyle::PointingHand));
                        }
                    }
                }
            }
            for (b, style) in boxes {
                let hitbox = window.insert_hitbox(b, HitboxBehavior::Normal);
                editor.hitboxes.push((hitbox, style));
            }
        });
    }

    fn paint(&mut self, _: Option<&GlobalElementId>, _: Option<&gpui::InspectorElementId>, bounds: Bounds<Pixels>, _: &mut (), _: &mut (), window: &mut Window, cx: &mut App) {
        let focus_handle = self.editor.read(cx).focus_handle.clone();
        if !self.editor.read(cx).read_only {
            window.handle_input(&focus_handle, ElementInputHandler::new(bounds, self.editor.clone()), cx);
        }
        let editor = self.editor.clone();
        window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
            if phase == DispatchPhase::Bubble {
                editor.update(cx, |editor, cx| editor.on_mouse_move(event, window, cx));
            }
        });
        self.editor.update(cx, |editor, cx| {
            for (hitbox, style) in &editor.hitboxes {
                window.set_cursor_style(*style, hitbox);
            }
            let caret = editor.caret_shown(window, cx);
            paint_doc(editor, bounds, caret, window, cx);
            // Report edits and the formatting state once per frame.
            let rev = editor.core.doc.rev();
            if rev != editor.emitted_rev {
                editor.emitted_rev = rev;
                editor.emit(serde_json::json!({"t": "edit", "rev": rev}));
            }
            let state = editor.state_json(window).to_string();
            if state != editor.emitted_state {
                editor.emitted_state = state.clone();
                editor.pending_events.push(state);
            }
            editor.flush_events();
        });
    }
}

fn quad(bounds: Bounds<Pixels>, color: Hsla, radius: f32) -> gpui::PaintQuad {
    fill(bounds, color).corner_radii(px(radius))
}

fn paint_doc(editor: &mut RichEditorState, bounds: Bounds<Pixels>, caret_on: bool, window: &mut Window, cx: &mut App) {
    let origin = bounds.origin;
    let theme = editor.theme.clone();
    let em = theme.size;
    let visible = window.content_mask().bounds;
    let (vis_top, vis_bottom) = (f32::from(visible.top() - origin.y), f32::from(visible.bottom() - origin.y));
    let sel = editor.core.sel;
    let focused = editor.focus_handle.is_focused(window);
    let mut painted_text = String::new();
    let selection_rects = editor.selection_rects();
    let count = editor.core.doc.len();
    // Quote bars: one continuous bar per run of quoted blocks and level.
    let mut quote_runs: Vec<(u8, f32, f32)> = Vec::new();
    for index in 0..count {
        let block = editor.core.doc.block(index);
        let place = editor.places[index];
        for level in 1..=block.quote {
            let x = (level - 1) as f32 * 16.0;
            let top = place.top;
            let bottom = place.top + place.height;
            match quote_runs.iter_mut().rev().find(|(l, _, b)| *l == level && index > 0 && editor.core.doc.block(index - 1).quote >= level && (*b - top).abs() < 200.0 && *b <= top + 0.5) {
                Some(run) => run.2 = bottom,
                None => quote_runs.push((level, top, bottom)),
            }
            let _ = x;
        }
    }
    for (level, top, bottom) in quote_runs {
        if bottom < vis_top || top > vis_bottom {
            continue;
        }
        let x = (level - 1) as f32 * 16.0;
        window.paint_quad(fill(Bounds::new(origin + point(px(x), px(top)), size(px(2.0), px(bottom - top))), theme.fg4));
    }
    for index in 0..count {
        let place = editor.places[index];
        if place.top + place.height < vis_top - 40.0 || place.top > vis_bottom + 40.0 {
            continue;
        }
        let block = editor.core.doc.blocks[index].clone();
        let Some(lay) = editor.layouts.get(&block.id) else { continue };
        let lh = lay.style.line_h;
        let text_origin = origin + point(px(place.tx), px(place.ty));
        match &block.kind {
            Kind::Code(_) => {
                let wash = Bounds::new(origin + point(px(place.tx - 16.0), px(place.top)), size(px(place.tw + 32.0), px(place.height)));
                window.paint_quad(quad(wash, theme.fill, 10.0));
            }
            Kind::Rule => {
                let selected = !sel.is_empty() && sel.start() <= Pos::new(index, 0) && sel.end() >= Pos::new(index, ATOM_LEN);
                let color = if selected { theme.focus } else { theme.hair };
                window.paint_quad(fill(Bounds::new(origin + point(px(place.tx), px(place.top)), size(px(place.tw), px(1.0))), color));
                continue;
            }
            Kind::Image { .. } => {
                let selected = !sel.is_empty() && sel.start() <= Pos::new(index, 0) && sel.end() >= Pos::new(index, ATOM_LEN);
                let (w, h) = lay.image_size;
                let b = Bounds::new(origin + point(px(place.tx), px(place.top)), size(px(w), px(h)));
                if let Some(image) = &lay.image {
                    let ring = if selected { 2.0 } else { 1.0 };
                    let ring_color = if selected { theme.focus } else { theme.hair };
                    window.paint_quad(quad(b.dilate(px(ring)), ring_color, 8.0 + ring));
                    let _ = window.paint_image(b, b, Corners::all(px(8.0)), image.clone(), 0, false);
                } else {
                    window.paint_quad(quad(b, theme.hover, 8.0));
                    window.paint_quad(gpui::outline(b, theme.hair, gpui::BorderStyle::Solid).corner_radii(px(8.0)));
                    if lay.image_failed {
                        let label: SharedString = "Image unavailable. Click to try again".into();
                        let meta = (em * 12.0 / 15.0).max(11.0);
                        let line = window.text_system().shape_line(label.clone(), px(meta), &[run(label.len(), font(&theme.font, 400.0, false), theme.fg3)], None);
                        let x = f32::from(b.origin.x) + (w - f32::from(line.width)) / 2.0;
                        let y = f32::from(b.origin.y) + (h - meta * 1.4) / 2.0;
                        line.paint(point(px(x), px(y)), px(meta * 1.4), gpui::TextAlign::Left, None, window, cx).ok();
                    }
                    if selected {
                        window.paint_quad(gpui::outline(b.dilate(px(2.0)), theme.focus, gpui::BorderStyle::Solid).corner_radii(px(10.0)));
                    }
                }
                continue;
            }
            _ => {}
        }
        // "Make task": a quiet fill behind the checklist line under the pointer.
        if editor.hover_line == Some(index) && editor.make_task {
            let check = 1.067 * em;
            let (pad_y, gap, pad_x, radius) = if theme.compact { (2.0, 9.0, 6.0, 6.0) } else { (4.0, 12.0, 8.0, 8.0) };
            let left = place.tx - check - gap - pad_x;
            let fill_b = Bounds::new(origin + point(px(left), px(place.ty - pad_y)), size(px(place.tw + check + gap + 2.0 * pad_x), px(lay.text_h + 2.0 * pad_y)));
            window.paint_quad(quad(fill_b, theme.hover, radius));
        }
        // Inline code washes and chip hovers sit under the text.
        for range in &lay.disp.codes {
            let mut rects = Vec::new();
            editor.display_rects(index, range.start, range.end, &mut rects, false);
            for r in rects {
                let pad_y = 0.13 * em * 0.833;
                let pad_x = 0.33 * em * 0.833;
                let inner_h = em * 0.833 * 1.2;
                let y = f32::from(r.origin.y) + (lh - inner_h) / 2.0 - pad_y;
                let wash = Bounds::new(origin + point(r.origin.x - px(pad_x), px(y)), size(r.size.width + px(2.0 * pad_x), px(inner_h + 2.0 * pad_y)));
                window.paint_quad(quad(wash, theme.fill, 5.0));
            }
        }
        for (n, chip) in lay.disp.chips.iter().enumerate() {
            let selected_atom = !sel.is_empty() && sel.start() <= Pos::new(index, chip.model) && sel.end() >= Pos::new(index, chip.model + ATOM_LEN);
            let hovered = editor.hover == Hover::Chip(index, n) && chip.clickable;
            if hovered || selected_atom {
                let mut rects = Vec::new();
                editor.range_rects(index, chip.model, chip.model + ATOM_LEN, &mut rects, false);
                for r in rects {
                    let inner_h = em * 13.0 / 15.0 + 4.0;
                    let y = f32::from(r.origin.y) + (lh - inner_h) / 2.0;
                    let b = Bounds::new(origin + point(r.origin.x - px(3.0), px(y)), size(r.size.width + px(6.0), px(inner_h)));
                    window.paint_quad(quad(b, if selected_atom { theme.sel } else { theme.hover }, 5.0));
                }
            }
        }
        // Selection.
        for r in selection_rects.iter() {
            let top = f32::from(r.origin.y);
            let gap_owner = index + 1 < count && top >= place.top + place.height - 0.5 && top < editor.places[index + 1].top;
            if (top >= place.top - 0.5 && top < place.top + place.height) || gap_owner {
                window.paint_quad(fill(Bounds::new(origin + r.origin, r.size), theme.selection));
            }
        }
        // Markers.
        match &block.kind {
            Kind::Bullet | Kind::Ordered(_) => {
                let first_mid = place.ty + lh / 2.0;
                if matches!(block.kind, Kind::Bullet) {
                    let level = list_level(&editor.core.doc, index);
                    let d = (em * 0.36).max(4.0);
                    let cx_ = place.tx - em * 0.9;
                    let b = Bounds::new(origin + point(px(cx_ - d / 2.0), px(first_mid - d / 2.0 + em * 0.04)), size(px(d), px(d)));
                    match level % 3 {
                        0 => window.paint_quad(quad(b, theme.fg3, d / 2.0)),
                        1 => window.paint_quad(gpui::outline(b, theme.fg3, gpui::BorderStyle::Solid).corner_radii(px(d / 2.0))),
                        _ => window.paint_quad(fill(b, theme.fg3)),
                    }
                } else {
                    let n = editor.core.doc.ordinal(index);
                    let level = list_level(&editor.core.doc, index);
                    let label: SharedString = if level % 2 == 1 { format!("{}.", alpha(n)) } else { format!("{n}.") }.into();
                    let mut f = font(&theme.font, lay.style.weight, false);
                    f.features = gpui::FontFeatures(Arc::new(vec![("tnum".into(), 1)]));
                    let line = window.text_system().shape_line(label.clone(), px(lay.style.size), &[run(label.len(), f, theme.fg3)], None);
                    let x = place.tx - em * 0.3 - f32::from(line.width);
                    line.paint(origin + point(px(x), px(place.ty)), px(lh), gpui::TextAlign::Left, None, window, cx).ok();
                }
            }
            Kind::Task(checked) => {
                // `.note-prose ul[data-type=taskList] > li > label`: the fill eases over 150 ms
                // (--ease-out), the tick scales from 0.6 (--ease-smooth-out) and fades in 80 ms.
                let now = Instant::now();
                // A held box squashes to 0.92 about its centre (80 ms `--ease-out`) and springs back on release.
                let press = match editor.check_press {
                    Some((i, since, down)) if i == index => {
                        let e = bezier(0.23, 1.0, 0.32, 1.0, (now.duration_since(since).as_secs_f32() / 0.08).min(1.0));
                        if down { 1.0 - 0.08 * e } else { 0.92 + 0.08 * e }
                    }
                    _ => 1.0,
                };
                if matches!(editor.check_press, Some((i, since, false)) if i == index && now.duration_since(since).as_secs_f32() >= 0.08) {
                    editor.check_press = None;
                } else if press != 1.0 || matches!(editor.check_press, Some((i, _, _)) if i == index) {
                    window.request_animation_frame();
                }
                let check = 1.067 * em * press;
                let b = Bounds::new(origin + point(px(place.list_x + 1.067 * em * (1.0 - press) / 2.0), px(place.ty + (lh - check) / 2.0)), size(px(check), px(check)));
                let r = 0.3 * em * press;
                let anim = editor.check_anims.get(&block.id).copied().filter(|(start, _)| now.duration_since(*start).as_secs_f32() < 0.15);
                let (fill_t, tick_scale, tick_alpha) = match anim {
                    Some((start, on)) => {
                        let e = now.duration_since(start).as_secs_f32();
                        let fill = bezier(0.23, 1.0, 0.32, 1.0, e / 0.15);
                        let scale = bezier(0.22, 1.0, 0.36, 1.0, e / 0.15);
                        let alpha = (e / 0.08).min(1.0);
                        if on { (fill, 0.6 + 0.4 * scale, alpha) } else { (1.0 - fill, 1.0 - 0.4 * scale, 1.0 - alpha) }
                    }
                    None => if *checked { (1.0, 1.0, 1.0) } else { (0.0, 0.6, 0.0) },
                };
                if anim.is_some() {
                    window.request_animation_frame();
                } else {
                    editor.check_anims.remove(&block.id);
                }
                let ring = if editor.hover == Hover::Check(index) { theme.fg3 } else { theme.fg4 };
                if fill_t < 1.0 {
                    let mut ring_c = ring;
                    ring_c.a *= 1.0 - fill_t;
                    window.paint_quad(gpui::quad(b, Corners::all(px(r)), gpui::transparent_black(), px(1.5), ring_c, gpui::BorderStyle::Solid));
                }
                if fill_t > 0.0 {
                    window.paint_quad(quad(b, mix(gpui::transparent_black(), theme.check_on, fill_t), r));
                }
                if tick_alpha > 0.0 {
                    // The fade is quantized to quarter steps: each is one cached raster of the tick.
                    let image = if tick_alpha >= 1.0 {
                        editor.glyphs.get("tick").map(|g| g.image.clone())
                    } else {
                        editor.glyph_source.get("tick").and_then(|(svg, color)| {
                            let step = (tick_alpha * 4.0).ceil() / 4.0;
                            let c = crate::color::parse_color_rgba(color)?;
                            crate::kybern_icon::image(svg, &format!("rgba({},{},{},{})", (c.r * 255.0).round(), (c.g * 255.0).round(), (c.b * 255.0).round(), c.a * step))
                        })
                    };
                    if let Some(image) = image {
                        let side = check * tick_scale;
                        let tb = Bounds::new(b.center() - point(px(side / 2.0), px(side / 2.0)), size(px(side), px(side)));
                        let _ = window.paint_image(tb, tb, Corners::default(), image, 0, false);
                    }
                }
            }
            _ => {}
        }
        // Text.
        let mut y = f32::from(text_origin.y);
        for line in &lay.lines {
            let h = f32::from(line.size(px(lh)).height);
            if y + h >= f32::from(visible.top()) - lh && y <= f32::from(visible.bottom()) + lh {
                line.paint(point(text_origin.x, px(y)), px(lh), gpui::TextAlign::Left, None, window, cx).ok();
            }
            y += h;
        }
        painted_text.push_str(&lay.disp.text);
        painted_text.push('\n');
        // Link underlines (`text-underline-offset: 0.18em`, 35% until hovered).
        for (n, (range, _)) in lay.disp.links.iter().enumerate() {
            let (a, b) = (lay.disp.d2m(range.start), lay.disp.d2m(range.end));
            let mut rects = Vec::new();
            editor.range_rects(index, a, b, &mut rects, false);
            let hovered = editor.hover == Hover::Link(index, n);
            let mut c = theme.link;
            if !hovered {
                c.a *= 0.35;
            }
            for r in rects {
                let baseline = f32::from(r.origin.y) + lh / 2.0 + lay.style.size * 0.35;
                let y = baseline + lay.style.size * 0.18 - lay.style.size * 0.08;
                window.paint_quad(fill(Bounds::new(origin + point(r.origin.x, px(y)), size(r.size.width, px(1.0))), c));
            }
        }
        // Task glyphs over their reserved room.
        for chip in &lay.disp.chips {
            let Some(glyph) = editor.glyphs.get(&chip.glyph_key) else { continue };
            let mut rects = Vec::new();
            editor.range_rects(index, chip.model, chip.model + ATOM_LEN, &mut rects, false);
            if let Some(r) = rects.first() {
                let g = em * 13.0 / 15.0;
                let b = Bounds::new(origin + point(r.origin.x, r.origin.y + px((lh - g) / 2.0)), size(px(g), px(g)));
                crate::kybern_icon::paint(window, &glyph.image, b);
            }
        }
        // Composition underline.
        if let Some((a, b)) = editor.core.marked {
            if a.block == index {
                let mut rects = Vec::new();
                editor.range_rects(index, a.offset, b.offset, &mut rects, false);
                for r in rects {
                    let y = f32::from(r.origin.y) + lh / 2.0 + lay.style.size * 0.5;
                    window.paint_quad(fill(Bounds::new(origin + point(r.origin.x, px(y)), size(r.size.width, px(1.0))), lay.style.color));
                }
            }
        }
        // Placeholder: the empty line being written ("Heading 2" on an empty heading).
        let show_placeholder = block.is_empty() && block.quote == 0 && !block.kind.is_list() && !block.cont && !block.kind.is_literal() && sel.head.block == index && sel.is_empty() && !editor.read_only && (focused || count == 1);
        if show_placeholder {
            let text: String = match block.kind {
                Kind::Heading(level) => format!("Heading {level}"),
                _ => editor.placeholder.clone(),
            };
            if !text.is_empty() {
                let label: SharedString = text.into();
                let line = window.text_system().shape_line(label.clone(), px(lay.style.size), &[run(label.len(), font(&theme.font, lay.style.weight, false), theme.fg4)], None);
                line.paint(text_origin, px(lh), gpui::TextAlign::Left, None, window, cx).ok();
            }
        }
    }
    crate::text::log_painted_text(painted_text.into());
    // Caret.
    if caret_on && sel.is_empty() && focused {
        let head = sel.head;
        if !editor.core.doc.block(head.block).kind.is_atom() {
            if let Some((p, lh)) = editor.point_for(head) {
                let style = editor.lay(head.block).map(|l| l.style).unwrap();
                // WebKit draws the caret at the font's height, centred on the line.
                let h = (style.size * 1.25).min(lh);
                let y = f32::from(p.y) + (lh - h) / 2.0;
                window.paint_quad(fill(Bounds::new(origin + point(p.x, px(y)), size(px(CARET_WIDTH), px(h))), theme.caret));
            }
        }
    }
}

/// CSS `cubic-bezier(x1, y1, x2, y2)` at `t` (0–1).
fn bezier(x1: f32, y1: f32, x2: f32, y2: f32, t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    let curve = |a: f32, b: f32, u: f32| 3.0 * a * u * (1.0 - u).powi(2) + 3.0 * b * u * u * (1.0 - u) + u.powi(3);
    let (mut lo, mut hi) = (0.0f32, 1.0f32);
    for _ in 0..24 {
        let mid = (lo + hi) / 2.0;
        if curve(x1, x2, mid) < t { lo = mid } else { hi = mid }
    }
    curve(y1, y2, (lo + hi) / 2.0)
}

fn mix(a: Hsla, b: Hsla, t: f32) -> Hsla {
    let (a, b) = (gpui::Rgba::from(a), gpui::Rgba::from(b));
    Hsla::from(gpui::Rgba { r: a.r + (b.r - a.r) * t, g: a.g + (b.g - a.g) * t, b: a.b + (b.b - a.b) * t, a: a.a + (b.a - a.a) * t })
}

fn list_level(doc: &Doc, index: usize) -> usize {
    let block = doc.block(index);
    let mut level = 0;
    let mut depth = block.depth;
    let mut at = index;
    while depth > 0 && at > 0 {
        at -= 1;
        let b = doc.block(at);
        if b.depth < depth && b.kind.is_list() {
            if std::mem::discriminant(&b.kind) == std::mem::discriminant(&block.kind) {
                level += 1;
            }
            depth = b.depth;
        }
    }
    level
}

fn alpha(n: u32) -> String {
    let mut n = n.max(1);
    let mut out = Vec::new();
    while n > 0 {
        n -= 1;
        out.push((b'a' + (n % 26) as u8) as char);
        n /= 26;
    }
    out.iter().rev().collect()
}

// ---------------------------------------------------------------- napi ----

fn pos_json(doc: &Doc, pos: Pos) -> serde_json::Value {
    serde_json::json!({"block": pos.block, "offset": utf16_from_byte(&doc.block(pos.block).text, pos.offset)})
}

fn pos_from(doc: &Doc, value: Option<&serde_json::Value>) -> Option<Pos> {
    let value = value?;
    let block = value.get("block")?.as_u64()? as usize;
    let block = block.min(doc.len() - 1);
    let units = value.get("offset").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
    Some(doc.clamp(Pos::new(block, byte_from_utf16(&doc.block(block).text, units))))
}

/// `richEditorCommand(id, command)`: formatting, insertion and queries from JS.
pub(crate) fn command(id: u64, command: serde_json::Value, window: &mut Window, cx: &mut App) -> napi::Result<serde_json::Value> {
    let editor = editor_for(id)?;
    let name = command.get("cmd").and_then(|v| v.as_str()).unwrap_or_default().to_string();
    editor.update(cx, |state, cx| {
        let editable = state.editable() && state.core.marked.is_none();
        let s = |k: &str| command.get(k).and_then(|v| v.as_str()).map(str::to_string);
        let result = match name.as_str() {
            "getDoc" => return Ok(doc_json(&state.core.doc)),
            "getState" => return Ok(state.state_json(window)),
            "getSelection" => {
                let sel = state.core.sel;
                return Ok(serde_json::json!({"anchor": pos_json(&state.core.doc, sel.anchor), "head": pos_json(&state.core.doc, sel.head), "composing": state.core.marked.is_some()}));
            }
            "caretAt" => {
                // A drop or paste point in window coordinates becomes the caret.
                let x = command.get("x").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32;
                let y = command.get("y").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32;
                let p = state.local(point(px(x), px(y)));
                let pos = if f32::from(p.y) > state.height { state.core.doc.end() } else { state.pos_at_point(p) };
                state.core.set_selection(Sel::caret(pos));
                serde_json::Value::Null
            }
            "select" => {
                let anchor = pos_from(&state.core.doc, command.get("anchor")).unwrap_or_default();
                let head = pos_from(&state.core.doc, command.get("head")).unwrap_or(anchor);
                state.core.set_selection(Sel { anchor, head });
                serde_json::Value::Null
            }
            "layout" => {
                // Block geometry relative to the element, for checks.
                let origin = state.last_bounds.map(|b| b.origin).unwrap_or_default();
                return Ok(serde_json::json!({
                    "x": f32::from(origin.x), "y": f32::from(origin.y),
                    "blocks": state.places.iter().enumerate().map(|(i, p)| {
                        let rects = |ranges: Vec<(usize, usize)>| ranges.into_iter().filter_map(|(a, b)| { let mut out = Vec::new(); state.range_rects(i, a, b, &mut out, false); out.first().map(|r| [f32::from(r.origin.x), f32::from(r.origin.y), f32::from(r.size.width), f32::from(r.size.height)]) }).collect::<Vec<_>>();
                        let (chips, links) = state.lay(i).map(|l| (l.disp.chips.iter().map(|c| (c.model, c.model + ATOM_LEN)).collect::<Vec<_>>(), l.disp.links.iter().map(|(r, _)| (l.disp.d2m(r.start), l.disp.d2m(r.end))).collect::<Vec<_>>())).unwrap_or_default();
                        serde_json::json!({"top": p.top, "height": p.height, "tx": p.tx, "ty": p.ty, "tw": p.tw, "listX": p.list_x, "lineHeight": state.lay(i).map(|l| l.style.line_h), "chips": rects(chips), "links": rects(links)})
                    }).collect::<Vec<_>>(),
                }));
            }
            "stats" => {
                let reshaped = std::mem::take(&mut state.reshaped);
                return Ok(serde_json::json!({"reshaped": reshaped, "blocks": state.core.doc.len(), "undo": state.core.history.undo.len()}));
            }
            "ime" => {
                // The input method path (NSTextInputClient → EntityInputHandler), for checks.
                match s("op").as_deref() {
                    Some("mark") => {
                        let text = s("text").unwrap_or_default();
                        let len = text.encode_utf16().count();
                        EntityInputHandler::replace_and_mark_text_in_range(state, None, &text, Some(len..len), window, cx);
                    }
                    Some("commit") => EntityInputHandler::replace_text_in_range(state, None, &s("text").unwrap_or_default(), window, cx),
                    Some("unmark") => EntityInputHandler::unmark_text(state, window, cx),
                    _ => return Err(napi::Error::from_reason("Unknown ime op")),
                }
                let marked = EntityInputHandler::marked_text_range(state, window, cx);
                return Ok(serde_json::json!({"marked": marked.map(|r| [r.start, r.end])}));
            }
            "focus" => {
                let at = s("at");
                match at.as_deref() {
                    Some("start") => state.core.set_selection(Sel::caret(Pos::new(0, 0))),
                    Some("end") => {
                        let end = state.core.doc.end();
                        state.core.set_selection(Sel::caret(end))
                    }
                    _ => {}
                }
                window.focus(&state.focus_handle, cx);
                if !state.read_only {
                    window.request_text_input();
                }
                serde_json::Value::Null
            }
            _ if !editable => return Err(napi::Error::from_reason("Rich editor is read-only or composing")),
            "mark" => {
                let mark = match s("mark").as_deref() {
                    Some("bold") => BOLD,
                    Some("italic") => ITALIC,
                    Some("strike") => STRIKE,
                    Some("code") => CODE,
                    _ => return Err(napi::Error::from_reason("Unknown mark")),
                };
                if !state.core.doc.block(state.core.sel.head.block).kind.is_literal() {
                    state.core.toggle_mark(mark);
                }
                serde_json::Value::Null
            }
            "turnInto" => {
                turn_into(&mut state.core, &s("block").unwrap_or_default(), false);
                serde_json::Value::Null
            }
            "toggle" => {
                // The toolbar's list, code and quote buttons: TipTap's toggle commands.
                turn_into(&mut state.core, &s("block").unwrap_or_default(), true);
                serde_json::Value::Null
            }
            "slash" => {
                // Delete "/query", then run the item, as one undo step.
                let item = s("item").unwrap_or_default();
                if let Some((from, _)) = state.slash_query() {
                    let head = state.core.sel.head;
                    state.core.step(|core| {
                        core.delete_range(Pos::new(head.block, from), head);
                        turn_into(core, &item, true);
                    });
                }
                serde_json::Value::Null
            }
            "link" => {
                let href = s("href").filter(|h| !h.trim().is_empty());
                let text = s("text");
                if command.get("extend").and_then(|v| v.as_bool()).unwrap_or(true) && state.core.sel.is_empty() {
                    if let Some((a, b, _)) = state.core.active_link() {
                        state.core.sel = Sel { anchor: a, head: b };
                    }
                }
                state.core.set_link(href.map(|href| Link { href, title: None }), text.as_deref());
                serde_json::Value::Null
            }
            "selectLink" => {
                if let Some((a, b, _)) = state.core.active_link() {
                    state.core.set_selection(Sel { anchor: a, head: b });
                }
                serde_json::Value::Null
            }
            "insertImage" => {
                let src = s("src").unwrap_or_default();
                let alt = s("alt").unwrap_or_default();
                state.core.insert_block(Kind::Image { src, alt, title: None });
                serde_json::Value::Null
            }
            "insertTaskRef" => {
                state.core.insert_atom(Atom::TaskRef { id: s("id").unwrap_or_default().to_lowercase(), label: s("label").unwrap_or_default() }, true);
                serde_json::Value::Null
            }
            "insertText" => {
                let now = state.now();
                state.core.insert_text(&s("text").unwrap_or_default(), now, command.get("rules").and_then(|v| v.as_bool()).unwrap_or(false));
                state.core.history.seal();
                serde_json::Value::Null
            }
            "undo" => {
                state.core.undo();
                serde_json::Value::Null
            }
            "redo" => {
                state.core.redo();
                serde_json::Value::Null
            }
            "toggleTask" => {
                let index = command.get("block").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                if index < state.core.doc.len() {
                    state.core.toggle_task(index);
                }
                serde_json::Value::Null
            }
            _ => return Err(napi::Error::from_reason(format!("Unknown rich editor command {name}"))),
        };
        state.goal_x = None;
        state.touched(cx);
        Ok(result)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_round_trips_blocks() {
        let value = serde_json::json!({"blocks": [
            {"id": 1, "type": "heading", "level": 2, "content": [{"text": "Hi "}, {"text": "there", "marks": ["bold"]}]},
            {"id": 2, "type": "task", "checked": true, "depth": 1, "content": [{"text": "see "}, {"atom": {"type": "taskRef", "id": "x", "label": "ADE-1"}}]},
            {"id": 3, "type": "code", "lang": "rs", "text": "fn main() {}\n"},
            {"id": 4, "type": "image", "src": "kybern://asset/a", "alt": "shot", "title": null},
            {"id": 5, "type": "paragraph", "quote": 1, "content": [{"text": "q", "link": {"href": "https://x.io", "title": null}}]}
        ]});
        let doc = doc_from_json(&value);
        assert_eq!(doc_json(&doc), value);
    }

    #[test]
    fn display_maps_atoms_and_code_room() {
        let theme = DocTheme::default();
        let mut block = Block::new(1, Kind::Paragraph, "");
        block.text = format!("a{ATOM}bc");
        block.runs = vec![
            Run::text(1, 0, None),
            Run::atom(Atom::TaskRef { id: "t".into(), label: "ADE-1".into() }, 0, None),
            Run::text(2, CODE, None),
        ];
        let style = block_style(&block, &theme);
        let disp = build_display(&block, &theme, &style, &HashMap::new(), false);
        assert_eq!(disp.text, "a  ADE-1 bc ");
        assert_eq!(disp.m2d(0), 0);
        assert_eq!(disp.m2d(1), 1);
        assert_eq!(disp.m2d(1 + ATOM_LEN), 8);
        assert_eq!(disp.d2m(3), 1);
        assert_eq!(disp.d2m(7), 1 + ATOM_LEN);
        assert_eq!(disp.d2m(9), 1 + ATOM_LEN);
        assert_eq!(disp.d2m(10), 1 + ATOM_LEN + 1);
        assert_eq!(disp.m2d(block.len()), 11);
    }
}


// Document leases protect every referenced image, including below the viewport.
// Detached screen state may still live in GPUI's previous frame, so destroy()
// removes its weak registry entry immediately rather than waiting for its Arc.
const EDITOR_IMAGE_BYTES: usize = 16 * 1024 * 1024;
fn editor_image_cost(src: &str, entry: &ImageEntry) -> usize {
    src.len().saturating_add(match entry {
        ImageEntry::Ready(image) => (0..image.frame_count()).map(|frame| image.as_bytes(frame).map_or(0, <[u8]>::len)).sum::<usize>().saturating_mul(2),
        ImageEntry::Failed => 0,
    })
}
fn editor_image_sources(cx: &App) -> std::collections::HashSet<String> {
    let editors: Vec<_> = EDITORS.with(|editors| editors.borrow().values().filter_map(|editor| editor.upgrade()).collect());
    let mut sources = std::collections::HashSet::new();
    for editor in editors {
        let state = editor.read(cx);
        let mut cached = state.image_sources.borrow_mut();
        if cached.0 != state.core.doc.rev() {
            cached.0 = state.core.doc.rev();
            cached.1.clear();
            let mut seen = std::collections::HashSet::new();
            let history = state.core.history.undo.iter().chain(&state.core.history.redo).map(|snapshot| &snapshot.blocks);
            let rule = state.core.rule_undo.as_ref().map(|(snapshot, _)| &snapshot.blocks);
            for blocks in std::iter::once(&state.core.doc.blocks).chain(history).chain(rule) {
                for block in blocks {
                    if !seen.insert(std::rc::Rc::as_ptr(block)) { continue; }
                    if let Kind::Image { src, .. } = &block.kind { cached.1.insert(src.clone()); }
                    for run in &block.runs {
                        if let Some(atom) = &run.atom { if let Atom::Image { src, .. } = atom.as_ref() { cached.1.insert(src.clone()); } }
                    }
                }
            }
        }
        sources.extend(cached.1.iter().cloned());
    }
    sources
}
// A failed placeholder must not suppress a later JavaScript retry.
pub(crate) fn has_image(src: &str) -> bool {
    IMAGES.with(|cache| matches!(cache.borrow().0.get(src), Some(ImageEntry::Ready(_))))
}
pub(crate) fn image_memory_stats() -> serde_json::Value {
    IMAGES.with(|cache| {
        let cache = cache.borrow();
        serde_json::json!({"entries":cache.0.len(),"cpuAndGpuPixelBytesEstimate":cache.0.iter().map(|(src,entry)| editor_image_cost(src,entry)).sum::<usize>()})
    })
}
pub(crate) fn trim_images(window: &mut Window, cx: &mut App) {
    if std::env::var("KYBERN_EDITOR_IMAGE_MEMORY").as_deref() == Ok("0") { return; }
    if IMAGES.with(|cache| cache.borrow().0.is_empty()) {
        drain_image_drops(window);
        return;
    }
    let protected = editor_image_sources(cx);
    IMAGES.with(|cache| {
        let (map, order) = &mut *cache.borrow_mut();
        // Reopening uses hasRichEditorImage to reload a reclaimed decode.
        let mut bytes: usize = map.iter().filter(|(src,_)| !protected.contains(*src)).map(|(src,entry)| editor_image_cost(src,entry)).sum();
        for src in order.clone() {
            if protected.contains(&src) { continue; }
            if !protected.is_empty() && bytes <= EDITOR_IMAGE_BYTES && map.len() <= IMAGE_LIMIT { break; }
            order.retain(|value| value != &src);
            if let Some(entry) = map.remove(&src) {
                bytes = bytes.saturating_sub(editor_image_cost(&src, &entry));
                if let ImageEntry::Ready(image) = entry { IMAGE_DROPS.with(|drops| drops.borrow_mut().push(image)); }
            }
        }
    });
    drain_image_drops(window);
}
