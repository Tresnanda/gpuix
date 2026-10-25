//! The document model behind `<rich-editor>` (std only, so `rustc --test` runs it).
//!
//! A note is a flat list of blocks. Nesting is attributes: `depth` for list
//! levels, `quote` for enclosing blockquotes, `cont` for a list item's later
//! paragraphs. Inline content is a string plus runs that cover it exactly; an
//! atom (task reference, inline image, inline HTML) is one U+FFFC character with
//! its own run. Rules for Enter, Backspace, Delete, Tab and the input rules
//! follow TipTap 3 / ProseMirror as the desktop editor configures them
//! (`apps/desktop/src/views/notes/editorExtensions.ts`).
//!
//! Offsets are UTF-8 bytes inside a block. The element converts to UTF-16 for
//! JavaScript and for the platform text input client.
#![allow(dead_code)]

use std::collections::{HashMap, VecDeque};
use std::rc::Rc;

pub const ATOM: char = '\u{FFFC}';
pub const ATOM_STR: &str = "\u{FFFC}";
pub const ATOM_LEN: usize = 3;

pub const BOLD: u8 = 1;
pub const ITALIC: u8 = 2;
pub const STRIKE: u8 = 4;
pub const CODE: u8 = 8;

/// ProseMirror's history `newGroupDelay` (TipTap's default).
pub const GROUP_MS: u64 = 500;
/// The desktop's history depth is 100; the GPUIX buffer has kept 200.
pub const HISTORY_LIMIT: usize = 200;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Link {
    pub href: String,
    pub title: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Atom {
    /// `[ADE-14](kybern://task/<id>)`: the label is what the link was written with.
    TaskRef { id: String, label: String },
    /// An image inside a line of text (a line holding only an image is a block).
    Image { src: String, alt: String, title: Option<String> },
    /// Inline HTML or anything else kept exactly; shown as its source.
    Raw { source: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Run {
    pub len: usize,
    pub marks: u8,
    pub link: Option<Rc<Link>>,
    pub atom: Option<Rc<Atom>>,
}

impl Run {
    pub fn text(len: usize, marks: u8, link: Option<Rc<Link>>) -> Self {
        Run { len, marks, link, atom: None }
    }
    pub fn atom(atom: Atom, marks: u8, link: Option<Rc<Link>>) -> Self {
        Run { len: ATOM_LEN, marks, link, atom: Some(Rc::new(atom)) }
    }
    fn same_style(&self, other: &Run) -> bool {
        self.atom.is_none() && other.atom.is_none() && self.marks == other.marks && self.link == other.link
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Kind {
    Paragraph,
    Heading(u8),
    Bullet,
    /// The list's start number; only the first item of a list uses it.
    Ordered(u32),
    Task(bool),
    Code(String),
    Rule,
    Image { src: String, alt: String, title: Option<String> },
    /// Source the editor does not model (tables, HTML, definitions), kept and edited literally.
    Raw,
}

impl Kind {
    pub fn is_list(&self) -> bool {
        matches!(self, Kind::Bullet | Kind::Ordered(_) | Kind::Task(_))
    }
    /// Rules and images: one atom character, no text.
    pub fn is_atom(&self) -> bool {
        matches!(self, Kind::Rule | Kind::Image { .. })
    }
    /// Code and raw source: literal text, no marks, no input rules.
    pub fn is_literal(&self) -> bool {
        matches!(self, Kind::Code(_) | Kind::Raw)
    }
    pub fn id(&self) -> &'static str {
        match self {
            Kind::Paragraph => "text",
            Kind::Heading(1) => "h1",
            Kind::Heading(2) => "h2",
            Kind::Heading(_) => "h3",
            Kind::Bullet => "bullet",
            Kind::Ordered(_) => "number",
            Kind::Task(_) => "checklist",
            Kind::Code(_) => "code",
            Kind::Rule => "divider",
            Kind::Image { .. } => "image",
            Kind::Raw => "raw",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Block {
    pub id: u64,
    pub kind: Kind,
    pub depth: u8,
    pub quote: u8,
    pub cont: bool,
    pub text: String,
    pub runs: Vec<Run>,
    pub rev: u64,
}

impl Block {
    pub fn new(id: u64, kind: Kind, text: &str) -> Self {
        let text = if kind.is_atom() { ATOM_STR.to_string() } else { text.to_string() };
        let runs = if text.is_empty() { Vec::new() } else { vec![Run::text(text.len(), 0, None)] };
        Block { id, kind, depth: 0, quote: 0, cont: false, text, runs, rev: 0 }
    }

    pub fn len(&self) -> usize {
        self.text.len()
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// Index of the run containing `offset` (the run that starts at it when on a boundary), and its start.
    pub fn run_at(&self, offset: usize) -> Option<(usize, usize)> {
        let mut start = 0;
        for (index, run) in self.runs.iter().enumerate() {
            if offset < start + run.len {
                return Some((index, start));
            }
            start += run.len;
        }
        None
    }

    /// Run boundaries split at `offset`; returns the index of the first run at or after it.
    fn split_runs_at(&mut self, offset: usize) -> usize {
        let mut start = 0;
        for index in 0..self.runs.len() {
            let len = self.runs[index].len;
            if offset == start {
                return index;
            }
            if offset < start + len {
                let mut tail = self.runs[index].clone();
                tail.len = start + len - offset;
                self.runs[index].len = offset - start;
                self.runs.insert(index + 1, tail);
                return index + 1;
            }
            start += len;
        }
        self.runs.len()
    }

    pub fn normalize(&mut self) {
        self.runs.retain(|run| run.len > 0);
        let mut index = 1;
        while index < self.runs.len() {
            if self.runs[index - 1].same_style(&self.runs[index]) {
                self.runs[index - 1].len += self.runs[index].len;
                self.runs.remove(index);
            } else {
                index += 1;
            }
        }
        debug_assert_eq!(self.runs.iter().map(|r| r.len).sum::<usize>(), self.text.len());
    }

    /// Insert plain text with the given style at a character boundary.
    pub fn insert(&mut self, offset: usize, text: &str, marks: u8, link: Option<Rc<Link>>) {
        if text.is_empty() {
            return;
        }
        self.text.insert_str(offset, text);
        let index = self.split_runs_at(offset);
        self.runs.insert(index, Run::text(text.len(), marks, link));
        self.normalize();
    }

    pub fn insert_atom(&mut self, offset: usize, atom: Atom) {
        self.text.insert_str(offset, ATOM_STR);
        let index = self.split_runs_at(offset);
        self.runs.insert(index, Run::atom(atom, 0, None));
        self.normalize();
    }

    pub fn remove(&mut self, start: usize, end: usize) {
        if start >= end {
            return;
        }
        self.text.replace_range(start..end, "");
        let first = self.split_runs_at(start);
        let mut removed = 0;
        let mut index = first;
        while index < self.runs.len() && removed < end - start {
            let run = &mut self.runs[index];
            let take = run.len.min(end - start - removed);
            run.len -= take;
            removed += take;
            index += 1;
        }
        self.normalize();
    }

    /// The text and runs of `start..end`.
    pub fn slice(&self, start: usize, end: usize) -> (String, Vec<Run>) {
        let mut runs = Vec::new();
        let mut at = 0;
        for run in &self.runs {
            let (a, b) = (at.max(start), (at + run.len).min(end));
            if a < b {
                let mut piece = run.clone();
                piece.len = b - a;
                runs.push(piece);
            }
            at += run.len;
        }
        (self.text[start..end].to_string(), runs)
    }

    pub fn append(&mut self, text: &str, runs: &[Run]) {
        self.text.push_str(text);
        self.runs.extend_from_slice(runs);
        self.normalize();
    }

    /// The marks typing at `offset` continues: those of the character before, else after.
    pub fn style_at(&self, offset: usize) -> (u8, Option<Rc<Link>>) {
        let probe = if offset > 0 { offset - 1 } else { 0 };
        match self.run_at(probe) {
            Some((index, _)) if self.runs[index].atom.is_none() => (self.runs[index].marks, self.runs[index].link.clone()),
            Some((index, _)) => {
                // An atom carries no typing style; look at its other side.
                let next = if offset > 0 { self.run_at(offset) } else { None };
                match next {
                    Some((n, _)) if self.runs[n].atom.is_none() && index != n => (self.runs[n].marks, self.runs[n].link.clone()),
                    _ => (0, None),
                }
            }
            None => (0, None),
        }
    }

    pub fn atom_at(&self, offset: usize) -> Option<Rc<Atom>> {
        let (index, start) = self.run_at(offset)?;
        (start == offset).then(|| self.runs[index].atom.clone()).flatten()
    }

    /// Whether every non-atom character in `start..end` has `mark`.
    pub fn has_mark(&self, start: usize, end: usize, mark: u8) -> Option<bool> {
        let mut at = 0;
        let mut any = false;
        for run in &self.runs {
            let (a, b) = (at.max(start), (at + run.len).min(end));
            if a < b && run.atom.is_none() {
                any = true;
                if run.marks & mark == 0 {
                    return Some(false);
                }
            }
            at += run.len;
        }
        any.then_some(true)
    }

    pub fn set_mark(&mut self, start: usize, end: usize, mark: u8, on: bool) {
        if start >= end || self.kind.is_literal() {
            return;
        }
        let first = self.split_runs_at(start);
        let last = self.split_runs_at(end);
        for run in &mut self.runs[first..last] {
            if run.atom.is_some() {
                continue;
            }
            if on {
                if mark == CODE {
                    // Code excludes every other mark, the link included (`excludes: "_"`).
                    run.marks = CODE;
                    run.link = None;
                } else if run.marks & CODE == 0 {
                    run.marks |= mark;
                }
            } else {
                run.marks &= !mark;
            }
        }
        self.normalize();
    }

    pub fn set_link(&mut self, start: usize, end: usize, link: Option<Rc<Link>>) {
        if start >= end || self.kind.is_literal() {
            return;
        }
        let first = self.split_runs_at(start);
        let last = self.split_runs_at(end);
        for run in &mut self.runs[first..last] {
            if run.atom.is_none() && run.marks & CODE == 0 {
                run.link = link.clone();
            }
        }
        self.normalize();
    }

    /// The range of the link run(s) around `offset` with the same target.
    pub fn link_range(&self, offset: usize) -> Option<(usize, usize, Rc<Link>)> {
        let probe = |o: usize| self.run_at(o).and_then(|(i, s)| self.runs[i].link.clone().map(|l| (i, s, l)));
        let (index, _, link) = probe(offset).or_else(|| offset.checked_sub(1).and_then(probe))?;
        let mut first = index;
        while first > 0 && self.runs[first - 1].link.as_ref() == Some(&link) {
            first -= 1;
        }
        let mut last = index;
        while last + 1 < self.runs.len() && self.runs[last + 1].link.as_ref() == Some(&link) {
            last += 1;
        }
        let start: usize = self.runs[..first].iter().map(|r| r.len).sum();
        let end: usize = start + self.runs[first..=last].iter().map(|r| r.len).sum::<usize>();
        Some((start, end, link))
    }

    fn reset_text(&mut self, text: String) {
        self.runs = if text.is_empty() { Vec::new() } else { vec![Run::text(text.len(), 0, None)] };
        self.text = text;
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Default)]
pub struct Pos {
    pub block: usize,
    pub offset: usize,
}

impl Pos {
    pub fn new(block: usize, offset: usize) -> Self {
        Pos { block, offset }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Sel {
    pub anchor: Pos,
    pub head: Pos,
}

impl Sel {
    pub fn caret(pos: Pos) -> Self {
        Sel { anchor: pos, head: pos }
    }
    pub fn start(&self) -> Pos {
        self.anchor.min(self.head)
    }
    pub fn end(&self) -> Pos {
        self.anchor.max(self.head)
    }
    pub fn is_empty(&self) -> bool {
        self.anchor == self.head
    }
    pub fn reversed(&self) -> bool {
        self.head < self.anchor
    }
}

#[derive(Clone)]
pub struct Doc {
    pub blocks: Vec<Rc<Block>>,
    next_id: u64,
    next_rev: u64,
}

impl Doc {
    pub fn new(mut blocks: Vec<Block>) -> Self {
        if blocks.is_empty() {
            blocks.push(Block::new(1, Kind::Paragraph, ""));
        }
        let next_id = blocks.iter().map(|b| b.id).max().unwrap_or(0).max(1 << 32) + 1;
        let mut doc = Doc { blocks: Vec::new(), next_id, next_rev: 1 };
        for mut block in blocks {
            block.normalize();
            block.rev = doc.bump();
            doc.blocks.push(Rc::new(block));
        }
        doc
    }

    fn bump(&mut self) -> u64 {
        self.next_rev += 1;
        self.next_rev
    }

    pub fn rev(&self) -> u64 {
        self.next_rev
    }

    pub fn fresh_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    pub fn len(&self) -> usize {
        self.blocks.len()
    }

    pub fn block(&self, index: usize) -> &Block {
        &self.blocks[index]
    }

    /// A block about to change: copied when history shares it, and given a new revision.
    pub fn edit(&mut self, index: usize) -> &mut Block {
        let rev = self.bump();
        let block = Rc::make_mut(&mut self.blocks[index]);
        block.rev = rev;
        block
    }

    pub fn insert_block(&mut self, index: usize, mut block: Block) {
        block.rev = self.bump();
        block.normalize();
        self.blocks.insert(index, Rc::new(block));
    }

    pub fn remove_block(&mut self, index: usize) -> Rc<Block> {
        self.next_rev += 1;
        let removed = self.blocks.remove(index);
        if self.blocks.is_empty() {
            let id = self.fresh_id();
            self.insert_block(0, Block::new(id, Kind::Paragraph, ""));
        }
        removed
    }

    pub fn end(&self) -> Pos {
        let last = self.blocks.len() - 1;
        Pos::new(last, self.blocks[last].len())
    }

    pub fn clamp(&self, pos: Pos) -> Pos {
        let block = pos.block.min(self.blocks.len() - 1);
        let text = &self.blocks[block].text;
        let mut offset = pos.offset.min(text.len());
        while !text.is_char_boundary(offset) {
            offset -= 1;
        }
        Pos::new(block, offset)
    }

    /// A fresh empty block that continues `index`'s list or quote, for Enter.
    fn sibling(&mut self, index: usize, kind: Kind) -> Block {
        let id = self.fresh_id();
        let source = &self.blocks[index];
        let mut block = Block::new(id, kind, "");
        block.depth = if block.kind.is_list() || source.cont { source.depth } else { 0 };
        block.quote = source.quote;
        block
    }

    // ---- editing primitives ----

    /// Delete `start..end` (ordered). Returns the caret.
    pub fn delete(&mut self, start: Pos, end: Pos) -> Pos {
        if start >= end {
            return start;
        }
        if start.block == end.block {
            let block = &self.blocks[start.block];
            if block.kind.is_atom() {
                // The whole rule or image.
                if start.offset == 0 && end.offset >= ATOM_LEN {
                    return self.remove_atom_block(start.block);
                }
                return start;
            }
            self.edit(start.block).remove(start.offset, end.offset);
            return start;
        }
        let first_atom = self.blocks[start.block].kind.is_atom();
        let last_atom = self.blocks[end.block].kind.is_atom();
        // Blocks strictly between go.
        for _ in start.block + 1..end.block {
            self.blocks.remove(start.block + 1);
            self.next_rev += 1;
        }
        let last = start.block + 1;
        match (first_atom, last_atom) {
            (true, _) if start.offset == 0 => {
                self.blocks.remove(start.block);
                self.next_rev += 1;
                let pos = Pos::new(start.block, 0);
                self.trim_front(pos.block, end.offset, last_atom);
                self.clamp(pos)
            }
            (true, _) => {
                self.trim_front(last, end.offset, last_atom);
                start
            }
            (false, true) => {
                let len = self.blocks[start.block].len();
                self.edit(start.block).remove(start.offset, len);
                if end.offset >= ATOM_LEN {
                    self.blocks.remove(last);
                    self.next_rev += 1;
                }
                start
            }
            (false, false) => {
                let tail = {
                    let block = &self.blocks[last];
                    block.slice(end.offset, block.len())
                };
                self.blocks.remove(last);
                self.next_rev += 1;
                let block = self.edit(start.block);
                let len = block.len();
                block.remove(start.offset, len);
                block.append(&tail.0, &tail.1);
                start
            }
        }
    }

    fn trim_front(&mut self, index: usize, offset: usize, atom: bool) {
        if index >= self.blocks.len() {
            return;
        }
        if atom {
            if offset >= ATOM_LEN {
                self.remove_block(index);
            }
        } else if offset > 0 {
            self.edit(index).remove(0, offset);
        }
    }

    fn remove_atom_block(&mut self, index: usize) -> Pos {
        self.remove_block(index);
        if index < self.blocks.len() {
            Pos::new(index, 0)
        } else {
            self.end()
        }
    }

    /// Enter: split the block at `pos` following ProseMirror's splitBlock/splitListItem.
    pub fn split(&mut self, pos: Pos) -> Pos {
        let block = self.blocks[pos.block].clone();
        if block.kind.is_atom() {
            let at = if pos.offset == 0 { pos.block } else { pos.block + 1 };
            let paragraph = self.sibling(pos.block, Kind::Paragraph);
            self.insert_block(at, paragraph);
            return if pos.offset == 0 { Pos::new(pos.block + 1, 0) } else { Pos::new(at, 0) };
        }
        if block.kind.is_list() && block.is_empty() && !block.cont {
            self.lift(pos.block);
            return Pos::new(pos.block, 0);
        }
        if block.is_empty() && block.quote > 0 && !block.kind.is_list() {
            // liftEmptyBlock: an empty paragraph leaves the quote.
            self.edit(pos.block).quote -= 1;
            return pos;
        }
        let at_end = pos.offset == block.len();
        let kind = match &block.kind {
            Kind::Heading(_) if at_end => Kind::Paragraph,
            Kind::Task(_) => Kind::Task(false),
            Kind::Ordered(_) => Kind::Ordered(1),
            other => other.clone(),
        };
        let (text, runs) = block.slice(pos.offset, block.len());
        let mut next = self.sibling(pos.block, kind);
        next.cont = block.cont && !next.kind.is_list();
        next.text = text;
        next.runs = runs;
        self.edit(pos.block).remove(pos.offset, block.len());
        self.insert_block(pos.block + 1, next);
        Pos::new(pos.block + 1, 0)
    }

    /// Out of the list one level (liftListItem), or out of it altogether.
    pub fn lift(&mut self, index: usize) {
        let block = self.edit(index);
        if block.depth > 0 {
            block.depth -= 1;
        } else {
            block.kind = Kind::Paragraph;
            block.cont = false;
        }
    }

    /// Join block `index` into the one before it (its kind is kept). Returns the caret.
    pub fn join_backward(&mut self, index: usize) -> Pos {
        let tail = self.blocks[index].slice(0, self.blocks[index].len());
        let prev = index - 1;
        let at = self.blocks[prev].len();
        self.edit(prev).append(&tail.0, &tail.1);
        self.blocks.remove(index);
        self.next_rev += 1;
        Pos::new(prev, at)
    }

    pub fn set_kind(&mut self, index: usize, kind: Kind) {
        let block = self.edit(index);
        if block.kind.is_atom() {
            return;
        }
        let was_literal = block.kind.is_literal();
        let now_literal = kind.is_literal();
        if !kind.is_list() {
            block.depth = if block.cont { block.depth } else { 0 };
        }
        if kind.is_list() {
            block.cont = false;
        }
        block.kind = kind;
        if now_literal && !was_literal {
            // Code keeps the words, not their formatting or references.
            let text: String = block
                .runs
                .iter()
                .scan(0usize, |at, run| {
                    let start = *at;
                    *at += run.len;
                    Some((start, run.clone()))
                })
                .map(|(start, run)| match run.atom.as_deref() {
                    Some(Atom::TaskRef { label, .. }) => label.clone(),
                    Some(Atom::Image { alt, .. }) => alt.clone(),
                    Some(Atom::Raw { source }) => source.clone(),
                    None => block.text[start..start + run.len].to_string(),
                })
                .collect();
            block.reset_text(text);
        }
    }

    // ---- flattened positions: blocks joined by "\n" ----

    pub fn flat_utf16(&self, pos: Pos) -> usize {
        let mut total = 0;
        for block in &self.blocks[..pos.block] {
            total += utf16_len(&block.text) + 1;
        }
        total + utf16_len(&self.blocks[pos.block].text[..pos.offset])
    }

    pub fn pos_from_flat_utf16(&self, mut units: usize) -> Pos {
        for (index, block) in self.blocks.iter().enumerate() {
            let len = utf16_len(&block.text);
            if units <= len {
                return Pos::new(index, byte_from_utf16(&block.text, units));
            }
            units -= len + 1;
        }
        self.end()
    }

    pub fn flat_len_utf16(&self) -> usize {
        self.blocks.iter().map(|b| utf16_len(&b.text) + 1).sum::<usize>() - 1
    }

    /// Text between positions: blocks joined by `separator`, atoms as their visible label.
    pub fn text_between(&self, start: Pos, end: Pos, separator: &str, atoms_as_label: bool) -> String {
        let mut out = String::new();
        for index in start.block..=end.block.min(self.blocks.len() - 1) {
            let block = &self.blocks[index];
            let a = if index == start.block { start.offset } else { 0 };
            let b = if index == end.block { end.offset } else { block.len() };
            if index > start.block {
                out.push_str(separator);
            }
            if !atoms_as_label {
                out.push_str(&block.text[a..b]);
                continue;
            }
            if let Kind::Image { alt, .. } = &block.kind {
                if a < b {
                    out.push_str(alt);
                }
                continue;
            }
            if block.kind == Kind::Rule {
                continue;
            }
            let mut at = 0;
            for run in &block.runs {
                let (x, y) = (at.max(a), (at + run.len).min(b));
                if x < y {
                    match run.atom.as_deref() {
                        Some(Atom::TaskRef { label, .. }) => out.push_str(label),
                        Some(Atom::Image { alt, .. }) => out.push_str(alt),
                        Some(Atom::Raw { source }) => out.push_str(source),
                        None => out.push_str(&block.text[x..y]),
                    }
                }
                at += run.len;
            }
        }
        out
    }

    /// The number an ordered list item shows (its list's start, counted on).
    pub fn ordinal(&self, index: usize) -> u32 {
        let block = &self.blocks[index];
        let mut count = 0;
        let mut at = index;
        loop {
            let b = &self.blocks[at];
            if b.depth == block.depth && b.quote == block.quote && !b.cont {
                match b.kind {
                    Kind::Ordered(start) => {
                        count += 1;
                        let first = at == 0 || !self.continues_list(at - 1, block);
                        if first {
                            return start + count - 1;
                        }
                    }
                    _ => return count.max(1),
                }
            }
            if at == 0 || !self.continues_list(at - 1, block) {
                return count.max(1);
            }
            at -= 1;
        }
    }

    /// Whether block `at` still belongs to the list that `item` is in, looking upward.
    fn continues_list(&self, at: usize, item: &Block) -> bool {
        let b = &self.blocks[at];
        if b.quote != item.quote {
            return false;
        }
        if b.depth > item.depth && (b.kind.is_list() || b.cont) {
            return true;
        }
        if b.cont && b.depth == item.depth {
            return true;
        }
        b.depth == item.depth && matches!(b.kind, Kind::Ordered(_))
    }
}

pub fn utf16_len(text: &str) -> usize {
    text.chars().map(char::len_utf16).sum()
}

pub fn byte_from_utf16(text: &str, units: usize) -> usize {
    let mut count = 0;
    for (byte, ch) in text.char_indices() {
        if count >= units {
            return byte;
        }
        count += ch.len_utf16();
    }
    text.len()
}

pub fn utf16_from_byte(text: &str, byte: usize) -> usize {
    utf16_len(&text[..byte.min(text.len())])
}

// ---- history ----

#[derive(Clone)]
pub struct Snapshot {
    pub blocks: Vec<Rc<Block>>,
    pub sel: Sel,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Group {
    Insert,
    DeleteBackward,
    DeleteForward,
}

#[derive(Default)]
pub struct History {
    pub undo: VecDeque<Snapshot>,
    pub redo: Vec<Snapshot>,
    last: Option<(Group, u64, Pos)>,
}

impl History {
    /// Record the state before an edit. Typing of one kind at the caret within
    /// `GROUP_MS` of the last keystroke joins its step.
    pub fn record(&mut self, before: Snapshot, group: Option<Group>, now: u64, at: Pos, after: Pos) {
        let joins = match (group, self.last) {
            (Some(g), Some((last, when, pos))) => g == last && now.saturating_sub(when) < GROUP_MS && pos == at,
            _ => false,
        };
        if !joins {
            if self.undo.len() == HISTORY_LIMIT {
                self.undo.pop_front();
            }
            self.undo.push_back(before);
        }
        self.redo.clear();
        self.last = group.map(|g| (g, now, after));
    }

    /// The next edit starts its own step (after formatting, paste, a caret move).
    pub fn seal(&mut self) {
        self.last = None;
    }
}

// ---- the editing session ----

/// What typing continues when nothing is selected: toggled marks wait here for the next character.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stored {
    pub marks: u8,
    pub link: Option<Rc<Link>>,
}

pub struct Core {
    pub doc: Doc,
    pub sel: Sel,
    pub history: History,
    pub stored: Option<Stored>,
    /// The state as typed, just before an input rule rewrote it, and the caret the rule left.
    pub rule_undo: Option<(Snapshot, Pos)>,
    /// Composition (IME) range, while the input method has marked text.
    pub marked: Option<(Pos, Pos)>,
    /// Task keys that name a task: `ADE-14` → id. Typed and pasted keys link to these.
    pub task_keys: HashMap<String, String>,
    /// Inside `step`: nested steps join the outer one.
    in_step: bool,
}

impl Core {
    pub fn new(doc: Doc) -> Self {
        Core {
            sel: Sel::caret(Pos::new(0, 0)),
            doc,
            history: History::default(),
            stored: None,
            rule_undo: None,
            marked: None,
            task_keys: HashMap::new(),
            in_step: false,
        }
    }

    pub fn snapshot(&self) -> Snapshot {
        Snapshot { blocks: self.doc.blocks.clone(), sel: self.sel }
    }

    fn restore(&mut self, snapshot: Snapshot) {
        self.doc.next_rev += 1;
        // Revisions must still grow, so the element relays out what came back.
        self.doc.blocks = snapshot.blocks;
        for index in 0..self.doc.blocks.len() {
            let rev = self.doc.bump();
            Rc::make_mut(&mut self.doc.blocks[index]).rev = rev;
        }
        self.sel = Sel { anchor: self.doc.clamp(snapshot.sel.anchor), head: self.doc.clamp(snapshot.sel.head) };
        self.stored = None;
        self.marked = None;
        self.rule_undo = None;
    }

    pub fn undo(&mut self) -> bool {
        let Some(previous) = self.history.undo.pop_back() else { return false };
        self.history.redo.push(self.snapshot());
        self.restore(previous);
        self.history.seal();
        true
    }

    pub fn redo(&mut self) -> bool {
        let Some(next) = self.history.redo.pop() else { return false };
        let current = self.snapshot();
        if self.history.undo.len() == HISTORY_LIMIT {
            self.history.undo.pop_front();
        }
        self.history.undo.push_back(current);
        self.restore(next);
        self.history.seal();
        true
    }

    /// Run an edit as one undo step of its own.
    pub fn step<R>(&mut self, edit: impl FnOnce(&mut Core) -> R) -> R {
        if self.in_step {
            return edit(self);
        }
        self.in_step = true;
        let before = self.snapshot();
        let rev = self.doc.rev();
        let result = edit(self);
        self.in_step = false;
        if self.doc.rev() != rev {
            self.history.record(before, None, 0, Pos::default(), Pos::default());
            self.history.seal();
        }
        self.rule_undo = None;
        result
    }

    pub fn set_selection(&mut self, sel: Sel) {
        let sel = Sel { anchor: self.doc.clamp(sel.anchor), head: self.doc.clamp(sel.head) };
        if sel != self.sel {
            self.sel = sel;
            self.stored = None;
            self.history.seal();
            self.rule_undo = None;
        }
    }

    /// The marks the next typed character takes.
    pub fn typing_style(&self) -> (u8, Option<Rc<Link>>) {
        if let Some(stored) = &self.stored {
            return (stored.marks, stored.link.clone());
        }
        let pos = self.sel.start();
        let block = self.doc.block(pos.block);
        if block.kind.is_literal() {
            return (0, None);
        }
        block.style_at(pos.offset)
    }

    /// Type `text` over the selection (or the composition), with input rules when `rules`.
    pub fn insert_text(&mut self, text: &str, now: u64, rules: bool) {
        let before = self.snapshot();
        let (start, end) = match self.marked {
            Some((a, b)) => (a, b),
            None => (self.sel.start(), self.sel.end()),
        };
        let composing = self.marked.is_some();
        let group = (start == end && text.chars().count() == 1 && !composing).then_some(Group::Insert);
        let (marks, link) = self.typing_style();
        // Stored marks style one insertion; an input rule below may store new ones.
        self.stored = None;
        let mut at = self.doc.delete(start, end);
        if self.doc.block(at.block).kind.is_atom() {
            // Typing on a selected rule or image: a paragraph takes the text.
            at = self.doc.split(at);
        }
        let literal = self.doc.block(at.block).kind.is_literal();
        let mut first = true;
        for line in text.split('\n') {
            if !first {
                at = if literal {
                    self.doc.edit(at.block).insert(at.offset, "\n", 0, None);
                    Pos::new(at.block, at.offset + 1)
                } else {
                    self.doc.split(at)
                };
            }
            first = false;
            if line.is_empty() {
                continue;
            }
            if literal {
                self.doc.edit(at.block).insert(at.offset, line, 0, None);
            } else {
                self.doc.edit(at.block).insert(at.offset, line, marks, link.clone());
            }
            at.offset += line.len();
        }
        self.sel = Sel::caret(at);
        self.marked = None;
        if composing {
            // Committing a composition finishes the step it opened.
            self.history.seal();
        } else {
            self.history.record(before.clone(), group, now, start, at);
        }
        if rules && group.is_some() && !literal {
            self.rule_undo = None;
            if self.apply_input_rules(text, before_typed(&before, self)) {
                self.history.seal();
            }
        } else {
            self.rule_undo = None;
        }
    }

    /// Replace the composition with marked text; the composition is one history step.
    pub fn set_marked(&mut self, text: &str, now: u64) {
        let before = self.snapshot();
        let (start, end) = self.marked.unwrap_or((self.sel.start(), self.sel.end()));
        let starting = self.marked.is_none();
        let (marks, link) = self.typing_style();
        let at = self.doc.delete(start, end);
        let literal = self.doc.block(at.block).kind.is_literal();
        let single: String = text.replace('\n', " ");
        if literal {
            self.doc.edit(at.block).insert(at.offset, &single, 0, None);
        } else {
            self.doc.edit(at.block).insert(at.offset, &single, marks, link);
        }
        let end = Pos::new(at.block, at.offset + single.len());
        if starting {
            self.history.record(before, None, now, start, end);
            self.history.seal();
        }
        self.marked = (!single.is_empty()).then_some((at, end));
        self.sel = Sel::caret(end);
        self.rule_undo = None;
    }

    pub fn unmark(&mut self) {
        self.marked = None;
    }

    /// Delete the selection, or the character before/after the caret.
    pub fn delete_selection(&mut self, now: u64, group: Option<Group>) -> bool {
        if self.sel.is_empty() {
            return false;
        }
        let before = self.snapshot();
        let (start, end) = (self.sel.start(), self.sel.end());
        let at = self.doc.delete(start, end);
        self.sel = Sel::caret(at);
        self.history.record(before, group, now, end, at);
        true
    }

    pub fn backspace(&mut self, now: u64) -> BackspaceResult {
        if let Some((snapshot, pos)) = self.rule_undo.take() {
            if self.sel.is_empty() && self.sel.head == pos {
                // undoInputRule: put back what was typed.
                let before = self.snapshot();
                self.restore(snapshot);
                self.history.record(before, None, now, pos, self.sel.head);
                self.history.seal();
                return BackspaceResult::Edited;
            }
        }
        if !self.sel.is_empty() {
            self.delete_selection(now, None);
            self.history.seal();
            return BackspaceResult::Edited;
        }
        let pos = self.sel.head;
        let block = self.doc.block(pos.block).clone();
        if pos.offset > 0 {
            if block.kind.is_atom() {
                // After a rule or image: select it.
                self.sel = Sel { anchor: Pos::new(pos.block, ATOM_LEN), head: Pos::new(pos.block, 0) };
                return BackspaceResult::Moved;
            }
            let previous = previous_grapheme(&block.text, pos.offset);
            let before = self.snapshot();
            self.doc.edit(pos.block).remove(previous, pos.offset);
            self.sel = Sel::caret(Pos::new(pos.block, previous));
            self.history.record(before, Some(Group::DeleteBackward), now, pos, self.sel.head);
            return BackspaceResult::Edited;
        }
        // At the start of a block.
        let before = self.snapshot();
        let result = if block.kind.is_list() && !block.cont {
            self.doc.lift(pos.block);
            BackspaceResult::Edited
        } else if block.quote > 0 && (pos.block == 0 || self.doc.block(pos.block - 1).quote < block.quote) {
            self.doc.edit(pos.block).quote -= 1;
            BackspaceResult::Edited
        } else if pos.block == 0 {
            if !matches!(block.kind, Kind::Paragraph) && block.is_empty() {
                self.doc.set_kind(0, Kind::Paragraph);
                BackspaceResult::Edited
            } else if block.kind == Kind::Paragraph && block.is_empty() && block.quote == 0 {
                BackspaceResult::AtStart
            } else if let Kind::Heading(_) = block.kind {
                self.doc.set_kind(0, Kind::Paragraph);
                BackspaceResult::Edited
            } else {
                BackspaceResult::AtStart
            }
        } else {
            let prev = self.doc.block(pos.block - 1).clone();
            if prev.kind.is_atom() {
                self.sel = Sel { anchor: Pos::new(pos.block - 1, ATOM_LEN), head: Pos::new(pos.block - 1, 0) };
                return BackspaceResult::Moved;
            }
            if prev.is_empty() && prev.kind == Kind::Paragraph && !block.is_empty() {
                self.doc.remove_block(pos.block - 1);
                self.sel = Sel::caret(Pos::new(pos.block - 1, 0));
                self.history.record(before, None, now, pos, self.sel.head);
                self.history.seal();
                return BackspaceResult::Edited;
            }
            if block.kind.is_atom() {
                self.sel = Sel { anchor: Pos::new(pos.block, ATOM_LEN), head: Pos::new(pos.block, 0) };
                return BackspaceResult::Moved;
            }
            let caret = if prev.kind.is_literal() && !block.kind.is_literal() {
                // Into code: the words only.
                let label = self.doc.text_between(Pos::new(pos.block, 0), Pos::new(pos.block, block.len()), "", true);
                let at = prev.len();
                self.doc.edit(pos.block - 1).insert(at, &label, 0, None);
                self.doc.remove_block(pos.block);
                Pos::new(pos.block - 1, at)
            } else {
                self.doc.join_backward(pos.block)
            };
            self.sel = Sel::caret(caret);
            BackspaceResult::Edited
        };
        if result == BackspaceResult::Edited {
            self.history.record(before, None, now, pos, self.sel.head);
            self.history.seal();
            self.sel = Sel { anchor: self.doc.clamp(self.sel.anchor), head: self.doc.clamp(self.sel.head) };
        }
        result
    }

    pub fn delete_forward(&mut self, now: u64) -> bool {
        self.rule_undo = None;
        if !self.sel.is_empty() {
            self.delete_selection(now, None);
            self.history.seal();
            return true;
        }
        let pos = self.sel.head;
        let block = self.doc.block(pos.block).clone();
        if pos.offset < block.len() {
            if block.kind.is_atom() {
                self.sel = Sel { anchor: Pos::new(pos.block, 0), head: Pos::new(pos.block, ATOM_LEN) };
                return true;
            }
            let next = next_grapheme(&block.text, pos.offset);
            let before = self.snapshot();
            self.doc.edit(pos.block).remove(pos.offset, next);
            self.history.record(before, Some(Group::DeleteForward), now, pos, pos);
            return true;
        }
        if pos.block + 1 >= self.doc.len() {
            return false;
        }
        let next = self.doc.block(pos.block + 1).clone();
        if next.kind.is_atom() {
            self.sel = Sel { anchor: Pos::new(pos.block + 1, 0), head: Pos::new(pos.block + 1, ATOM_LEN) };
            return true;
        }
        let before = self.snapshot();
        if block.is_empty() && !block.kind.is_atom() {
            self.doc.remove_block(pos.block);
            self.sel = Sel::caret(Pos::new(pos.block, 0));
        } else if block.kind.is_atom() {
            return false;
        } else {
            self.sel = Sel::caret(self.doc.join_backward(pos.block + 1));
        }
        self.history.record(before, None, now, pos, self.sel.head);
        self.history.seal();
        true
    }

    /// Enter.
    pub fn enter(&mut self, now: u64) {
        let rule_before = self.snapshot();
        if !self.sel.is_empty() {
            let (start, end) = (self.sel.start(), self.sel.end());
            let at = self.doc.delete(start, end);
            self.sel = Sel::caret(at);
        }
        let pos = self.sel.head;
        let block = self.doc.block(pos.block).clone();
        // "```lang" then Enter makes a code block.
        if block.kind == Kind::Paragraph && pos.offset == block.len() {
            if let Some(lang) = fence_lang(&block.text) {
                let saved = self.snapshot();
                let b = self.doc.edit(pos.block);
                b.reset_text(String::new());
                b.kind = Kind::Code(lang);
                self.sel = Sel::caret(Pos::new(pos.block, 0));
                self.history.record(rule_before, None, now, pos, self.sel.head);
                self.history.seal();
                self.rule_undo = Some((saved, self.sel.head));
                return;
            }
        }
        self.autolink_before(pos);
        let pos = self.sel.head;
        let block = self.doc.block(pos.block).clone();
        let caret = if block.kind.is_literal() {
            if matches!(block.kind, Kind::Code(_)) && pos.offset == block.len() && block.text.ends_with("\n\n") {
                // exitOnTripleEnter.
                let len = block.len();
                self.doc.edit(pos.block).remove(len - 2, len);
                let paragraph = self.doc.sibling(pos.block, Kind::Paragraph);
                self.doc.insert_block(pos.block + 1, paragraph);
                Pos::new(pos.block + 1, 0)
            } else {
                self.doc.edit(pos.block).insert(pos.offset, "\n", 0, None);
                Pos::new(pos.block, pos.offset + 1)
            }
        } else {
            self.doc.split(pos)
        };
        self.sel = Sel::caret(caret);
        self.stored = None;
        self.history.record(rule_before, None, now, pos, caret);
        self.history.seal();
        self.rule_undo = None;
    }

    /// Shift-Enter: a line break inside the block.
    pub fn hard_break(&mut self, now: u64) {
        let before = self.snapshot();
        let (start, end) = (self.sel.start(), self.sel.end());
        let at = self.doc.delete(start, end);
        if self.doc.block(at.block).kind.is_atom() {
            return;
        }
        let (marks, link) = self.doc.block(at.block).style_at(at.offset);
        let literal = self.doc.block(at.block).kind.is_literal();
        self.doc.edit(at.block).insert(at.offset, "\n", if literal { 0 } else { marks }, if literal { None } else { link });
        self.sel = Sel::caret(Pos::new(at.block, at.offset + 1));
        self.history.record(before, None, now, start, self.sel.head);
        self.history.seal();
        self.rule_undo = None;
    }

    /// Tab / Shift-Tab in a list: sink (when there is an item to nest under) or lift.
    pub fn indent(&mut self, deeper: bool) -> bool {
        let (start, end) = (self.sel.start().block, self.sel.end().block);
        if !(start..=end).all(|i| self.doc.block(i).kind.is_list()) {
            return false;
        }
        self.step(|core| {
            for index in start..=end {
                let depth = core.doc.block(index).depth;
                if deeper {
                    let limit = (0..index).rev().map(|i| core.doc.block(i)).find(|b| b.kind.is_list() && !b.cont).map(|b| b.depth + 1);
                    if index > 0 && limit.is_some_and(|limit| depth < limit) && depth < 8 {
                        core.doc.edit(index).depth += 1;
                    }
                } else if depth > 0 {
                    core.doc.edit(index).depth -= 1;
                } else if index == start {
                    core.doc.lift(index);
                }
            }
        });
        true
    }

    // ---- formatting ----

    fn text_blocks_in_selection(&self) -> Vec<(usize, usize, usize)> {
        let (start, end) = (self.sel.start(), self.sel.end());
        (start.block..=end.block)
            .filter(|&i| !self.doc.block(i).kind.is_atom())
            .map(|i| {
                let len = self.doc.block(i).len();
                (i, if i == start.block { start.offset } else { 0 }, if i == end.block { end.offset } else { len })
            })
            .collect()
    }

    pub fn mark_active(&self, mark: u8) -> bool {
        if self.sel.is_empty() {
            return self.typing_style().0 & mark != 0;
        }
        let mut any = false;
        for (index, a, b) in self.text_blocks_in_selection() {
            match self.doc.block(index).has_mark(a, b, mark) {
                Some(false) => return false,
                Some(true) => any = true,
                None => {}
            }
        }
        any
    }

    pub fn toggle_mark(&mut self, mark: u8) {
        let on = !self.mark_active(mark);
        if self.sel.is_empty() {
            let (marks, link) = self.typing_style();
            let marks = if on { if mark == CODE { CODE } else if marks & CODE != 0 { marks } else { marks | mark } } else { marks & !mark };
            self.stored = Some(Stored { marks, link: if marks & CODE != 0 { None } else { link } });
            return;
        }
        self.step(|core| {
            for (index, a, b) in core.text_blocks_in_selection() {
                core.doc.edit(index).set_mark(a, b, mark, on);
            }
        });
    }

    pub fn active_link(&self) -> Option<(Pos, Pos, Rc<Link>)> {
        let pos = self.sel.head;
        let block = self.doc.block(pos.block);
        let (a, b, link) = block.link_range(pos.offset)?;
        if !self.sel.is_empty() && self.sel.start().block != self.sel.end().block {
            return None;
        }
        Some((Pos::new(pos.block, a), Pos::new(pos.block, b), link))
    }

    /// Apply or remove a link: on the selection, or on the link around the caret.
    pub fn set_link(&mut self, link: Option<Link>, text_if_empty: Option<&str>) {
        let link = link.map(Rc::new);
        self.step(|core| {
            if core.sel.is_empty() {
                if let Some((a, b, _)) = core.active_link() {
                    core.doc.edit(a.block).set_link(a.offset, b.offset, link);
                    return;
                }
                if let (Some(link), Some(text)) = (link, text_if_empty) {
                    let at = core.sel.head;
                    if core.doc.block(at.block).kind.is_literal() || core.doc.block(at.block).kind.is_atom() {
                        return;
                    }
                    core.doc.edit(at.block).insert(at.offset, text, 0, Some(link));
                    core.sel = Sel::caret(Pos::new(at.block, at.offset + text.len()));
                }
                return;
            }
            for (index, a, b) in core.text_blocks_in_selection() {
                core.doc.edit(index).set_link(a, b, link.clone());
            }
        });
    }

    /// The block id the "Turn into" menu shows for the selection.
    pub fn current_block(&self) -> &'static str {
        let block = self.doc.block(self.sel.head.block);
        block.kind.id()
    }

    /// Turn the selected blocks into `kind` (TipTap's toggle*: the same list again turns back into text).
    pub fn set_block(&mut self, kind: Kind, toggle: bool) {
        let (start, end) = (self.sel.start().block, self.sel.end().block);
        let same = (start..=end).all(|i| std::mem::discriminant(&self.doc.block(i).kind) == std::mem::discriminant(&kind));
        let target = if toggle && same { Kind::Paragraph } else { kind };
        self.step(|core| {
            for index in start..=end {
                if core.doc.block(index).kind.is_atom() {
                    continue;
                }
                let next = target.clone();
                core.doc.set_kind(index, next);
            }
            let sel = core.sel;
            core.sel = Sel { anchor: core.doc.clamp(sel.anchor), head: core.doc.clamp(sel.head) };
        });
    }

    pub fn toggle_quote(&mut self) {
        let (start, end) = (self.sel.start().block, self.sel.end().block);
        let all = (start..=end).all(|i| self.doc.block(i).quote > 0);
        self.step(|core| {
            for index in start..=end {
                let block = core.doc.edit(index);
                if all {
                    block.quote -= 1;
                } else {
                    block.quote = block.quote.max(1);
                }
            }
        });
    }

    pub fn toggle_task(&mut self, index: usize) {
        if let Kind::Task(checked) = self.doc.block(index).kind {
            self.step(|core| core.doc.edit(index).kind = Kind::Task(!checked));
        }
    }

    /// Insert a rule or image block after the caret's block (an empty paragraph is replaced).
    pub fn insert_block(&mut self, kind: Kind) {
        self.step(|core| {
            let at = core.doc.delete(core.sel.start(), core.sel.end());
            let current = core.doc.block(at.block).clone();
            let id = core.doc.fresh_id();
            let mut block = Block::new(id, kind, "");
            block.quote = current.quote;
            let index = if current.is_empty() && current.kind == Kind::Paragraph {
                core.doc.remove_block(at.block);
                at.block
            } else if at.offset == 0 && !current.kind.is_atom() {
                at.block
            } else if at.offset >= current.len() || current.kind.is_atom() {
                at.block + 1
            } else {
                core.doc.split(at).block
            };
            core.doc.insert_block(index.min(core.doc.len()), block);
            // A paragraph follows, so typing can go on.
            if index + 1 >= core.doc.len() || core.doc.block(index + 1).kind.is_atom() {
                let paragraph = core.doc.sibling(index, Kind::Paragraph);
                core.doc.insert_block(index + 1, paragraph);
            }
            core.sel = Sel::caret(Pos::new(index + 1, 0));
        });
    }

    pub fn insert_atom(&mut self, atom: Atom, trailing_space: bool) {
        self.step(|core| {
            let at = core.doc.delete(core.sel.start(), core.sel.end());
            let at = if core.doc.block(at.block).kind.is_atom() || core.doc.block(at.block).kind.is_literal() {
                core.doc.split(at)
            } else {
                at
            };
            core.doc.edit(at.block).insert_atom(at.offset, atom);
            let mut caret = Pos::new(at.block, at.offset + ATOM_LEN);
            if trailing_space {
                core.doc.edit(at.block).insert(caret.offset, " ", 0, None);
                caret.offset += 1;
            }
            core.sel = Sel::caret(caret);
        });
    }

    /// Delete `start..end` as part of a command (the "/" query), without its own step.
    pub fn delete_range(&mut self, start: Pos, end: Pos) {
        let at = self.doc.delete(start, end);
        self.sel = Sel::caret(at);
    }

    // ---- input rules ----

    /// After typing `typed` at the caret. Returns true when a rule rewrote the text.
    fn apply_input_rules(&mut self, typed: &str, as_typed: Snapshot) -> bool {
        let pos = self.sel.head;
        let block = self.doc.block(pos.block).clone();
        if block.kind.is_literal() || block.kind.is_atom() {
            return false;
        }
        let before = &block.text[..pos.offset];
        let fired = self.block_rule(pos, &block, before) || self.mark_rule(pos, &block, typed) || self.task_key_rule(pos, &block, typed) || self.autolink_rule(pos, &block, typed);
        if fired {
            self.rule_undo = Some((as_typed, self.sel.head));
        }
        fired
    }

    fn block_rule(&mut self, pos: Pos, block: &Block, before: &str) -> bool {
        let plain = block.kind == Kind::Paragraph && !block.cont;
        let set = |core: &mut Core, kind: Option<Kind>, quote: bool| {
            let b = core.doc.edit(pos.block);
            b.remove(0, pos.offset);
            if let Some(kind) = kind {
                b.kind = kind;
            }
            if quote {
                b.quote += 1;
            }
            core.sel = Sel::caret(Pos::new(pos.block, 0));
        };
        if before == "---" || before == "—-" || before == "___ " || before == "*** " {
            if !plain {
                return false;
            }
            let b = self.doc.edit(pos.block);
            b.remove(0, pos.offset);
            let rest_empty = b.is_empty();
            if rest_empty {
                let quote = b.quote;
                b.kind = Kind::Rule;
                b.text = ATOM_STR.to_string();
                b.runs = vec![Run::text(ATOM_LEN, 0, None)];
                let id = self.doc.fresh_id();
                let mut paragraph = Block::new(id, Kind::Paragraph, "");
                paragraph.quote = quote;
                self.doc.insert_block(pos.block + 1, paragraph);
                self.sel = Sel::caret(Pos::new(pos.block + 1, 0));
            } else {
                let id = self.doc.fresh_id();
                self.doc.insert_block(pos.block, Block::new(id, Kind::Rule, ""));
                self.sel = Sel::caret(Pos::new(pos.block + 1, 0));
            }
            return true;
        }
        if !before.ends_with(' ') && !before.ends_with('\u{a0}') {
            return false;
        }
        let head = &before[..before.len() - 1];
        if plain {
            match head {
                "#" | "##" | "###" => {
                    set(self, Some(Kind::Heading(head.len() as u8)), false);
                    return true;
                }
                "-" | "+" | "*" => {
                    set(self, Some(Kind::Bullet), false);
                    return true;
                }
                ">" => {
                    set(self, None, true);
                    return true;
                }
                "[ ]" | "[]" | "[x]" | "[X]" => {
                    set(self, Some(Kind::Task(head.contains(['x', 'X']))), false);
                    return true;
                }
                _ => {}
            }
            if let Some(number) = head.strip_suffix('.').filter(|n| !n.is_empty() && n.len() <= 9 && n.bytes().all(|b| b.is_ascii_digit())) {
                set(self, Some(Kind::Ordered(number.parse().unwrap_or(1))), false);
                return true;
            }
            if let Some(lang) = fence_lang(head) {
                set(self, Some(Kind::Code(lang)), false);
                return true;
            }
        }
        if block.kind == Kind::Bullet && matches!(head, "[ ]" | "[]" | "[x]" | "[X]") {
            set(self, Some(Kind::Task(head.contains(['x', 'X']))), false);
            return true;
        }
        false
    }

    fn mark_rule(&mut self, pos: Pos, block: &Block, typed: &str) -> bool {
        let Some(close) = typed.chars().next().filter(|c| matches!(c, '*' | '_' | '~' | '`')) else { return false };
        let before = &block.text[..pos.offset];
        // Only plain text: the rule never reaches across an atom or into code.
        let (index, run_start) = match block.run_at(pos.offset - 1) {
            Some(found) => found,
            None => return false,
        };
        if block.runs[index].marks & CODE != 0 {
            return false;
        }
        let _ = run_start;
        let candidates: &[(&str, u8)] = match close {
            '*' => &[("**", BOLD), ("*", ITALIC)],
            '_' => &[("__", BOLD), ("_", ITALIC)],
            '~' => &[("~~", STRIKE)],
            _ => &[("`", CODE)],
        };
        for &(delim, mark) in candidates {
            let Some(inner_end) = before.len().checked_sub(delim.len()) else { continue };
            if !before.ends_with(delim) {
                continue;
            }
            let body = &before[..inner_end];
            let Some(open) = body.rfind(delim) else { continue };
            let inner = &body[open + delim.len()..];
            if inner.is_empty() || inner.contains(ATOM) {
                continue;
            }
            let ch = delim.chars().next().unwrap();
            if mark != CODE {
                // `(?:^|\s)` before the opening delimiter, no delimiter characters inside,
                // and not ` **x **`: the inner text may not start with whitespace before the closing run.
                if inner.contains(ch) || inner.starts_with(char::is_whitespace) || inner.ends_with(char::is_whitespace) {
                    continue;
                }
                let lead = &body[..open];
                if !(lead.is_empty() || lead.ends_with(char::is_whitespace)) {
                    continue;
                }
                // `***` should not be read as bold+italic delimiters of an empty span.
                if delim.len() == 1 && lead.ends_with(ch) {
                    continue;
                }
            } else {
                if inner.contains('`') {
                    continue;
                }
                if body[..open].ends_with('`') {
                    continue;
                }
            }
            let b = self.doc.edit(pos.block);
            let close_at = inner_end;
            b.remove(close_at, pos.offset);
            b.remove(open, open + delim.len());
            let inner_start = open;
            let inner_stop = close_at - delim.len();
            b.set_mark(inner_start, inner_stop, mark, true);
            self.sel = Sel::caret(Pos::new(pos.block, inner_stop));
            // removeStoredMark: what is typed next is not formatted.
            let (marks, link) = self.doc.block(pos.block).style_at(inner_stop);
            self.stored = Some(Stored { marks: marks & !mark, link: if mark == CODE { None } else { link } });
            return true;
        }
        false
    }

    fn task_key_rule(&mut self, pos: Pos, block: &Block, typed: &str) -> bool {
        if self.task_keys.is_empty() {
            return false;
        }
        let Some(end) = typed.chars().next().filter(|c| c.is_whitespace() || ".,;:!?)]}\"'”’".contains(*c)) else { return false };
        let before = &block.text[..pos.offset - end.len_utf8()];
        let Some((key, at)) = trailing_task_key(before) else { return false };
        let Some(id) = self.task_keys.get(key).cloned() else { return false };
        // Not inside a link or code.
        if let Some((index, _)) = block.run_at(at) {
            let run = &block.runs[index];
            if run.link.is_some() || run.marks & CODE != 0 {
                return false;
            }
        }
        let key = key.to_string();
        let b = self.doc.edit(pos.block);
        b.remove(at, at + key.len());
        b.insert_atom(at, Atom::TaskRef { id, label: key.clone() });
        self.sel = Sel::caret(Pos::new(pos.block, pos.offset - key.len() + ATOM_LEN));
        true
    }

    fn autolink_rule(&mut self, pos: Pos, block: &Block, typed: &str) -> bool {
        if !typed.chars().next().is_some_and(char::is_whitespace) {
            return false;
        }
        self.autolink_word(pos.block, pos.offset - typed.len(), block)
    }

    fn autolink_before(&mut self, pos: Pos) {
        let block = self.doc.block(pos.block).clone();
        if !block.kind.is_literal() && !block.kind.is_atom() && pos.offset == block.len() {
            self.autolink_word(pos.block, pos.offset, &block);
        }
    }

    fn autolink_word(&mut self, index: usize, end: usize, block: &Block) -> bool {
        let before = &block.text[..end];
        let start = before.rfind(char::is_whitespace).map(|i| i + before[i..].chars().next().unwrap().len_utf8()).unwrap_or(0);
        let word = before[start..].trim_end_matches(['.', ',', ';', ':', '!', '?', ')']);
        if word.is_empty() || word.contains(ATOM) {
            return false;
        }
        let Some(href) = autolink_href(word) else { return false };
        let stop = start + word.len();
        if (start..stop).any(|o| block.run_at(o).is_some_and(|(i, _)| block.runs[i].link.is_some() || block.runs[i].marks & CODE != 0)) {
            return false;
        }
        self.doc.edit(index).set_link(start, stop, Some(Rc::new(Link { href, title: None })));
        true
    }

    /// Link `[KEY](kybern://task/…)` and known task keys in text that was just pasted.
    pub fn link_pasted(&mut self, start: Pos, end: Pos) {
        for index in start.block..=end.block.min(self.doc.len() - 1) {
            let block = self.doc.block(index).clone();
            if block.kind.is_literal() || block.kind.is_atom() {
                continue;
            }
            let a = if index == start.block { start.offset } else { 0 };
            let b = if index == end.block { end.offset.min(block.len()) } else { block.len() };
            let mut edits: Vec<(usize, usize, Atom)> = Vec::new();
            let text = &block.text[a..b];
            for (from, to, label, id) in markdown_task_links(text) {
                edits.push((a + from, a + to, Atom::TaskRef { id, label }));
            }
            for (from, key) in standalone_task_keys(text) {
                let from = a + from;
                if edits.iter().any(|(x, y, _)| from >= *x && from < *y) {
                    continue;
                }
                if let Some(id) = self.task_keys.get(&key) {
                    edits.push((from, from + key.len(), Atom::TaskRef { id: id.clone(), label: key }));
                }
            }
            edits.sort_by(|x, y| y.0.cmp(&x.0));
            for (from, to, atom) in edits {
                if (from..to).any(|o| block.run_at(o).is_some_and(|(i, _)| block.runs[i].link.is_some() || block.runs[i].marks & CODE != 0 || block.runs[i].atom.is_some())) {
                    continue;
                }
                let target = self.doc.edit(index);
                target.remove(from, to);
                target.insert_atom(from, atom);
                if self.sel.head.block == index && self.sel.head.offset >= to {
                    let shift = to - from - ATOM_LEN;
                    self.sel.head.offset -= shift;
                    self.sel.anchor = self.sel.head;
                }
            }
        }
    }

    /// Paste plain text: lines become blocks; known task keys and links are linked.
    pub fn paste_text(&mut self, text: &str) {
        let text = text.replace("\r\n", "\n").replace('\r', "\n");
        self.step(|core| {
            let start = core.sel.start();
            let literal = core.doc.block(start.block).kind.is_literal();
            // linkOnPaste: a URL pasted over selected text links it.
            if !core.sel.is_empty() && !literal && core.sel.start().block == core.sel.end().block {
                if let Some(href) = autolink_href(text.trim()).filter(|_| !text.trim().contains(char::is_whitespace)) {
                    let (a, b) = (core.sel.start(), core.sel.end());
                    core.doc.edit(a.block).set_link(a.offset, b.offset, Some(Rc::new(Link { href, title: None })));
                    return;
                }
            }
            let text = if literal { text } else { collapse_blank_lines(&text) };
            core.insert_text(&text, 0, false);
            let end = core.sel.head;
            core.link_pasted(start, end);
            if !literal {
                let end = core.sel.head;
                for index in start.block..=end.block {
                    let block = core.doc.block(index).clone();
                    let mut offset = 0;
                    for word in block.text.split(' ') {
                        let stop = offset + word.len();
                        let trimmed = word.trim_end_matches(['.', ',', ';', ':', '!', '?', ')', '\n']);
                        if !trimmed.is_empty() && autolink_href(trimmed).is_some() {
                            let block_now = core.doc.block(index).clone();
                            core.autolink_word(index, offset + trimmed.len(), &block_now);
                        }
                        offset = stop + 1;
                    }
                }
            }
        });
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BackspaceResult {
    Edited,
    Moved,
    /// Backspace at the start of an empty first line: the title can take the caret.
    AtStart,
}

fn before_typed(_before: &Snapshot, core: &Core) -> Snapshot {
    core.snapshot()
}

/// "```js" / "~~~" with an optional lowercase language.
pub fn fence_lang(text: &str) -> Option<String> {
    let rest = text.strip_prefix("```").or_else(|| text.strip_prefix("~~~"))?;
    rest.bytes().all(|b| b.is_ascii_lowercase()).then(|| rest.to_string())
}

/// Plain-text paste: PM makes a paragraph per line, so a run of blank lines is one break.
fn collapse_blank_lines(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut newlines = 0;
    for ch in text.chars() {
        if ch == '\n' {
            newlines += 1;
            if newlines == 1 {
                out.push('\n');
            }
        } else {
            newlines = 0;
            out.push(ch);
        }
    }
    out
}

pub fn previous_grapheme(text: &str, offset: usize) -> usize {
    graphemes(text).into_iter().rev().find(|&i| i < offset).unwrap_or(0)
}

pub fn next_grapheme(text: &str, offset: usize) -> usize {
    graphemes(text).into_iter().find(|&i| i > offset).unwrap_or(text.len())
}

/// Grapheme cluster starts plus the end. A simple approximation for the pure model;
/// the element uses unicode-segmentation where it is available.
fn graphemes(text: &str) -> Vec<usize> {
    let mut out = Vec::new();
    let mut previous: Option<char> = None;
    for (index, ch) in text.char_indices() {
        let joins = matches!(ch, '\u{200d}' | '\u{fe0f}' | '\u{20e3}' | '\u{1f3fb}'..='\u{1f3ff}' | '\u{0300}'..='\u{036f}' | '\u{e0020}'..='\u{e007f}')
            || previous == Some('\u{200d}')
            || (('\u{1f1e6}'..='\u{1f1ff}').contains(&ch) && previous.is_some_and(|p| ('\u{1f1e6}'..='\u{1f1ff}').contains(&p)) && out.len() % 2 == 1);
        if !joins {
            out.push(index);
        }
        previous = Some(ch);
    }
    out.push(text.len());
    out
}

fn is_key_body(word: &str) -> bool {
    let Some((prefix, number)) = word.split_once('-') else { return false };
    let p = prefix.as_bytes();
    (2..=5).contains(&p.len())
        && p[0].is_ascii_uppercase()
        && p[1..].iter().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
        && (1..=7).contains(&number.len())
        && number.as_bytes()[0] != b'0'
        && number.bytes().all(|b| b.is_ascii_digit())
}

fn key_may_follow(ch: Option<char>) -> bool {
    match ch {
        None => true,
        Some(c) => c.is_whitespace() || "([{\"'“‘".contains(c),
    }
}

/// `typedTaskKey` without its trailing character: a key that ends the text and stands alone.
pub fn trailing_task_key(text: &str) -> Option<(&str, usize)> {
    let start = text
        .char_indices()
        .rev()
        .find(|(_, c)| !(c.is_ascii_alphanumeric() || *c == '-'))
        .map(|(i, c)| i + c.len_utf8())
        .unwrap_or(0);
    let word = &text[start..];
    (is_key_body(word) && key_may_follow(text[..start].chars().last())).then_some((word, start))
}

/// `standaloneTaskKeys`: keys standing alone anywhere in pasted text.
pub fn standalone_task_keys(text: &str) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    let mut start = None;
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    for (n, &(index, ch)) in chars.iter().enumerate() {
        let word_char = ch.is_ascii_alphanumeric() || ch == '-';
        if word_char && start.is_none() {
            start = Some((n, index));
        }
        let ends = !word_char || n + 1 == chars.len();
        if let (true, Some((first, from))) = (ends, start) {
            let to = if word_char { index + ch.len_utf8() } else { index };
            let word = &text[from..to];
            let before = first.checked_sub(1).map(|i| chars[i].1);
            let after = if word_char { None } else { Some(ch) };
            if is_key_body(word) && key_may_follow(before) && after.is_none_or(|c| c.is_whitespace() || ".,;:!?)]}\"'”’".contains(c)) {
                out.push((from, word.to_string()));
            }
            start = None;
        }
    }
    out
}

/// `[label](kybern://task/<uuid>)` in pasted text: (from, to, label, id).
pub fn markdown_task_links(text: &str) -> Vec<(usize, usize, String, String)> {
    let mut out = Vec::new();
    let mut search = 0;
    while let Some(found) = text[search..].find("](kybern://task/") {
        let close = search + found;
        let Some(open) = text[..close].rfind('[') else { search = close + 1; continue };
        let label = &text[open + 1..close];
        let id_start = close + "](kybern://task/".len();
        let id: String = text[id_start..].chars().take(36).collect();
        let mut end = id_start + id.len();
        if text[end..].starts_with('/') {
            end += 1;
        }
        let valid = id.len() == 36 && id.chars().all(|c| c.is_ascii_hexdigit() || c == '-') && text[end..].starts_with(')') && !label.is_empty() && label.chars().count() <= 64 && !label.contains(['\n', ']']);
        if valid {
            out.push((open, end + 1, label.to_string(), id.to_lowercase()));
            search = end + 1;
        } else {
            search = close + 1;
        }
    }
    out
}

/// What autolink treats as a URL: a scheme, `www.`, or a bare domain with a common TLD.
pub fn autolink_href(word: &str) -> Option<String> {
    let lower = word.to_ascii_lowercase();
    if (lower.starts_with("https://") || lower.starts_with("http://")) && word.len() > lower.find("://").unwrap() + 3 {
        return Some(word.to_string());
    }
    if lower.starts_with("mailto:") && word.contains('@') {
        return Some(word.to_string());
    }
    let host = lower.split(['/', '?', '#']).next().unwrap_or("");
    let labels: Vec<&str> = host.split('.').collect();
    let tld_ok = labels.len() >= 2
        && labels.iter().all(|l| !l.is_empty() && l.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'))
        && matches!(*labels.last().unwrap(), "com" | "org" | "net" | "io" | "dev" | "app" | "ai" | "co" | "gov" | "edu" | "me" | "so" | "sh" | "xyz" | "info" | "us" | "uk" | "de" | "fr" | "jp" | "id");
    if lower.starts_with("www.") && labels.len() >= 3 && tld_ok || tld_ok && !word.contains('@') {
        return Some(format!("https://{word}"));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(texts: &[(&str, Kind)]) -> Core {
        let blocks = texts.iter().enumerate().map(|(i, (t, k))| Block::new(i as u64 + 1, k.clone(), t)).collect();
        Core::new(Doc::new(blocks))
    }

    fn texts(core: &Core) -> Vec<String> {
        core.doc.blocks.iter().map(|b| b.text.clone()).collect()
    }

    fn typing(core: &mut Core, text: &str, mut now: u64) -> u64 {
        for ch in text.chars() {
            core.insert_text(&ch.to_string(), now, true);
            now += 50;
        }
        now
    }

    #[test]
    fn typing_groups_into_one_step_and_formatting_is_its_own() {
        let mut core = doc(&[("", Kind::Paragraph)]);
        let now = typing(&mut core, "hello", 0);
        assert_eq!(texts(&core), ["hello"]);
        core.sel = Sel { anchor: Pos::new(0, 0), head: Pos::new(0, 5) };
        core.toggle_mark(BOLD);
        assert_eq!(core.doc.block(0).runs[0].marks, BOLD);
        core.sel = Sel::caret(Pos::new(0, 5));
        typing(&mut core, " you", now + 1000);
        assert_eq!(texts(&core), ["hello you"]);
        assert!(core.undo());
        assert_eq!(texts(&core), ["hello"]);
        assert!(core.undo());
        assert_eq!(core.doc.block(0).runs[0].marks, 0);
        assert!(core.undo());
        assert_eq!(texts(&core), [""]);
        assert!(core.redo() && core.redo() && core.redo());
        assert_eq!(texts(&core), ["hello you"]);
    }

    #[test]
    fn a_pause_starts_a_new_typing_step() {
        let mut core = doc(&[("", Kind::Paragraph)]);
        typing(&mut core, "ab", 0);
        typing(&mut core, "cd", 5000);
        core.undo();
        assert_eq!(texts(&core), ["ab"]);
    }

    #[test]
    fn enter_splits_and_backspace_joins() {
        let mut core = doc(&[("hello world", Kind::Paragraph)]);
        core.sel = Sel::caret(Pos::new(0, 5));
        core.enter(0);
        assert_eq!(texts(&core), ["hello", " world"]);
        assert_eq!(core.sel.head, Pos::new(1, 0));
        assert_eq!(core.backspace(10), BackspaceResult::Edited);
        assert_eq!(texts(&core), ["hello world"]);
        assert_eq!(core.sel.head, Pos::new(0, 5));
    }

    #[test]
    fn heading_enter_at_end_makes_a_paragraph() {
        let mut core = doc(&[("Title", Kind::Heading(2))]);
        core.sel = Sel::caret(Pos::new(0, 5));
        core.enter(0);
        assert_eq!(core.doc.block(1).kind, Kind::Paragraph);
    }

    #[test]
    fn list_enter_continues_and_empty_item_lifts() {
        let mut core = doc(&[("one", Kind::Task(true))]);
        core.sel = Sel::caret(Pos::new(0, 3));
        core.enter(0);
        assert_eq!(core.doc.block(1).kind, Kind::Task(false));
        core.enter(10);
        assert_eq!(core.doc.len(), 2);
        assert_eq!(core.doc.block(1).kind, Kind::Paragraph);
    }

    #[test]
    fn block_input_rules_and_backspace_undoes_them() {
        let mut core = doc(&[("", Kind::Paragraph)]);
        typing(&mut core, "## ", 0);
        assert_eq!(core.doc.block(0).kind, Kind::Heading(2));
        assert_eq!(texts(&core), [""]);
        core.backspace(500);
        assert_eq!(core.doc.block(0).kind, Kind::Paragraph);
        assert_eq!(texts(&core), ["## "]);
        let mut core = doc(&[("", Kind::Paragraph)]);
        typing(&mut core, "- [ ] x", 0);
        assert_eq!(core.doc.block(0).kind, Kind::Task(false));
        assert_eq!(texts(&core), ["x"]);
        let mut core = doc(&[("", Kind::Paragraph)]);
        typing(&mut core, "12. ", 0);
        assert_eq!(core.doc.block(0).kind, Kind::Ordered(12));
        let mut core = doc(&[("", Kind::Paragraph)]);
        typing(&mut core, "> q", 0);
        assert_eq!(core.doc.block(0).quote, 1);
        let mut core = doc(&[("", Kind::Paragraph)]);
        typing(&mut core, "---", 0);
        assert_eq!(core.doc.block(0).kind, Kind::Rule);
        assert_eq!(core.sel.head, Pos::new(1, 0));
        let mut core = doc(&[("", Kind::Paragraph)]);
        typing(&mut core, "```rust ", 0);
        assert_eq!(core.doc.block(0).kind, Kind::Code("rust".into()));
    }

    #[test]
    fn mark_input_rules() {
        let mut core = doc(&[("", Kind::Paragraph)]);
        typing(&mut core, "a **bold** and *it* `c` ~~s~~", 0);
        let b = core.doc.block(0);
        assert_eq!(b.text, "a bold and it c s");
        let marks: Vec<(usize, u8)> = b.runs.iter().map(|r| (r.len, r.marks)).collect();
        assert_eq!(marks, [(2, 0), (4, BOLD), (5, 0), (2, ITALIC), (1, 0), (1, CODE), (1, 0), (1, STRIKE)]);
        let mut core = doc(&[("", Kind::Paragraph)]);
        typing(&mut core, "2*3*4", 0);
        assert_eq!(core.doc.block(0).runs.len(), 1, "no emphasis without a space before");
    }

    #[test]
    fn task_keys_link_when_typed_and_pasted() {
        let mut core = doc(&[("", Kind::Paragraph)]);
        core.task_keys.insert("ADE-14".into(), "11111111-1111-1111-1111-111111111111".into());
        typing(&mut core, "see ADE-14 now", 0);
        let b = core.doc.block(0);
        assert_eq!(b.text, format!("see {ATOM} now"));
        assert!(matches!(b.runs[1].atom.as_deref(), Some(Atom::TaskRef { label, .. }) if label == "ADE-14"));
        let mut core = doc(&[("", Kind::Paragraph)]);
        core.task_keys.insert("ADE-14".into(), "11111111-1111-1111-1111-111111111111".into());
        core.paste_text("fix ADE-14, and [Old](kybern://task/22222222-2222-2222-2222-222222222222) x/ADE-14");
        let b = core.doc.block(0);
        assert_eq!(b.runs.iter().filter(|r| r.atom.is_some()).count(), 2);
        assert!(b.text.ends_with("x/ADE-14"));
    }

    #[test]
    fn deleting_across_blocks_keeps_the_first_kind() {
        let mut core = doc(&[("Heading", Kind::Heading(1)), ("middle", Kind::Paragraph), ("item text", Kind::Bullet)]);
        core.sel = Sel { anchor: Pos::new(0, 4), head: Pos::new(2, 4) };
        core.delete_selection(0, None);
        assert_eq!(texts(&core), ["Head text"]);
        assert_eq!(core.doc.block(0).kind, Kind::Heading(1));
        core.undo();
        assert_eq!(texts(&core), ["Heading", "middle", "item text"]);
    }

    #[test]
    fn atoms_select_then_delete() {
        let mut core = doc(&[("a", Kind::Paragraph), ("", Kind::Rule), ("b", Kind::Paragraph)]);
        core.sel = Sel::caret(Pos::new(2, 0));
        assert_eq!(core.backspace(0), BackspaceResult::Moved);
        assert_eq!(core.sel.start(), Pos::new(1, 0));
        core.backspace(10);
        assert_eq!(texts(&core), ["a", "b"]);
    }

    #[test]
    fn flat_utf16_round_trips() {
        let core = doc(&[("héllo 😀", Kind::Paragraph), ("x", Kind::Paragraph)]);
        let pos = Pos::new(1, 1);
        let flat = core.doc.flat_utf16(pos);
        assert_eq!(flat, 8 + 1 + 1);
        assert_eq!(core.doc.pos_from_flat_utf16(flat), pos);
        assert_eq!(core.doc.pos_from_flat_utf16(8), Pos::new(0, "héllo 😀".len()));
    }

    #[test]
    fn composition_is_one_step() {
        let mut core = doc(&[("", Kind::Paragraph)]);
        core.set_marked("ｎ", 0);
        core.set_marked("に", 10);
        core.set_marked("日本", 20);
        core.insert_text("日本", 30, true);
        assert_eq!(texts(&core), ["日本"]);
        assert!(core.marked.is_none());
        core.undo();
        assert_eq!(texts(&core), [""]);
        assert!(!core.undo());
    }

    #[test]
    fn ordinals_count_within_one_list() {
        let mut core = doc(&[("a", Kind::Ordered(3)), ("b", Kind::Ordered(1)), ("c", Kind::Ordered(1))]);
        core.doc.edit(1).depth = 1;
        assert_eq!(core.doc.ordinal(0), 3);
        assert_eq!(core.doc.ordinal(1), 1);
        assert_eq!(core.doc.ordinal(2), 4);
    }

    #[test]
    fn code_excludes_other_marks_and_links() {
        let mut core = doc(&[("abc", Kind::Paragraph)]);
        core.sel = Sel { anchor: Pos::new(0, 0), head: Pos::new(0, 3) };
        core.toggle_mark(BOLD);
        core.set_link(Some(Link { href: "https://x.io".into(), title: None }), None);
        core.toggle_mark(CODE);
        let run = &core.doc.block(0).runs[0];
        assert_eq!((run.marks, run.link.is_none()), (CODE, true));
    }

    #[test]
    fn paste_lines_become_paragraphs_and_urls_link() {
        let mut core = doc(&[("", Kind::Paragraph)]);
        core.paste_text("one\n\n\ntwo https://kybern.dev");
        assert_eq!(texts(&core), ["one", "two https://kybern.dev"]);
        assert!(core.doc.block(1).runs.iter().any(|r| r.link.is_some()));
        core.undo();
        assert_eq!(texts(&core), [""]);
    }

    #[test]
    fn tab_nests_under_a_previous_item() {
        let mut core = doc(&[("a", Kind::Bullet), ("b", Kind::Bullet)]);
        core.sel = Sel::caret(Pos::new(1, 0));
        core.indent(true);
        assert_eq!(core.doc.block(1).depth, 1);
        core.indent(true);
        assert_eq!(core.doc.block(1).depth, 1, "one level deeper than the item above at most");
        core.sel = Sel::caret(Pos::new(0, 0));
        core.indent(true);
        assert_eq!(core.doc.block(0).depth, 0);
    }
}
