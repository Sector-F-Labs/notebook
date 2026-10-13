//! The WYSIWYG note editor (ADR-0002): a rich-text widget of our own that
//! edits the document model directly, with no markdown syntax on screen.
//!
//! Each text paragraph is laid out as one egui galley from its styled spans;
//! list markers, checkboxes and quote bars are painted beside the text, so
//! the cursor never enters them. Tables, code blocks and images are embedded
//! widgets (see `embeds`). The cursor lives in text paragraphs only and skips over
//! embeds.
//!
//! Input is read from egui's events while the editor has focus: typed text
//! and IME commits, editing and movement keys, clipboard events, and mouse
//! presses and drags on the text. Keyboard shortcuts for formatting are
//! handled by the app, which calls `toggle_inline` and `set_block`.

use std::path::PathBuf;
use std::sync::Arc;

use egui::text::{CCursor, LayoutJob, TextFormat};
use egui::{Color32, Event, EventFilter, Galley, Id, Key, Modifiers, Pos2, Rect, Sense, Stroke, Vec2, vec2};

use crate::doc::{self, Para, Pos};
use crate::embeds;
use crate::fonts;
use crate::markdown::{self, Block, Image, Inline, Line, ListKind, MAX_DEPTH, Span, Table};

/// Paragraph styles offered by the toolbar and shortcuts.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BlockStyle {
    Paragraph,
    Heading(u8),
    Quote,
    Bullet,
    Ordered,
    Task,
    Code,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Clipboard {
    Cut,
    Copy,
    Paste,
}

#[derive(Debug, Clone, PartialEq)]
enum MenuAction {
    Clipboard(Clipboard),
    Inline(Inline),
    CodeBlock,
    InsertTable,
    SelectAll,
}

fn code_id(editor: Id, para: usize) -> Id {
    editor.with(("code", para))
}

/// The id of a code block's text field (see `embeds::code`).
fn code_text_id(editor: Id, para: usize) -> Id {
    code_id(editor, para).with("text")
}

/// The right-click menu on the text.
fn context_menu(ui: &mut egui::Ui, has_selection: bool, picked: &mut Option<MenuAction>) {
    let mut item = |ui: &mut egui::Ui, enabled: bool, label: &str, keys: &str, action: MenuAction| {
        if ui.add_enabled(enabled, egui::Button::new(label).shortcut_text(keys)).clicked() {
            *picked = Some(action);
            ui.close();
        }
    };
    item(ui, has_selection, "Cut", "Ctrl+X", MenuAction::Clipboard(Clipboard::Cut));
    item(ui, has_selection, "Copy", "Ctrl+C", MenuAction::Clipboard(Clipboard::Copy));
    item(ui, true, "Paste", "Ctrl+V", MenuAction::Clipboard(Clipboard::Paste));
    ui.separator();
    item(ui, true, "Bold", "Ctrl+B", MenuAction::Inline(Inline::Bold));
    item(ui, true, "Italic", "Ctrl+I", MenuAction::Inline(Inline::Italic));
    item(ui, true, "Strikethrough", "Ctrl+Shift+X", MenuAction::Inline(Inline::Strike));
    item(ui, true, "Code", "Ctrl+E", MenuAction::Inline(Inline::Code));
    ui.separator();
    let code_label = if has_selection { "Make Code Block" } else { "Code Block" };
    item(ui, true, code_label, "", MenuAction::CodeBlock);
    item(ui, true, "Insert Table", "", MenuAction::InsertTable);
    ui.separator();
    item(ui, true, "Select All", "Ctrl+A", MenuAction::SelectAll);
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum EditKind {
    Typing,
    Deleting,
    Other,
}

#[derive(Clone)]
struct Snapshot {
    paras: Vec<Para>,
    cursor: Pos,
    anchor: Pos,
}

/// Where a text paragraph was drawn last frame.
#[derive(Clone)]
struct Placed {
    galley: Arc<Galley>,
    /// The galley's top-left corner on screen.
    origin: Pos2,
    /// The paragraph's whole row, inset and spacing included.
    rect: Rect,
}

/// Typing within this many seconds of the last keystroke joins its undo step.
const UNDO_GROUP_SECS: f64 = 1.0;
const UNDO_LIMIT: usize = 200;
const BODY_SIZE: f32 = 15.0;
const INDENT: f32 = 24.0;

pub struct Editor {
    paras: Vec<Para>,
    cursor: Pos,
    anchor: Pos,
    /// Styles for the next typed text, set by toggling a style with nothing
    /// selected; cleared when the cursor moves.
    typing: Option<Vec<Inline>>,
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
    last_kind: Option<EditKind>,
    last_time: f64,
    placed: Vec<Option<Placed>>,
    /// Remembered x position for moving up and down through rows.
    h_pos: Option<f32>,
    dragging: bool,
    /// The last copy: its plain text and the paragraphs it came from, so
    /// pasting it back inside the app keeps the formatting.
    clip: Option<(String, Vec<Para>)>,
    preedit: Option<String>,
    edits: u64,
    scroll_to_cursor: bool,
    /// A code block just made, whose text should take the keyboard.
    focus_code: Option<usize>,
    /// The note's folder, which relative image paths start from.
    dir: Option<PathBuf>,
    id: Id,
}

impl Editor {
    pub fn new(id: Id) -> Self {
        Self {
            paras: vec![Para::text(Block::Paragraph)],
            cursor: Pos::default(),
            anchor: Pos::default(),
            typing: None,
            undo: Vec::new(),
            redo: Vec::new(),
            last_kind: None,
            last_time: 0.0,
            placed: Vec::new(),
            h_pos: None,
            dragging: false,
            clip: None,
            preedit: None,
            edits: 0,
            scroll_to_cursor: false,
            focus_code: None,
            dir: None,
            id,
        }
    }

    /// Sets the folder relative image paths are found from: the note's.
    pub fn set_dir(&mut self, dir: Option<PathBuf>) {
        self.dir = dir;
    }

    /// Replaces the document with `md`, clearing undo history.
    pub fn load(&mut self, md: &str) {
        self.paras = doc::from_lines(markdown::parse(md));
        self.cursor = self.fix(Pos::default());
        self.anchor = self.cursor;
        self.undo.clear();
        self.redo.clear();
        self.last_kind = None;
        self.typing = None;
        self.placed.clear();
    }

    pub fn markdown(&self) -> String {
        markdown::to_markdown(&doc::to_lines(&self.paras))
    }

    /// Counts edits, so the app can tell when the note changed.
    pub fn edits(&self) -> u64 {
        self.edits
    }

    pub fn focus(&self, ctx: &egui::Context) {
        ctx.memory_mut(|m| m.request_focus(self.id));
    }

    pub fn has_focus(&self, ctx: &egui::Context) -> bool {
        ctx.memory(|m| m.has_focus(self.id))
    }

    /// Puts the cursor at the end of the first line (after a new note's title).
    pub fn cursor_to_first_line_end(&mut self) {
        let pos = self.fix(Pos::new(0, usize::MAX));
        self.cursor = pos;
        self.anchor = pos;
    }

    // -----------------------------------------------------------------------
    // Positions
    // -----------------------------------------------------------------------

    fn line(&self, para: usize) -> &Line {
        self.paras[para].as_text().expect("cursor is in a text paragraph")
    }

    fn line_mut(&mut self, para: usize) -> &mut Line {
        self.paras[para].as_text_mut().expect("cursor is in a text paragraph")
    }

    fn line_len(&self, para: usize) -> usize {
        doc::len(&self.line(para).spans)
    }

    fn next_text(&self, para: usize) -> Option<usize> {
        (para + 1..self.paras.len()).find(|&i| self.paras[i].as_text().is_some())
    }

    fn prev_text(&self, para: usize) -> Option<usize> {
        (0..para).rev().find(|&i| self.paras[i].as_text().is_some())
    }

    /// The nearest valid position: in a text paragraph, within its length.
    fn fix(&self, pos: Pos) -> Pos {
        let para = pos.para.min(self.paras.len() - 1);
        let para = if self.paras[para].as_text().is_some() {
            para
        } else {
            self.next_text(para).or_else(|| self.prev_text(para)).unwrap_or(0)
        };
        Pos::new(para, pos.col.min(self.line_len(para)))
    }

    fn selection(&self) -> (Pos, Pos) {
        (self.cursor.min(self.anchor), self.cursor.max(self.anchor))
    }

    fn has_selection(&self) -> bool {
        self.cursor != self.anchor
    }

    fn move_to(&mut self, pos: Pos, extend: bool) {
        self.cursor = self.fix(pos);
        if !extend {
            self.anchor = self.cursor;
        }
        self.typing = None;
        self.scroll_to_cursor = true;
    }

    // -----------------------------------------------------------------------
    // Undo
    // -----------------------------------------------------------------------

    fn snapshot(&self) -> Snapshot {
        Snapshot { paras: self.paras.clone(), cursor: self.cursor, anchor: self.anchor }
    }

    /// Records the state before an edit, joining runs of typing.
    fn begin(&mut self, kind: EditKind, now: f64) {
        let joins = kind != EditKind::Other && self.last_kind == Some(kind) && now - self.last_time < UNDO_GROUP_SECS;
        if !joins {
            self.undo.push(self.snapshot());
            if self.undo.len() > UNDO_LIMIT {
                self.undo.remove(0);
            }
        }
        self.redo.clear();
        self.last_kind = Some(kind);
        self.last_time = now;
        self.edits += 1;
    }

    fn restore(&mut self, snap: Snapshot) {
        self.paras = snap.paras;
        self.cursor = self.fix(snap.cursor);
        self.anchor = self.fix(snap.anchor);
        self.last_kind = None;
        self.edits += 1;
        self.scroll_to_cursor = true;
    }

    pub fn undo(&mut self) {
        if let Some(snap) = self.undo.pop() {
            self.redo.push(self.snapshot());
            self.restore(snap);
        }
    }

    pub fn redo(&mut self) {
        if let Some(snap) = self.redo.pop() {
            self.undo.push(self.snapshot());
            self.restore(snap);
        }
    }

    // -----------------------------------------------------------------------
    // Editing
    // -----------------------------------------------------------------------

    /// Deletes the selection; the paragraphs between its ends go entirely.
    fn delete_selection(&mut self) {
        let (a, b) = self.selection();
        if a == b {
            return;
        }
        if a.para == b.para {
            doc::delete(&mut self.line_mut(a.para).spans, a.col..b.col);
        } else {
            let tail = doc::split_off(&mut self.line_mut(b.para).spans, b.col);
            let first = self.line_mut(a.para);
            doc::split_off(&mut first.spans, a.col);
            doc::append(&mut first.spans, tail);
            self.paras.drain(a.para + 1..=b.para);
        }
        self.cursor = a;
        self.anchor = a;
        doc::normalize(&mut self.paras);
    }

    /// Inserts text without newlines at the cursor.
    fn insert_inline(&mut self, text: &str) {
        let Pos { para, col } = self.cursor;
        let styles = match &self.line(para).block {
            Block::Raw => Vec::new(),
            _ => self.typing.clone().unwrap_or_else(|| doc::styles_at(&self.line(para).spans, col)),
        };
        doc::insert(&mut self.line_mut(para).spans, col, text, &styles);
        self.cursor.col += text.chars().count();
        self.anchor = self.cursor;
    }

    /// Inserts text, starting new paragraphs at newlines.
    fn insert_text(&mut self, text: &str) {
        self.delete_selection();
        let text = text.replace("\r\n", "\n");
        for (i, chunk) in text.split('\n').enumerate() {
            if i > 0 {
                self.split(false);
            }
            if !chunk.is_empty() {
                self.insert_inline(chunk);
            }
        }
        self.scroll_to_cursor = true;
    }

    /// Enter: splits the paragraph at the cursor. With `exit_empty`, Enter
    /// on an empty list item or quote line leaves the list or quote instead.
    fn split(&mut self, exit_empty: bool) {
        let Pos { para, col } = self.cursor;
        let block = self.line(para).block.clone();
        let empty = self.line_len(para) == 0;
        if exit_empty && empty {
            match block {
                Block::Item { depth, kind } if depth > 0 => {
                    self.line_mut(para).block = Block::Item { kind, depth: depth - 1 };
                    return;
                }
                Block::Item { .. } | Block::Quote => {
                    self.line_mut(para).block = Block::Paragraph;
                    return;
                }
                _ => {}
            }
        }
        let next_block = match &block {
            Block::Heading(_) => Block::Paragraph,
            Block::Item { kind, depth } => Block::Item {
                kind: match kind {
                    ListKind::Task(_) => ListKind::Task(false),
                    ListKind::Ordered(n) => ListKind::Ordered(n + 1),
                    ListKind::Bullet => ListKind::Bullet,
                },
                depth: *depth,
            },
            other => other.clone(),
        };
        if col == 0 && !empty {
            // At the start of a line, push it down rather than splitting it,
            // so a heading stays a heading.
            let before = if matches!(block, Block::Heading(_)) { Block::Paragraph } else { next_block };
            self.paras.insert(para, Para::text(before));
            self.cursor = Pos::new(para + 1, 0);
        } else {
            let tail = doc::split_off(&mut self.line_mut(para).spans, col);
            self.paras.insert(para + 1, Para::Text(Line { block: next_block, spans: tail }));
            self.cursor = Pos::new(para + 1, 0);
        }
        self.anchor = self.cursor;
    }

    fn backspace(&mut self, word: bool) {
        if self.has_selection() {
            self.delete_selection();
            return;
        }
        let Pos { para, col } = self.cursor;
        if col > 0 {
            let from = if word { doc::word_boundary(&doc::plain(&self.line(para).spans), col, false) } else { col - 1 };
            doc::delete(&mut self.line_mut(para).spans, from..col);
            self.cursor.col = from;
            self.anchor = self.cursor;
            return;
        }
        // At the start of a line: first undo its paragraph style.
        match self.line(para).block.clone() {
            Block::Item { depth, kind } if depth > 0 => {
                self.line_mut(para).block = Block::Item { kind, depth: depth - 1 };
                return;
            }
            Block::Item { .. } | Block::Quote | Block::Heading(_) => {
                self.line_mut(para).block = Block::Paragraph;
                return;
            }
            _ => {}
        }
        match para.checked_sub(1).map(|p| &self.paras[p]) {
            Some(Para::Text(_)) => {
                let prev = para - 1;
                let end = self.line_len(prev);
                let spans = std::mem::take(&mut self.line_mut(para).spans);
                doc::append(&mut self.line_mut(prev).spans, spans);
                self.paras.remove(para);
                self.cursor = Pos::new(prev, end);
                self.anchor = self.cursor;
            }
            // After a table or code block, an empty line just goes away.
            Some(_) if self.line_len(para) == 0 && self.next_text(para).is_some() => {
                self.paras.remove(para);
                let pos = self.prev_text(para).map(|p| Pos::new(p, usize::MAX)).unwrap_or_default();
                self.cursor = self.fix(pos);
                self.anchor = self.cursor;
            }
            _ => {}
        }
    }

    fn delete_forward(&mut self, word: bool) {
        if self.has_selection() {
            self.delete_selection();
            return;
        }
        let Pos { para, col } = self.cursor;
        let len = self.line_len(para);
        if col < len {
            let to = if word { doc::word_boundary(&doc::plain(&self.line(para).spans), col, true) } else { col + 1 };
            doc::delete(&mut self.line_mut(para).spans, col..to);
        } else if let Some(Para::Text(_)) = self.paras.get(para + 1) {
            let Para::Text(next) = self.paras.remove(para + 1) else { unreachable!() };
            doc::append(&mut self.line_mut(para).spans, next.spans);
        }
    }

    /// Selected text paragraph indices.
    fn selected_paras(&self) -> Vec<usize> {
        let (a, b) = self.selection();
        (a.para..=b.para).filter(|&i| self.paras[i].as_text().is_some()).collect()
    }

    /// Tab / Shift+Tab on list items. Returns whether anything was a list item.
    fn indent(&mut self, delta: i8) -> bool {
        let mut any = false;
        for i in self.selected_paras() {
            if let Block::Item { depth, .. } = &mut self.line_mut(i).block {
                *depth = (*depth as i8 + delta).clamp(0, MAX_DEPTH as i8 - 1) as u8;
                any = true;
            }
        }
        any
    }

    fn toggle_task(&mut self, para: usize, now: f64) {
        if let Some(Line { block: Block::Item { kind: ListKind::Task(done), .. }, .. }) = self.paras[para].as_text() {
            let done = *done;
            self.begin(EditKind::Other, now);
            if let Block::Item { kind, .. } = &mut self.line_mut(para).block {
                *kind = ListKind::Task(!done);
            }
        }
    }

    /// Whether the selection (or the typing style, with nothing selected)
    /// has `style`, for the toolbar.
    pub fn inline_active(&self, style: &Inline) -> bool {
        if !self.has_selection() {
            return match &self.typing {
                Some(t) => t.contains(style),
                None => doc::styles_at(&self.line(self.cursor.para).spans, self.cursor.col).contains(style),
            };
        }
        let (a, b) = self.selection();
        self.selected_paras().iter().all(|&i| {
            let len = self.line_len(i);
            let start = if i == a.para { a.col } else { 0 };
            let end = if i == b.para { b.col } else { len };
            start >= end || doc::all_have(&self.line(i).spans, start..end, style)
        })
    }

    /// Bold, italic and so on: over the selection, or for the next typed
    /// text when nothing is selected.
    pub fn toggle_inline(&mut self, style: Inline, now: f64) {
        let on = !self.inline_active(&style);
        if !self.has_selection() {
            let mut typing = self
                .typing
                .clone()
                .unwrap_or_else(|| doc::styles_at(&self.line(self.cursor.para).spans, self.cursor.col));
            typing.retain(|s| *s != style);
            if on {
                typing.push(style);
            }
            self.typing = Some(typing);
            return;
        }
        self.begin(EditKind::Other, now);
        let (a, b) = self.selection();
        for i in self.selected_paras() {
            let len = self.line_len(i);
            let start = if i == a.para { a.col } else { 0 };
            let end = if i == b.para { b.col } else { len };
            doc::set_style(&mut self.line_mut(i).spans, start..end, &style, on);
        }
    }

    /// The paragraph style at the cursor, for the toolbar.
    pub fn block_at_cursor(&self) -> BlockStyle {
        match &self.line(self.cursor.para).block {
            Block::Heading(n) => BlockStyle::Heading(*n),
            Block::Quote => BlockStyle::Quote,
            Block::Item { kind: ListKind::Bullet, .. } => BlockStyle::Bullet,
            Block::Item { kind: ListKind::Ordered(_), .. } => BlockStyle::Ordered,
            Block::Item { kind: ListKind::Task(_), .. } => BlockStyle::Task,
            _ => BlockStyle::Paragraph,
        }
    }

    /// Sets the selected paragraphs' style. Setting the style they already
    /// have turns them back into body text.
    pub fn set_block(&mut self, style: BlockStyle, now: f64) {
        self.begin(EditKind::Other, now);
        if style == BlockStyle::Code {
            self.make_code();
            return;
        }
        let paras = self.selected_paras();
        let all_already = paras.iter().all(|&i| block_matches(&self.line(i).block, style));
        for i in paras {
            let line = self.line_mut(i);
            let depth = match line.block {
                Block::Item { depth, .. } => depth,
                _ => 0,
            };
            line.block = if all_already {
                Block::Paragraph
            } else {
                match style {
                    BlockStyle::Paragraph | BlockStyle::Code => Block::Paragraph,
                    BlockStyle::Heading(n) => Block::Heading(n),
                    BlockStyle::Quote => Block::Quote,
                    BlockStyle::Bullet => Block::Item { kind: ListKind::Bullet, depth },
                    BlockStyle::Ordered => Block::Item { kind: ListKind::Ordered(1), depth },
                    BlockStyle::Task => Block::Item { kind: ListKind::Task(false), depth },
                }
            };
        }
    }

    /// Turns the selected text paragraphs into one code block.
    fn make_code(&mut self) {
        let (a, b) = self.selection();
        let code: Vec<String> =
            (a.para..=b.para).filter_map(|i| self.paras[i].as_text().map(|l| doc::plain(&l.spans))).collect();
        self.paras.splice(a.para..=b.para, [Para::Code { lang: String::new(), code: code.join("\n") }]);
        self.focus_code = Some(a.para);
        if !matches!(self.paras.get(a.para + 1), Some(Para::Text(_))) {
            self.paras.insert(a.para + 1, Para::text(Block::Paragraph));
        }
        self.cursor = Pos::new(a.para + 1, 0);
        self.anchor = self.cursor;
    }

    /// Inserts a 2×2 table; see `insert_embed`.
    pub fn insert_table(&mut self, now: f64) {
        self.insert_embed(Para::Table(Table::new(2, 2)), now);
    }

    /// Inserts an image; see `insert_embed`.
    pub fn insert_image(&mut self, image: Image, now: f64) {
        self.insert_embed(Para::Image(image), now);
    }

    /// Inserts a table or image after the cursor's paragraph, or in place
    /// of it when it's empty, and puts the cursor on the line below.
    fn insert_embed(&mut self, embed: Para, now: f64) {
        self.begin(EditKind::Other, now);
        let para = self.cursor.para;
        let at = if self.line_len(para) == 0 && self.line(para).block == Block::Paragraph {
            self.paras[para] = embed;
            para
        } else {
            self.paras.insert(para + 1, embed);
            para + 1
        };
        if !matches!(self.paras.get(at + 1), Some(Para::Text(_))) {
            self.paras.insert(at + 1, Para::text(Block::Paragraph));
        }
        self.cursor = Pos::new(at + 1, 0);
        self.anchor = self.cursor;
    }

    pub fn select_all(&mut self) {
        self.anchor = self.fix(Pos::new(0, 0));
        self.cursor = self.fix(Pos::new(self.paras.len() - 1, usize::MAX));
    }

    // -----------------------------------------------------------------------
    // Clipboard
    // -----------------------------------------------------------------------

    /// The selection as paragraphs, and as plain text.
    fn copy_selection(&self) -> Option<(String, Vec<Para>)> {
        let (a, b) = self.selection();
        if a == b {
            return None;
        }
        let mut paras = Vec::new();
        for i in a.para..=b.para {
            match &self.paras[i] {
                Para::Text(line) => {
                    let len = doc::len(&line.spans);
                    let start = if i == a.para { a.col } else { 0 };
                    let end = if i == b.para { b.col } else { len };
                    paras.push(Para::Text(Line {
                        block: line.block.clone(),
                        spans: doc::slice(&line.spans, start..end),
                    }));
                }
                other => paras.push(other.clone()),
            }
        }
        let plain: Vec<String> = paras
            .iter()
            .map(|p| match p {
                Para::Text(line) => doc::plain(&line.spans),
                other => markdown::to_markdown(&doc::to_lines(std::slice::from_ref(other))).trim_end().to_string(),
            })
            .collect();
        Some((plain.join("\n"), paras))
    }

    fn paste(&mut self, text: &str) {
        match self.clip.clone() {
            Some((plain, paras)) if plain == text => self.paste_paras(paras),
            _ => self.insert_text(text),
        }
    }

    /// Pastes copied paragraphs: the first joins the cursor's line, the
    /// last takes the rest of it, and any between are inserted whole.
    fn paste_paras(&mut self, mut paras: Vec<Para>) {
        self.delete_selection();
        let Pos { para, col } = self.cursor;
        if paras.len() == 1
            && let Para::Text(line) = &paras[0]
        {
            let n = doc::len(&line.spans);
            let spans = &mut self.line_mut(para).spans;
            let tail = doc::split_off(spans, col);
            doc::append(spans, line.spans.clone());
            doc::append(spans, tail);
            self.cursor.col += n;
            self.anchor = self.cursor;
            return;
        }
        let tail = doc::split_off(&mut self.line_mut(para).spans, col);
        if let Some(Para::Text(first)) = paras.first() {
            let spans = first.spans.clone();
            doc::append(&mut self.line_mut(para).spans, spans);
            paras.remove(0);
        }
        let count = paras.len();
        let end = match paras.last_mut() {
            Some(Para::Text(last)) => {
                let col = doc::len(&last.spans);
                doc::append(&mut last.spans, tail);
                Pos::new(para + count, col)
            }
            _ => {
                paras.push(Para::Text(Line { block: Block::Paragraph, spans: tail }));
                Pos::new(para + count + 1, 0)
            }
        };
        self.paras.splice(para + 1..para + 1, paras);
        self.cursor = self.fix(end);
        self.anchor = self.cursor;
    }

    // -----------------------------------------------------------------------
    // Input
    // -----------------------------------------------------------------------

    fn handle_events(&mut self, ui: &egui::Ui) {
        let now = ui.input(|i| i.time);
        let events = ui.input(|i| i.events.clone());
        for event in events {
            match event {
                Event::Text(text) => {
                    if text.chars().any(|c| !c.is_control()) {
                        self.begin(EditKind::Typing, now);
                        self.insert_text(&text);
                    }
                }
                Event::Ime(egui::ImeEvent::Commit(text)) => {
                    self.preedit = None;
                    if !text.is_empty() {
                        self.begin(EditKind::Typing, now);
                        self.insert_text(&text);
                    }
                }
                Event::Ime(egui::ImeEvent::Preedit { text, .. }) => {
                    self.preedit = (!text.is_empty()).then_some(text);
                }
                Event::Copy => {
                    if let Some((plain, paras)) = self.copy_selection() {
                        ui.ctx().copy_text(plain.clone());
                        self.clip = Some((plain, paras));
                    }
                }
                Event::Cut => {
                    if let Some((plain, paras)) = self.copy_selection() {
                        ui.ctx().copy_text(plain.clone());
                        self.clip = Some((plain, paras));
                        self.begin(EditKind::Other, now);
                        self.delete_selection();
                    }
                }
                Event::Paste(text) => {
                    self.begin(EditKind::Other, now);
                    self.paste(&text);
                }
                Event::Key { key, pressed: true, modifiers, .. } => self.key(key, modifiers, now),
                _ => {}
            }
        }
    }

    fn key(&mut self, key: Key, mods: Modifiers, now: f64) {
        let extend = mods.shift;
        let word = mods.command || mods.alt;
        if key != Key::ArrowUp && key != Key::ArrowDown {
            self.h_pos = None;
        }
        match key {
            Key::ArrowLeft | Key::ArrowRight => {
                let forward = key == Key::ArrowRight;
                if self.has_selection() && !extend {
                    let (a, b) = self.selection();
                    self.move_to(if forward { b } else { a }, false);
                    return;
                }
                let Pos { para, col } = self.cursor;
                let target = if word {
                    let text = doc::plain(&self.line(para).spans);
                    let at = doc::word_boundary(&text, col, forward);
                    if at != col { Pos::new(para, at) } else { self.step(forward) }
                } else {
                    self.step(forward)
                };
                self.move_to(target, extend);
            }
            Key::ArrowUp | Key::ArrowDown => {
                let target = self.vertical(key == Key::ArrowDown);
                let h = self.h_pos;
                self.move_to(target, extend);
                self.h_pos = h;
            }
            Key::Home | Key::End => {
                let end = key == Key::End;
                let target = if mods.command {
                    if end { Pos::new(self.paras.len() - 1, usize::MAX) } else { Pos::new(0, 0) }
                } else {
                    self.row_edge(end)
                };
                self.move_to(target, extend);
            }
            Key::Enter => {
                self.begin(EditKind::Other, now);
                self.delete_selection();
                self.split(true);
                self.scroll_to_cursor = true;
            }
            Key::Backspace => {
                self.begin(EditKind::Deleting, now);
                self.backspace(word);
                self.typing = None;
            }
            Key::Delete => {
                self.begin(EditKind::Deleting, now);
                self.delete_forward(word);
                self.typing = None;
            }
            Key::Tab => {
                let is_list = self.selected_paras().iter().any(|&i| matches!(self.line(i).block, Block::Item { .. }));
                self.begin(EditKind::Other, now);
                if is_list {
                    self.indent(if mods.shift { -1 } else { 1 });
                } else if !mods.shift {
                    self.insert_text("\t");
                }
            }
            Key::A if mods.command => self.select_all(),
            Key::Z if mods.command && mods.shift => self.redo(),
            Key::Z if mods.command => self.undo(),
            Key::Y if mods.command => self.redo(),
            _ => {}
        }
    }

    /// One character left or right, across paragraphs.
    fn step(&self, forward: bool) -> Pos {
        let Pos { para, col } = self.cursor;
        if forward {
            if col < self.line_len(para) {
                Pos::new(para, col + 1)
            } else {
                self.next_text(para).map_or(self.cursor, |p| Pos::new(p, 0))
            }
        } else if col > 0 {
            Pos::new(para, col - 1)
        } else {
            self.prev_text(para).map_or(self.cursor, |p| Pos::new(p, self.line_len(p)))
        }
    }

    /// One row up or down, keeping the x position, across paragraphs.
    fn vertical(&mut self, down: bool) -> Pos {
        let Pos { para, col } = self.cursor;
        let Some(Some(placed)) = self.placed.get(para).cloned() else {
            return self.step(down);
        };
        let galley = &placed.galley;
        let caret = galley.pos_from_cursor(CCursor::new(col));
        let x = *self.h_pos.get_or_insert(caret.center().x);
        let first_row = galley.rows.first().is_none_or(|r| caret.center().y < r.rect().max.y);
        let last_row = galley.rows.last().is_none_or(|r| caret.center().y > r.rect().min.y);
        if down && !last_row {
            return Pos::new(para, galley.cursor_from_pos(vec2(x, caret.max.y + 1.0)).index.0);
        }
        if !down && !first_row {
            return Pos::new(para, galley.cursor_from_pos(vec2(x, caret.min.y - 1.0)).index.0);
        }
        let target = if down { self.next_text(para) } else { self.prev_text(para) };
        let Some(target) = target else {
            return if down { Pos::new(para, self.line_len(para)) } else { Pos::new(para, 0) };
        };
        match self.placed.get(target).cloned().flatten() {
            Some(other) => {
                let y = if down { 1.0 } else { other.galley.size().y - 1.0 };
                Pos::new(target, other.galley.cursor_from_pos(vec2(x, y)).index.0)
            }
            None => Pos::new(target, 0),
        }
    }

    /// The start or end of the cursor's visual row.
    fn row_edge(&self, end: bool) -> Pos {
        let Pos { para, col } = self.cursor;
        match self.placed.get(para).cloned().flatten() {
            Some(placed) => {
                let c = CCursor::new(col);
                let edge =
                    if end { placed.galley.cursor_end_of_row(&c) } else { placed.galley.cursor_begin_of_row(&c) };
                Pos::new(para, edge.index.0)
            }
            None => Pos::new(para, if end { self.line_len(para) } else { 0 }),
        }
    }

    /// The text position under a point on screen, if the point is level
    /// with or below the first text paragraph.
    fn pos_at(&self, point: Pos2) -> Option<Pos> {
        let mut best = None;
        for (i, placed) in self.placed.iter().enumerate() {
            let Some(placed) = placed else { continue };
            if point.y < placed.rect.min.y && best.is_some() {
                break;
            }
            let local = point - placed.origin;
            best = Some(Pos::new(i, placed.galley.cursor_from_pos(local).index.0));
            if point.y <= placed.rect.max.y {
                break;
            }
        }
        best
    }

    /// Cut, Copy and Paste for menus: the window turns them into the same
    /// events the keyboard shortcuts give, read next frame while focused.
    pub fn clipboard(&self, ctx: &egui::Context, action: Clipboard) {
        ctx.send_viewport_cmd(match action {
            Clipboard::Cut => egui::ViewportCommand::RequestCut,
            Clipboard::Copy => egui::ViewportCommand::RequestCopy,
            Clipboard::Paste => egui::ViewportCommand::RequestPaste,
        });
        self.focus(ctx);
    }

    fn menu_action(&mut self, ctx: &egui::Context, action: MenuAction, now: f64) {
        match action {
            MenuAction::Clipboard(c) => self.clipboard(ctx, c),
            MenuAction::Inline(style) => self.toggle_inline(style, now),
            MenuAction::CodeBlock => self.set_block(BlockStyle::Code, now),
            MenuAction::InsertTable => self.insert_table(now),
            MenuAction::SelectAll => self.select_all(),
        }
    }

    // -----------------------------------------------------------------------
    // Drawing
    // -----------------------------------------------------------------------

    /// Draws and edits the document. Returns whether it changed.
    pub fn show(&mut self, ui: &mut egui::Ui) -> bool {
        let before = self.edits;
        let now = ui.input(|i| i.time);
        let focused = ui.memory(|m| m.has_focus(self.id));
        if focused {
            ui.memory_mut(|m| {
                m.set_focus_lock_filter(
                    self.id,
                    EventFilter { tab: true, horizontal_arrows: true, vertical_arrows: true, escape: false },
                )
            });
            self.handle_events(ui);
        }
        self.cursor = self.fix(self.cursor);
        self.anchor = self.fix(self.anchor);

        let width = ui.available_width();
        let numbers = self.ordered_numbers();
        let (sel_a, sel_b) = self.selection();
        let visuals = ui.visuals().clone();
        let mut placed: Vec<Option<Placed>> = vec![None; self.paras.len()];
        let mut presses: Vec<(usize, egui::Response)> = Vec::new();
        let mut tasks: Vec<usize> = Vec::new();
        let mut embed_edits: Vec<(usize, Para)> = Vec::new();
        let mut embed_delete = None;
        let top = ui.cursor().min;

        for i in 0..self.paras.len() {
            if let Para::Text(line) = &self.paras[i] {
                let look = Look::of(&line.block);
                ui.add_space(look.before);
                let inset = look.inset;
                let preedit = (focused && i == self.cursor.para).then(|| self.preedit.clone()).flatten();
                let job = layout_job(line, &look, &visuals, width - inset, preedit.map(|p| (self.cursor.col, p)));
                let galley = ui.fonts_mut(|f| f.layout_job(job));
                let (rect, resp) = ui.allocate_exact_size(vec2(width, galley.size().y), Sense::click_and_drag());
                let origin = rect.min + vec2(inset, 0.0);
                let painter = ui.painter();

                if (sel_a.para..=sel_b.para).contains(&i) && sel_a != sel_b {
                    let len = doc::len(&line.spans);
                    let start = if i == sel_a.para { sel_a.col } else { 0 };
                    let end = if i == sel_b.para { sel_b.col } else { len };
                    paint_selection(painter, &galley, origin, start, end, i != sel_b.para, visuals.selection.bg_fill);
                }
                if let Some(rect) = self.paint_marker(ui, &line.block, numbers[i], rect, origin, &galley, &visuals)
                    && ui.interact(rect, self.id.with(("task", i)), Sense::click()).clicked()
                {
                    tasks.push(i);
                }
                ui.painter().galley(origin, galley.clone(), look.color(&visuals));
                placed[i] = Some(Placed { galley, origin, rect });
                presses.push((i, resp));
                ui.add_space(look.after);
            } else {
                ui.add_space(6.0);
                let original = self.paras[i].clone();
                let response = match &mut self.paras[i] {
                    Para::Table(table) => embeds::table(ui, table, self.id.with(("table", i))),
                    Para::Code { lang, code } => embeds::code(ui, lang, code, code_id(self.id, i)),
                    Para::Image(image) => embeds::image(ui, image, self.dir.as_deref(), self.id.with(("image", i))),
                    Para::Text(_) => unreachable!(),
                };
                if response.changed {
                    embed_edits.push((i, original));
                }
                if response.delete {
                    embed_delete = Some(i);
                }
                ui.add_space(6.0);
            }
        }

        // The empty space below the text puts the cursor at the end.
        let rest = ui.available_size_before_wrap();
        let (below, below_resp) = ui.allocate_exact_size(vec2(width, rest.y.max(40.0)), Sense::click());
        let area = Rect::from_min_max(top, below.max);
        ui.interact(area, self.id, Sense::focusable_noninteractive());
        self.placed = placed;

        // Mouse.
        let pointer = ui.input(|i| i.pointer.interact_pos());
        let shift = ui.input(|i| i.modifiers.shift);
        let command = ui.input(|i| i.modifiers.command);
        let has_selection = self.has_selection();
        let mut menu = None;
        for (_, resp) in &presses {
            resp.context_menu(|ui| context_menu(ui, has_selection, &mut menu));
        }
        below_resp.context_menu(|ui| context_menu(ui, has_selection, &mut menu));
        for (i, resp) in &presses {
            if resp.secondary_clicked() {
                // Right-clicking outside the selection moves the cursor there.
                if let Some(pos) = pointer.and_then(|p| self.pos_at(p)) {
                    let (a, b) = self.selection();
                    if pos < a || pos > b || a == b {
                        self.move_to(pos, false);
                    }
                }
                continue;
            }
            if resp.double_clicked() {
                let Pos { col, .. } = self.pos_at(pointer.unwrap_or_default()).unwrap_or(self.cursor);
                let range = doc::word_at(&doc::plain(&self.line(*i).spans), col);
                self.anchor = Pos::new(*i, range.start);
                self.cursor = Pos::new(*i, range.end);
                self.dragging = false;
            } else if resp.triple_clicked() {
                self.anchor = Pos::new(*i, 0);
                self.cursor = Pos::new(*i, self.line_len(*i));
                self.dragging = false;
            } else if resp.is_pointer_button_down_on() && ui.input(|inp| inp.pointer.primary_pressed()) {
                if let Some(pos) = pointer.and_then(|p| self.pos_at(p)) {
                    if command && let Some(url) = doc::link_at(&self.line(pos.para).spans, pos.col) {
                        ui.ctx().open_url(egui::OpenUrl::new_tab(url));
                    } else {
                        self.move_to(pos, shift);
                        self.dragging = true;
                    }
                }
                self.focus(ui.ctx());
            }
        }
        if self.dragging {
            if ui.input(|i| i.pointer.primary_down()) {
                if let Some(pos) = pointer.and_then(|p| self.pos_at(p)) {
                    self.cursor = pos;
                    self.scroll_to_cursor = true;
                }
            } else {
                self.dragging = false;
            }
        }
        if below_resp.clicked() {
            self.move_to(Pos::new(self.paras.len() - 1, usize::MAX), shift);
        }
        // Each row is a widget of its own, and on the frame a click ends
        // egui counts only the clicked widget as hovered, so it sees a click
        // on the text as a click outside the editor and takes its focus
        // away. Clicks in the editor and on its menu take it back.
        let clicked = |r: &egui::Response| r.clicked() || r.secondary_clicked();
        if presses.iter().any(|(_, r)| clicked(r)) || clicked(&below_resp) {
            self.focus(ui.ctx());
        }
        if let Some(action) = menu {
            self.menu_action(ui.ctx(), action, now);
            self.focus(ui.ctx());
        }
        // Last, so it wins over the focus a menu or toolbar click asks for.
        if let Some(i) = self.focus_code.take() {
            ui.memory_mut(|m| m.request_focus(code_text_id(self.id, i)));
        }
        for i in tasks {
            self.toggle_task(i, now);
        }

        // Embedded widgets: record their edits for undo, then deletions.
        for (i, original) in embed_edits {
            let mut snap = self.snapshot();
            snap.paras[i] = original;
            self.undo.push(snap);
            self.redo.clear();
            self.last_kind = None;
            self.edits += 1;
        }
        if let Some(i) = embed_delete {
            self.begin(EditKind::Other, now);
            self.paras.remove(i);
            doc::normalize(&mut self.paras);
            self.cursor = self.fix(Pos::new(i, 0));
            self.anchor = self.cursor;
        }

        // The caret, and where the IME should pop up.
        if ui.memory(|m| m.has_focus(self.id))
            && let Some(Some(placed)) = self.placed.get(self.cursor.para)
        {
            let caret = placed.galley.pos_from_cursor(CCursor::new(self.cursor.col)).translate(placed.origin.to_vec2());
            let caret = Rect::from_min_size(caret.min, vec2(2.0, caret.height()));
            ui.painter().rect_filled(caret, 0.0, visuals.text_cursor.stroke.color);
            ui.output_mut(|o| {
                o.ime = Some(egui::output::IMEOutput {
                    rect: area,
                    cursor_rect: caret,
                    should_interrupt_composition: false,
                })
            });
            if std::mem::take(&mut self.scroll_to_cursor) {
                ui.scroll_to_rect(caret.expand(24.0), None);
            }
        }

        let changed = self.edits != before;
        if changed {
            ui.ctx().request_repaint();
        }
        changed
    }

    /// The number shown for each ordered list item, counting consecutive
    /// items at each depth as the markdown writer does.
    fn ordered_numbers(&self) -> Vec<Option<u64>> {
        let mut out = Vec::with_capacity(self.paras.len());
        let mut counters: Vec<Option<u64>> = Vec::new();
        for para in &self.paras {
            match para.as_text().map(|l| &l.block) {
                Some(Block::Item { kind, depth }) => {
                    let depth = *depth as usize;
                    counters.truncate(depth + 1);
                    counters.resize(depth + 1, None);
                    match kind {
                        ListKind::Ordered(n) => {
                            let next = counters[depth].unwrap_or(*n);
                            counters[depth] = Some(next + 1);
                            out.push(Some(next));
                        }
                        _ => {
                            counters[depth] = None;
                            out.push(None);
                        }
                    }
                }
                _ => {
                    counters.clear();
                    out.push(None);
                }
            }
        }
        out
    }

    /// Bullets, numbers, checkboxes and quote bars. Returns a checkbox's
    /// rect, which toggles the task when clicked.
    #[allow(clippy::too_many_arguments)]
    fn paint_marker(
        &self,
        ui: &egui::Ui,
        block: &Block,
        number: Option<u64>,
        rect: Rect,
        origin: Pos2,
        galley: &Galley,
        visuals: &egui::Visuals,
    ) -> Option<Rect> {
        let painter = ui.painter();
        let first_row = galley.rows.first().map_or(Rect::from_min_size(Pos2::ZERO, vec2(0.0, BODY_SIZE)), |r| r.rect());
        let mid_y = origin.y + first_row.center().y;
        let color = visuals.text_color();
        match block {
            Block::Quote => {
                let bar = Rect::from_min_max(rect.min + vec2(3.0, 0.0), Pos2::new(rect.min.x + 6.0, rect.max.y));
                painter.rect_filled(bar, 1.5, visuals.weak_text_color());
                None
            }
            Block::Item { kind, depth } => {
                let x = origin.x - 14.0;
                match kind {
                    ListKind::Bullet => {
                        // Drawn rather than typed: a filled dot, a ring, then a square.
                        let c = Pos2::new(x, mid_y);
                        match depth % 3 {
                            0 => {
                                painter.circle_filled(c, 2.6, color);
                            }
                            1 => {
                                painter.circle_stroke(c, 2.6, Stroke::new(1.1, color));
                            }
                            _ => {
                                painter.rect_filled(Rect::from_center_size(c, Vec2::splat(4.6)), 0.0, color);
                            }
                        }
                        None
                    }
                    ListKind::Ordered(_) => {
                        let text = format!("{}.", number.unwrap_or(1));
                        painter.text(
                            Pos2::new(origin.x - 6.0, mid_y),
                            egui::Align2::RIGHT_CENTER,
                            text,
                            fonts::note(BODY_SIZE, false),
                            color,
                        );
                        None
                    }
                    ListKind::Task(done) => {
                        let boxed = Rect::from_center_size(Pos2::new(x - 2.0, mid_y), Vec2::splat(14.0));
                        let stroke =
                            Stroke::new(1.2, if *done { visuals.selection.bg_fill } else { visuals.weak_text_color() });
                        if *done {
                            painter.rect_filled(boxed, 3.0, visuals.selection.bg_fill);
                            let c = boxed.center();
                            let tick = vec![c + vec2(-3.5, 0.0), c + vec2(-1.0, 3.0), c + vec2(4.0, -3.0)];
                            painter.add(egui::Shape::line(tick, Stroke::new(1.8, visuals.selection.stroke.color)));
                        } else {
                            painter.rect_stroke(boxed, 3.0, stroke, egui::StrokeKind::Inside);
                        }
                        Some(boxed.expand(3.0))
                    }
                }
            }
            _ => None,
        }
    }
}

fn block_matches(block: &Block, style: BlockStyle) -> bool {
    match (block, style) {
        (Block::Paragraph, BlockStyle::Paragraph) | (Block::Quote, BlockStyle::Quote) => true,
        (Block::Heading(a), BlockStyle::Heading(b)) => *a == b,
        (Block::Item { kind: ListKind::Bullet, .. }, BlockStyle::Bullet)
        | (Block::Item { kind: ListKind::Ordered(_), .. }, BlockStyle::Ordered)
        | (Block::Item { kind: ListKind::Task(_), .. }, BlockStyle::Task) => true,
        _ => false,
    }
}

/// How a paragraph style looks: text size and weight, spacing and inset.
struct Look {
    size: f32,
    bold: bool,
    mono: bool,
    weak: bool,
    strong: bool,
    done: bool,
    before: f32,
    after: f32,
    inset: f32,
}

impl Look {
    fn of(block: &Block) -> Self {
        let body = Look {
            size: BODY_SIZE,
            bold: false,
            mono: false,
            weak: false,
            strong: false,
            done: false,
            before: 0.0,
            after: 8.0,
            inset: 0.0,
        };
        match block {
            Block::Heading(1) => Look { size: 26.0, bold: true, strong: true, before: 6.0, after: 8.0, ..body },
            Block::Heading(2) => Look { size: 21.0, bold: true, strong: true, before: 6.0, after: 6.0, ..body },
            Block::Heading(_) => Look { size: 17.0, bold: true, strong: true, before: 4.0, after: 4.0, ..body },
            Block::Quote => Look { inset: 18.0, after: 4.0, ..body },
            Block::Item { kind, depth } => Look {
                inset: INDENT * (*depth as f32 + 1.0) + if matches!(kind, ListKind::Task(_)) { 4.0 } else { 0.0 },
                after: 3.0,
                done: matches!(kind, ListKind::Task(true)),
                ..body
            },
            Block::Raw => Look { size: 13.0, mono: true, weak: true, after: 2.0, ..body },
            _ => body,
        }
    }

    fn color(&self, v: &egui::Visuals) -> Color32 {
        if self.weak || self.done {
            v.weak_text_color()
        } else if self.strong {
            v.strong_text_color()
        } else {
            v.text_color()
        }
    }
}

/// The text format for a span's styles in a paragraph of this look.
fn format(styles: &[Inline], look: &Look, v: &egui::Visuals) -> TextFormat {
    let color = look.color(v);
    let bold = look.bold || styles.contains(&Inline::Bold);
    let italic = styles.contains(&Inline::Italic);
    let mut f = if look.mono || styles.iter().any(|s| matches!(s, Inline::Code | Inline::Raw)) {
        TextFormat { font_id: egui::FontId::monospace(look.size - 1.5), color, ..Default::default() }
    } else {
        TextFormat { font_id: fonts::note(look.size, italic), coords: fonts::weight(bold), color, ..Default::default() }
    };
    if styles.contains(&Inline::Code) {
        f.background = v.code_bg_color;
    }
    if styles.contains(&Inline::Raw) {
        f.color = v.weak_text_color();
    }
    if styles.contains(&Inline::Strike) || look.done {
        f.strikethrough = Stroke::new(1.0, f.color);
    }
    if styles.iter().any(|s| matches!(s, Inline::Link(_))) {
        f.color = v.hyperlink_color;
        f.underline = Stroke::new(1.0, v.hyperlink_color);
    }
    f
}

/// Lays out a paragraph's spans, with an IME composition shown underlined
/// at its column.
fn layout_job(line: &Line, look: &Look, v: &egui::Visuals, width: f32, preedit: Option<(usize, String)>) -> LayoutJob {
    let mut job = LayoutJob::default();
    job.wrap.max_width = width.max(20.0);
    let mut spans: Vec<Span> = line.spans.clone();
    let mut preedit_at = None;
    if let Some((col, text)) = preedit {
        let tail = doc::split_off(&mut spans, col);
        preedit_at = Some((spans.len(), text));
        spans.extend(tail);
    }
    if spans.is_empty() && preedit_at.is_none() {
        job.append("", 0.0, format(&[], look, v));
    }
    for (i, span) in spans.iter().enumerate() {
        if let Some((at, text)) = &preedit_at
            && *at == i
        {
            let mut f = format(&span.styles, look, v);
            f.underline = Stroke::new(1.0, f.color);
            job.append(text, 0.0, f);
        }
        job.append(&span.text, 0.0, format(&span.styles, look, v));
    }
    if let Some((at, text)) = &preedit_at
        && *at == spans.len()
    {
        let mut f = format(&[], look, v);
        f.underline = Stroke::new(1.0, f.color);
        job.append(text, 0.0, f);
    }
    job
}

/// Highlights characters `start..end` of a galley, plus a sliver past the
/// end of the last row when the selection carries on to the next paragraph.
fn paint_selection(
    painter: &egui::Painter,
    galley: &Galley,
    origin: Pos2,
    start: usize,
    end: usize,
    past_end: bool,
    color: Color32,
) {
    let mut row_start = 0;
    let rows = galley.rows.len();
    for (r, row) in galley.rows.iter().enumerate() {
        let count = row.char_count_including_newline().0;
        let row_end = row_start + row.row.char_count_excluding_newline().0;
        let s = start.max(row_start);
        let e = end.min(row_end);
        let carries = past_end && r == rows - 1;
        if s < e || (carries && end >= row_end && start <= row_end) {
            let x0 = row.pos.x + row.row.x_offset(egui::text::CharIndex(s.min(row_end) - row_start));
            let mut x1 = row.pos.x + row.row.x_offset(egui::text::CharIndex(e.max(s).min(row_end) - row_start));
            if carries || (end > row_end && r < rows - 1) {
                x1 += 6.0;
            }
            let rect = row.rect();
            let sel =
                Rect::from_x_y_ranges(origin.x + x0..=origin.x + x1, origin.y + rect.min.y..=origin.y + rect.max.y);
            painter.rect_filled(sel, 0.0, color);
        }
        row_start += count;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs one frame of the editor in a 600×800 window with `events`.
    fn frame(ctx: &egui::Context, editor: &mut Editor, t: &mut f64, events: Vec<Event>) {
        *t += 0.1;
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(600.0, 800.0))),
            time: Some(*t),
            events,
            ..Default::default()
        };
        let _ = ctx.run_ui(input, |ui| {
            egui::CentralPanel::default().show(ui, |ui| {
                editor.show(ui);
            });
        });
    }

    fn key(key: Key, modifiers: Modifiers) -> Event {
        Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers }
    }

    fn type_text(text: &str) -> Event {
        Event::Text(text.into())
    }

    /// An editor holding `md`, focused, with a frame drawn so it has a layout.
    fn setup(md: &str) -> (egui::Context, Editor, f64) {
        let ctx = egui::Context::default();
        crate::fonts::install(&ctx);
        let mut editor = Editor::new(Id::new("editor"));
        editor.load(md);
        let mut t = 0.0;
        frame(&ctx, &mut editor, &mut t, vec![]);
        editor.focus(&ctx);
        frame(&ctx, &mut editor, &mut t, vec![]);
        (ctx, editor, t)
    }

    #[test]
    fn typing_and_enter_continue_a_list_and_leave_it_on_an_empty_item() {
        let (ctx, mut ed, mut t) = setup("- one\n");
        ed.cursor_to_first_line_end();
        frame(&ctx, &mut ed, &mut t, vec![key(Key::Enter, Modifiers::NONE), type_text("two")]);
        assert_eq!(ed.markdown(), "- one\n- two\n");
        frame(
            &ctx,
            &mut ed,
            &mut t,
            vec![key(Key::Enter, Modifiers::NONE), key(Key::Enter, Modifiers::NONE), type_text("after")],
        );
        assert_eq!(ed.markdown(), "- one\n- two\n\nafter\n");
    }

    #[test]
    fn bold_while_typing_and_over_a_selection() {
        let (ctx, mut ed, mut t) = setup("");
        frame(&ctx, &mut ed, &mut t, vec![type_text("plain ")]);
        ed.toggle_inline(Inline::Bold, t);
        frame(&ctx, &mut ed, &mut t, vec![type_text("bold")]);
        assert_eq!(ed.markdown(), "plain **bold**\n");
        ed.select_all();
        ed.toggle_inline(Inline::Italic, t);
        assert_eq!(ed.markdown(), "*plain **bold***\n");
    }

    #[test]
    fn backspace_at_the_start_removes_styles_then_joins_lines() {
        let (ctx, mut ed, mut t) = setup("first\n\n## Second\n");
        ed.move_to(Pos::new(1, 0), false);
        frame(&ctx, &mut ed, &mut t, vec![key(Key::Backspace, Modifiers::NONE)]);
        assert_eq!(ed.markdown(), "first\n\nSecond\n");
        frame(&ctx, &mut ed, &mut t, vec![key(Key::Backspace, Modifiers::NONE)]);
        assert_eq!(ed.markdown(), "firstSecond\n");
    }

    #[test]
    fn enter_at_the_start_of_a_heading_pushes_it_down() {
        let (ctx, mut ed, mut t) = setup("# Title\n");
        frame(&ctx, &mut ed, &mut t, vec![key(Key::Enter, Modifiers::NONE)]);
        frame(&ctx, &mut ed, &mut t, vec![key(Key::ArrowUp, Modifiers::NONE), type_text("above")]);
        assert_eq!(ed.markdown(), "above\n\n# Title\n");
    }

    #[test]
    fn undo_restores_and_redo_reapplies() {
        let (ctx, mut ed, mut t) = setup("start\n");
        ed.cursor_to_first_line_end();
        frame(&ctx, &mut ed, &mut t, vec![type_text(" more")]);
        t += 2.0;
        frame(&ctx, &mut ed, &mut t, vec![key(Key::Enter, Modifiers::NONE)]);
        frame(&ctx, &mut ed, &mut t, vec![key(Key::Z, Modifiers::COMMAND)]);
        assert_eq!(ed.markdown(), "start more\n");
        frame(&ctx, &mut ed, &mut t, vec![key(Key::Z, Modifiers::COMMAND)]);
        assert_eq!(ed.markdown(), "start\n");
        frame(&ctx, &mut ed, &mut t, vec![key(Key::Z, Modifiers::COMMAND | Modifiers::SHIFT)]);
        assert_eq!(ed.markdown(), "start more\n");
    }

    #[test]
    fn copy_and_paste_keep_formatting_inside_the_app() {
        let (ctx, mut ed, mut t) = setup("**bold** text\n\n- item\n");
        ed.select_all();
        frame(&ctx, &mut ed, &mut t, vec![Event::Copy]);
        let plain = ed.clip.as_ref().unwrap().0.clone();
        assert_eq!(plain, "bold text\nitem");
        ed.move_to(Pos::new(ed.paras.len() - 1, usize::MAX), false);
        frame(
            &ctx,
            &mut ed,
            &mut t,
            vec![key(Key::Enter, Modifiers::NONE), key(Key::Enter, Modifiers::NONE), Event::Paste(plain)],
        );
        assert_eq!(ed.markdown(), "**bold** text\n\n- item\n\n**bold** text\n\n- item\n");
    }

    #[test]
    fn pasting_outside_text_is_plain() {
        let (ctx, mut ed, mut t) = setup("");
        frame(&ctx, &mut ed, &mut t, vec![Event::Paste("a *b*\nc".into())]);
        assert_eq!(ed.markdown(), "a \\*b\\*\n\nc\n");
    }

    #[test]
    fn tab_indents_list_items() {
        let (ctx, mut ed, mut t) = setup("- a\n- b\n");
        ed.move_to(Pos::new(1, 1), false);
        frame(&ctx, &mut ed, &mut t, vec![key(Key::Tab, Modifiers::NONE)]);
        assert_eq!(ed.markdown(), "- a\n  - b\n");
        frame(&ctx, &mut ed, &mut t, vec![key(Key::Tab, Modifiers::SHIFT)]);
        assert_eq!(ed.markdown(), "- a\n- b\n");
    }

    #[test]
    fn block_styles_toggle() {
        let (_ctx, mut ed, t) = setup("line\n");
        ed.set_block(BlockStyle::Heading(2), t);
        assert_eq!(ed.markdown(), "## line\n");
        ed.set_block(BlockStyle::Heading(2), t);
        assert_eq!(ed.markdown(), "line\n");
        ed.set_block(BlockStyle::Task, t);
        assert_eq!(ed.markdown(), "- [ ] line\n");
        ed.set_block(BlockStyle::Code, t);
        assert_eq!(ed.markdown(), "```\nline\n```\n");
    }

    #[test]
    fn selecting_across_lines_and_deleting_joins_them() {
        let (ctx, mut ed, mut t) = setup("abc\n\ndef\n");
        ed.move_to(Pos::new(0, 1), false);
        ed.move_to(Pos::new(1, 2), true);
        frame(&ctx, &mut ed, &mut t, vec![key(Key::Delete, Modifiers::NONE)]);
        assert_eq!(ed.markdown(), "af\n");
    }

    #[test]
    fn unsupported_markdown_survives_editing_elsewhere() {
        let src = "<div>html</div>\n\nText\n";
        let (ctx, mut ed, mut t) = setup(src);
        ed.move_to(Pos::new(ed.paras.len() - 1, usize::MAX), false);
        frame(&ctx, &mut ed, &mut t, vec![type_text("!")]);
        assert_eq!(ed.markdown(), "<div>html</div>\n\nText!\n");
    }

    fn press(at: Pos2, pressed: bool, button: egui::PointerButton) -> Event {
        Event::PointerButton { pos: at, button, pressed, modifiers: Modifiers::NONE }
    }

    /// Clicks at `at` with a frame for the press and one for the release.
    fn click(ctx: &egui::Context, ed: &mut Editor, t: &mut f64, at: Pos2) {
        frame(ctx, ed, t, vec![Event::PointerMoved(at), press(at, true, egui::PointerButton::Primary)]);
        frame(ctx, ed, t, vec![press(at, false, egui::PointerButton::Primary)]);
    }

    /// Where character `col` of paragraph `para` was drawn, mid-row.
    fn screen_pos(ed: &Editor, para: usize, col: usize) -> Pos2 {
        let placed = ed.placed[para].as_ref().unwrap();
        placed.galley.pos_from_cursor(CCursor::new(col)).center() + placed.origin.to_vec2()
    }

    /// An editor that has never had focus, as when a note was just opened.
    fn unfocused(md: &str) -> (egui::Context, Editor, f64) {
        let ctx = egui::Context::default();
        crate::fonts::install(&ctx);
        let mut editor = Editor::new(Id::new("editor"));
        editor.load(md);
        let mut t = 0.0;
        frame(&ctx, &mut editor, &mut t, vec![]);
        (ctx, editor, t)
    }

    #[test]
    fn clicking_into_text_and_typing_types_there() {
        let (ctx, mut ed, mut t) = unfocused("hello world\n");
        let at = screen_pos(&ed, 0, 5);
        click(&ctx, &mut ed, &mut t, at);
        frame(&ctx, &mut ed, &mut t, vec![type_text(",")]);
        assert_eq!(ed.markdown(), "hello, world\n");
    }

    #[test]
    fn double_clicking_a_word_selects_it_and_typing_replaces_it() {
        let (ctx, mut ed, mut t) = unfocused("hello world\n");
        let at = screen_pos(&ed, 0, 8);
        click(&ctx, &mut ed, &mut t, at);
        t += 0.05;
        click(&ctx, &mut ed, &mut t, at);
        assert_eq!(ed.selection(), (Pos::new(0, 6), Pos::new(0, 11)));
        frame(&ctx, &mut ed, &mut t, vec![type_text("there")]);
        assert_eq!(ed.markdown(), "hello there\n");
    }

    #[test]
    fn dragging_a_selection_and_typing_replaces_it() {
        let (ctx, mut ed, mut t) = unfocused("hello world\n");
        let (from, to) = (screen_pos(&ed, 0, 0), screen_pos(&ed, 0, 5));
        frame(&ctx, &mut ed, &mut t, vec![Event::PointerMoved(from), press(from, true, egui::PointerButton::Primary)]);
        frame(&ctx, &mut ed, &mut t, vec![Event::PointerMoved(to)]);
        frame(&ctx, &mut ed, &mut t, vec![press(to, false, egui::PointerButton::Primary)]);
        frame(&ctx, &mut ed, &mut t, vec![type_text("goodbye")]);
        assert_eq!(ed.markdown(), "goodbye world\n");
    }

    fn right_click(ctx: &egui::Context, ed: &mut Editor, t: &mut f64, at: Pos2) {
        frame(ctx, ed, t, vec![Event::PointerMoved(at), press(at, true, egui::PointerButton::Secondary)]);
        frame(ctx, ed, t, vec![press(at, false, egui::PointerButton::Secondary)]);
        frame(ctx, ed, t, vec![]);
    }

    #[test]
    fn right_click_opens_a_menu_and_keeps_a_selection_it_lands_in() {
        let (ctx, mut ed, mut t) = unfocused("hello world\n");
        ed.move_to(Pos::new(0, 6), false);
        ed.move_to(Pos::new(0, 11), true);
        let at = screen_pos(&ed, 0, 8);
        right_click(&ctx, &mut ed, &mut t, at);
        assert!(egui::Popup::is_any_open(&ctx), "the context menu is open");
        assert_eq!(ed.selection(), (Pos::new(0, 6), Pos::new(0, 11)));
        assert!(ctx.memory(|m| m.has_focus(ed.id)), "the editor has the keyboard");
    }

    #[test]
    fn right_click_outside_the_selection_moves_the_cursor() {
        let (ctx, mut ed, mut t) = unfocused("hello world\n");
        ed.move_to(Pos::new(0, 0), false);
        ed.move_to(Pos::new(0, 3), true);
        let at = screen_pos(&ed, 0, 8);
        right_click(&ctx, &mut ed, &mut t, at);
        assert!(!ed.has_selection());
        assert_eq!(ed.cursor.para, 0);
        assert!((7..=9).contains(&ed.cursor.col), "{:?}", ed.cursor);
    }

    #[test]
    fn making_a_code_block_puts_the_keyboard_in_it() {
        let (ctx, mut ed, mut t) = setup("first\n\nlet x = 1;\n");
        ed.move_to(Pos::new(1, 3), false);
        ed.set_block(BlockStyle::Code, t);
        frame(&ctx, &mut ed, &mut t, vec![]);
        frame(&ctx, &mut ed, &mut t, vec![]);
        assert_eq!(ed.markdown(), "first\n\n```\nlet x = 1;\n```\n");
        assert!(ctx.memory(|m| m.has_focus(code_text_id(ed.id, 1))), "the code block has the keyboard");
        frame(&ctx, &mut ed, &mut t, vec![type_text(" // one")]);
        assert_eq!(ed.markdown(), "first\n\n```\nlet x = 1; // one\n```\n");
    }
}

