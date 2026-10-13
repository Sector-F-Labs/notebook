//! Conversion between markdown text and the line-based document model the
//! rich editor works with.
//!
//! The rich editor only understands a subset of markdown (paragraphs,
//! headings, quotes, code blocks, tables, images on a line of their own,
//! simple lists and basic inline styles). Anything else (HTML, images
//! inside text, footnotes, front matter, complex lists) is kept as `Raw`
//! text and written back verbatim, so opening and saving a file never
//! drops content.

use pulldown_cmark::{Alignment, CodeBlockKind, Event, Options, Parser, Tag, TagEnd};
use std::ops::Range;

pub const MAX_DEPTH: u8 = 5;

#[derive(Debug, Clone, PartialEq)]
pub enum ListKind {
    Bullet,
    Ordered(u64),
    Task(bool),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Block {
    Paragraph,
    Heading(u8),
    Quote,
    Code(String),
    Raw,
    Item { kind: ListKind, depth: u8 },
    Table(Table),
    Image(Image),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Align {
    None,
    Left,
    Center,
    Right,
}

/// A GFM table. Cells hold their inline markdown source (with `\|`
/// unescaped), and every row has `align.len()` cells. Row 0 is the header.
#[derive(Debug, Clone, PartialEq)]
pub struct Table {
    pub align: Vec<Align>,
    pub rows: Vec<Vec<String>>,
}

impl Table {
    pub fn new(columns: usize, body_rows: usize) -> Self {
        Table { align: vec![Align::None; columns], rows: vec![vec![String::new(); columns]; body_rows + 1] }
    }

    pub fn columns(&self) -> usize {
        self.align.len()
    }
}

/// An image alone in its paragraph: `![alt](src "title")`. `src` is as
/// written: a path relative to the note, an absolute path or a URL.
#[derive(Debug, Clone, PartialEq)]
pub struct Image {
    pub alt: String,
    pub src: String,
    pub title: String,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Inline {
    // Declaration order is the nesting order used when writing markdown:
    // links outermost, code and raw are atomic.
    Link(String),
    Bold,
    Italic,
    Strike,
    Code,
    Raw,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Span {
    pub text: String,
    pub styles: Vec<Inline>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Line {
    pub block: Block,
    pub spans: Vec<Span>,
}

impl Line {
    fn new(block: Block) -> Self {
        Line { block, spans: Vec::new() }
    }

    pub fn is_empty(&self) -> bool {
        self.spans.iter().all(|s| s.text.is_empty())
    }
}

// ---------------------------------------------------------------------------
// markdown -> lines
// ---------------------------------------------------------------------------

fn options() -> Options {
    Options::ENABLE_TABLES
        | Options::ENABLE_FOOTNOTES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_YAML_STYLE_METADATA_BLOCKS
}

type Events<'a> = Vec<(Event<'a>, Range<usize>)>;

/// Index of the `End` event matching the `Start` at `start`.
fn matching_end(events: &Events, start: usize) -> usize {
    let mut depth = 0;
    for (i, (event, _)) in events.iter().enumerate().skip(start) {
        match event {
            Event::Start(_) => depth += 1,
            Event::End(_) => {
                depth -= 1;
                if depth == 0 {
                    return i;
                }
            }
            _ => {}
        }
    }
    events.len() - 1
}

fn is_inline_tag(tag: &Tag) -> bool {
    matches!(tag, Tag::Emphasis | Tag::Strong | Tag::Strikethrough | Tag::Link { .. } | Tag::Image { .. })
}

/// Read a table starting at `start` (a `Start(Table)` event).
fn parse_table(events: &Events, start: usize, src: &str) -> Table {
    let align = match &events[start].0 {
        Event::Start(Tag::Table(align)) => align
            .iter()
            .map(|a| match a {
                Alignment::None => Align::None,
                Alignment::Left => Align::Left,
                Alignment::Center => Align::Center,
                Alignment::Right => Align::Right,
            })
            .collect(),
        _ => Vec::new(),
    };
    let mut rows: Vec<Vec<String>> = Vec::new();
    for (event, range) in &events[start..matching_end(events, start)] {
        match event {
            Event::Start(Tag::TableHead | Tag::TableRow) => rows.push(Vec::new()),
            Event::Start(Tag::TableCell) => {
                let cell = src[range.clone()].trim().replace("\\|", "|");
                if let Some(row) = rows.last_mut() {
                    row.push(cell);
                }
            }
            _ => {}
        }
    }
    let mut table = Table { align, rows };
    let columns = table.columns();
    for row in &mut table.rows {
        row.resize(columns, String::new());
    }
    table
}

fn table_markdown(table: &Table) -> String {
    let cell = |text: &str| text.replace('\n', " ").replace('|', "\\|");
    let row = |cells: &[String]| {
        let cells: Vec<String> = cells.iter().map(|c| format!(" {} ", cell(c))).collect();
        format!("|{}|", cells.join("|"))
    };
    let separator: Vec<&str> = table
        .align
        .iter()
        .map(|a| match a {
            Align::None => " --- ",
            Align::Left => " :-- ",
            Align::Center => " :-: ",
            Align::Right => " --: ",
        })
        .collect();

    let mut lines = vec![row(&table.rows[0]), format!("|{}|", separator.join("|"))];
    lines.extend(table.rows[1..].iter().map(|r| row(r)));
    lines.join("\n")
}

/// The image filling the paragraph that starts at `start`, if that's all
/// it holds, and the index of the paragraph's end.
fn lone_image(events: &Events, start: usize) -> Option<(Image, usize)> {
    let Some((Event::Start(Tag::Image { dest_url, title, .. }), _)) = events.get(start + 1) else {
        return None;
    };
    let end = matching_end(events, start + 1);
    if !matches!(events.get(end + 1), Some((Event::End(TagEnd::Paragraph), _))) {
        return None;
    }
    // The description as plain text: any styling in it is dropped.
    let alt = events[start + 2..end]
        .iter()
        .filter_map(|(e, _)| match e {
            Event::Text(t) | Event::Code(t) => Some(t.as_ref()),
            Event::SoftBreak | Event::HardBreak => Some(" "),
            _ => None,
        })
        .collect();
    Some((Image { alt, src: dest_url.to_string(), title: title.to_string() }, end + 1))
}

fn image_markdown(image: &Image) -> String {
    let title = match image.title.as_str() {
        "" => String::new(),
        t => format!(" \"{}\"", t.replace('"', "\\\"")),
    };
    format!("![{}]({}{title})", escape(&image.alt), link_dest(&image.src))
}

/// A quote the rich editor can represent: paragraphs only.
fn quote_is_simple(events: &Events, start: usize) -> bool {
    let end = matching_end(events, start);
    events[start + 1..end].iter().all(|(e, _)| match e {
        Event::Start(tag) => matches!(tag, Tag::Paragraph) || is_inline_tag(tag),
        _ => true,
    })
}

/// A list the rich editor can represent: every item holds at most one
/// paragraph plus nested lists, and nesting stays within `MAX_DEPTH`.
fn list_is_simple(events: &Events, start: usize) -> bool {
    let end = matching_end(events, start);
    let mut paragraphs: Vec<u32> = Vec::new();
    let mut lists = 0u8;
    for (event, _) in &events[start..end] {
        match event {
            Event::Start(Tag::List(_)) => {
                lists += 1;
                if lists > MAX_DEPTH + 1 {
                    return false;
                }
            }
            Event::End(TagEnd::List(_)) => lists -= 1,
            Event::Start(Tag::Item) => paragraphs.push(0),
            Event::End(TagEnd::Item) => {
                paragraphs.pop();
            }
            Event::Start(Tag::Paragraph) => match paragraphs.last_mut() {
                Some(n) if *n == 0 => *n += 1,
                _ => return false,
            },
            Event::Start(tag) if !is_inline_tag(tag) => return false,
            _ => {}
        }
    }
    true
}

struct Importer<'a> {
    src: &'a str,
    lines: Vec<Line>,
    current: Option<Line>,
    styles: Vec<Inline>,
    // One entry per open list: the next number for ordered lists.
    lists: Vec<Option<u64>>,
    quotes: u32,
    last_was_raw: bool,
}

impl<'a> Importer<'a> {
    fn context_block(&self) -> Block {
        if self.quotes > 0 { Block::Quote } else { Block::Paragraph }
    }

    fn start_line(&mut self, block: Block) {
        self.finish_line();
        self.current = Some(Line::new(block));
    }

    fn finish_line(&mut self) {
        if let Some(line) = self.current.take() {
            self.push(line);
        }
    }

    fn push(&mut self, line: Line) {
        self.last_was_raw = line.block == Block::Raw;
        self.lines.push(line);
    }

    fn text(&mut self, text: &str, extra: Option<Inline>) {
        if self.current.is_none() {
            self.current = Some(Line::new(self.context_block()));
        }
        let mut styles = self.styles.clone();
        styles.extend(extra);
        styles.sort();
        styles.dedup();
        let line = self.current.as_mut().unwrap();
        match line.spans.last_mut() {
            Some(last) if last.styles == styles => last.text.push_str(text),
            _ => line.spans.push(Span { text: text.to_string(), styles }),
        }
    }

    fn raw_block(&mut self, range: Range<usize>) {
        self.finish_line();
        let text = self.src[range].trim_end_matches(['\n', '\r']);
        if self.last_was_raw {
            // Keep the blank line that separated two raw blocks.
            self.push(Line::new(Block::Raw));
        }
        for l in text.split('\n') {
            let mut line = Line::new(Block::Raw);
            line.spans.push(Span { text: l.trim_end_matches('\r').to_string(), styles: vec![] });
            self.push(line);
        }
    }
}

pub fn parse(src: &str) -> Vec<Line> {
    let events: Events = Parser::new_ext(src, options()).into_offset_iter().collect();
    let mut im = Importer {
        src,
        lines: Vec::new(),
        current: None,
        styles: Vec::new(),
        lists: Vec::new(),
        quotes: 0,
        last_was_raw: false,
    };

    let mut i = 0;
    while i < events.len() {
        let (event, range) = &events[i];
        match event {
            Event::Start(tag) => match tag {
                Tag::Paragraph if im.lists.is_empty() && im.quotes == 0 && lone_image(&events, i).is_some() => {
                    let (image, end) = lone_image(&events, i).expect("checked above");
                    im.finish_line();
                    im.push(Line::new(Block::Image(image)));
                    i = end;
                }
                Tag::Paragraph => {
                    let in_fresh_item = matches!(
                        &im.current,
                        Some(Line { block: Block::Item { .. }, spans }) if spans.is_empty()
                    );
                    if !in_fresh_item {
                        im.start_line(im.context_block());
                    }
                }
                Tag::Heading { level, .. } => im.start_line(Block::Heading(*level as u8)),
                Tag::BlockQuote(_) => {
                    if im.lists.is_empty() && quote_is_simple(&events, i) {
                        im.quotes += 1;
                    } else {
                        let end = matching_end(&events, i);
                        im.raw_block(range.clone());
                        i = end;
                    }
                }
                Tag::CodeBlock(kind) => {
                    let end = matching_end(&events, i);
                    if im.lists.is_empty() && im.quotes == 0 {
                        im.finish_line();
                        let lang = match kind {
                            CodeBlockKind::Fenced(lang) => lang.to_string(),
                            CodeBlockKind::Indented => String::new(),
                        };
                        let mut code = String::new();
                        for (e, _) in &events[i + 1..end] {
                            if let Event::Text(t) = e {
                                code.push_str(t);
                            }
                        }
                        let code = code.strip_suffix('\n').unwrap_or(&code);
                        for l in code.split('\n') {
                            let mut line = Line::new(Block::Code(lang.clone()));
                            line.spans.push(Span { text: l.to_string(), styles: vec![] });
                            im.push(line);
                        }
                    } else {
                        im.raw_block(range.clone());
                    }
                    i = end;
                }
                Tag::List(start) => {
                    if im.lists.is_empty() && !list_is_simple(&events, i) {
                        let end = matching_end(&events, i);
                        im.raw_block(range.clone());
                        i = end;
                    } else {
                        im.finish_line();
                        im.lists.push(*start);
                    }
                }
                Tag::Item => {
                    let depth = (im.lists.len() - 1) as u8;
                    let kind = match im.lists.last_mut() {
                        Some(Some(n)) => {
                            *n += 1;
                            ListKind::Ordered(*n - 1)
                        }
                        _ => ListKind::Bullet,
                    };
                    im.start_line(Block::Item { kind, depth });
                }
                Tag::Emphasis => im.styles.push(Inline::Italic),
                Tag::Strong => im.styles.push(Inline::Bold),
                Tag::Strikethrough => im.styles.push(Inline::Strike),
                Tag::Link { dest_url, .. } => im.styles.push(Inline::Link(dest_url.to_string())),
                Tag::Image { .. } => {
                    let end = matching_end(&events, i);
                    im.text(&src[range.clone()], Some(Inline::Raw));
                    i = end;
                }
                Tag::Table(_) if im.lists.is_empty() && im.quotes == 0 => {
                    im.finish_line();
                    im.push(Line::new(Block::Table(parse_table(&events, i, src))));
                    i = matching_end(&events, i);
                }
                _ => {
                    // HTML blocks, footnote definitions, front matter...
                    let end = matching_end(&events, i);
                    im.raw_block(range.clone());
                    i = end;
                }
            },
            Event::End(tag) => match tag {
                TagEnd::Paragraph => {
                    if !matches!(&im.current, Some(Line { block: Block::Item { .. }, .. })) {
                        im.finish_line();
                    }
                }
                TagEnd::Heading(_) | TagEnd::Item => im.finish_line(),
                TagEnd::BlockQuote(_) => {
                    im.finish_line();
                    im.quotes -= 1;
                }
                TagEnd::List(_) => {
                    im.finish_line();
                    im.lists.pop();
                }
                TagEnd::Emphasis | TagEnd::Strong | TagEnd::Strikethrough | TagEnd::Link => {
                    im.styles.pop();
                }
                _ => {}
            },
            Event::Text(t) => im.text(t, None),
            Event::Code(t) => im.text(t, Some(Inline::Code)),
            Event::SoftBreak => im.text(" ", None),
            Event::HardBreak => {
                let block = match &im.current {
                    Some(Line { block: b @ (Block::Paragraph | Block::Quote), .. }) => Some(b.clone()),
                    _ => None,
                };
                match block {
                    Some(b) => im.start_line(b),
                    None => im.text(" ", None),
                }
            }
            Event::TaskListMarker(checked) => {
                if let Some(Line { block: Block::Item { kind, .. }, .. }) = &mut im.current {
                    *kind = ListKind::Task(*checked);
                }
            }
            Event::Rule => im.raw_block(range.clone()),
            Event::Html(_) | Event::InlineHtml(_) | Event::FootnoteReference(_) => {
                im.text(&src[range.clone()], Some(Inline::Raw))
            }
            Event::InlineMath(_) | Event::DisplayMath(_) => im.text(&src[range.clone()], Some(Inline::Raw)),
        }
        i += 1;
    }
    im.finish_line();

    if im.lines.is_empty() {
        im.lines.push(Line::new(Block::Paragraph));
    }
    im.lines
}

// ---------------------------------------------------------------------------
// lines -> markdown
// ---------------------------------------------------------------------------

fn escape(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    for (i, &c) in chars.iter().enumerate() {
        let prev = i.checked_sub(1).map(|p| chars[p]);
        let next = chars.get(i + 1).copied();
        let needs = match c {
            '\\' | '*' | '`' | '[' | ']' | '~' => true,
            '_' => !(prev.is_some_and(char::is_alphanumeric) && next.is_some_and(char::is_alphanumeric)),
            '<' => next.is_some_and(|n| n.is_ascii_alphabetic() || matches!(n, '/' | '!' | '?')),
            '&' => next.is_some_and(|n| n.is_ascii_alphanumeric() || n == '#'),
            _ => false,
        };
        if needs {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Escape text that would otherwise be read as the start of a block.
fn escape_line_start(text: &str) -> String {
    let text = text.trim_start();
    let first = text.chars().next();
    let second = text.chars().nth(1);
    let space_or_end = second.is_none_or(char::is_whitespace);
    let all_rule = text.len() >= 3
        && text.chars().all(|c| matches!(c, '-' | '*' | '_' | ' '))
        && text.chars().filter(|c| !c.is_whitespace()).collect::<std::collections::HashSet<_>>().len() == 1;

    match first {
        Some('#') if space_or_end || text.trim_end().chars().all(|c| c == '#') => format!("\\{text}"),
        Some('>') => format!("\\{text}"),
        Some('-' | '+') if space_or_end || all_rule => format!("\\{text}"),
        _ if text.starts_with("\\*") && all_rule => text.to_string(),
        _ => {
            // "1. foo" / "1) foo" would become an ordered list.
            let digits = text.chars().take_while(char::is_ascii_digit).count();
            let after = text[digits..].chars().next();
            let after2 = text[digits..].chars().nth(1);
            if (1..=9).contains(&digits) && matches!(after, Some('.' | ')')) && after2.is_none_or(char::is_whitespace) {
                format!("{}\\{}", &text[..digits], &text[digits..])
            } else {
                text.to_string()
            }
        }
    }
}

fn code_span(text: &str) -> String {
    let longest = text.split(|c| c != '`').map(str::len).max().unwrap_or(0);
    let fence = "`".repeat(longest + 1);
    let pad = text.starts_with('`') || text.ends_with('`');
    if pad { format!("{fence} {text} {fence}") } else { format!("{fence}{text}{fence}") }
}

fn link_dest(url: &str) -> String {
    if url.contains([' ', '(', ')']) { format!("<{url}>") } else { url.to_string() }
}

fn opener(style: &Inline) -> &'static str {
    match style {
        Inline::Link(_) => "[",
        Inline::Bold => "**",
        Inline::Italic => "*",
        Inline::Strike => "~~",
        Inline::Code | Inline::Raw => "",
    }
}

fn closer(style: &Inline) -> String {
    match style {
        Inline::Link(url) => format!("]({})", link_dest(url)),
        other => opener(other).to_string(),
    }
}

/// Close styles without leaving whitespace just inside the closing marker,
/// which CommonMark would not recognise as the end of emphasis.
fn close_into(out: &mut String, styles: &[Inline]) {
    if styles.is_empty() {
        return;
    }
    let trimmed = out.trim_end_matches([' ', '\t']).len();
    let trailing = out.split_off(trimmed);
    for style in styles.iter().rev() {
        out.push_str(&closer(style));
    }
    out.push_str(&trailing);
}

pub fn inline_markdown(spans: &[Span]) -> String {
    let mut out = String::new();
    let mut open: Vec<Inline> = Vec::new();

    for span in spans.iter().filter(|s| !s.text.is_empty()) {
        let wanted: Vec<Inline> =
            span.styles.iter().filter(|s| !matches!(s, Inline::Code | Inline::Raw)).cloned().collect();

        let keep = open.iter().take_while(|s| wanted.contains(s)).count();
        let closing = open.split_off(keep);
        close_into(&mut out, &closing);

        let mut text = span.text.as_str();
        let opening: Vec<Inline> = wanted.into_iter().filter(|s| !open.contains(s)).collect();
        if !opening.is_empty() {
            let lead = text.len() - text.trim_start().len();
            out.push_str(&text[..lead]);
            text = &text[lead..];
            for style in &opening {
                out.push_str(opener(style));
            }
            open.extend(opening);
        }

        if span.styles.contains(&Inline::Raw) {
            out.push_str(text);
        } else if span.styles.contains(&Inline::Code) {
            out.push_str(&code_span(text));
        } else {
            out.push_str(&escape(text));
        }
    }
    close_into(&mut out, &open);
    out
}

pub fn to_markdown(lines: &[Line]) -> String {
    let mut blocks: Vec<String> = Vec::new();
    let mut i = 0;

    while i < lines.len() {
        let line = &lines[i];
        match &line.block {
            Block::Raw => {
                let mut text = Vec::new();
                while i < lines.len() && lines[i].block == Block::Raw {
                    let raw: String = lines[i].spans.iter().map(|s| s.text.as_str()).collect();
                    text.push(raw);
                    i += 1;
                }
                while text.last().is_some_and(|l| l.trim().is_empty()) {
                    text.pop();
                }
                if !text.is_empty() {
                    blocks.push(text.join("\n"));
                }
                continue;
            }
            Block::Code(lang) => {
                let mut body = Vec::new();
                while i < lines.len() && lines[i].block == line.block {
                    body.push(lines[i].spans.iter().map(|s| s.text.as_str()).collect::<String>());
                    i += 1;
                }
                let body = body.join("\n");
                let longest = body.split(|c| c != '`').map(str::len).max().unwrap_or(0);
                let fence = "`".repeat(longest.max(2) + 1);
                blocks.push(format!("{fence}{lang}\n{body}\n{fence}"));
                continue;
            }
            Block::Quote => {
                let mut parts = Vec::new();
                while i < lines.len() && lines[i].block == Block::Quote {
                    if !lines[i].is_empty() {
                        let text = escape_line_start(&inline_markdown(&lines[i].spans));
                        parts.push(format!("> {text}"));
                    }
                    i += 1;
                }
                if !parts.is_empty() {
                    blocks.push(parts.join("\n>\n"));
                }
                continue;
            }
            Block::Item { .. } => {
                let mut out = Vec::new();
                // Content indent and next ordered number per open depth.
                let mut widths: Vec<usize> = Vec::new();
                let mut numbers: Vec<Option<u64>> = Vec::new();
                let family = |k: &ListKind| std::mem::discriminant(k);
                let mut top: Option<std::mem::Discriminant<ListKind>> = None;
                while let Some(Line { block: Block::Item { kind, depth }, spans }) = lines.get(i) {
                    let depth = (*depth as usize).min(widths.len());
                    if depth == 0 {
                        // A different kind of top-level item starts a new list.
                        if top.is_some_and(|t| t != family(kind)) {
                            break;
                        }
                        top = Some(family(kind));
                    }
                    widths.truncate(depth);
                    numbers.truncate(depth + 1);
                    let marker = match kind {
                        ListKind::Bullet => "- ".to_string(),
                        ListKind::Task(done) => format!("- [{}] ", if *done { 'x' } else { ' ' }),
                        ListKind::Ordered(n) => {
                            let n = match numbers.get(depth) {
                                Some(Some(next)) => *next,
                                _ => *n,
                            };
                            numbers.resize(depth + 1, None);
                            numbers[depth] = Some(n + 1);
                            format!("{n}. ")
                        }
                    };
                    if !matches!(kind, ListKind::Ordered(_)) && numbers.len() > depth {
                        numbers[depth] = None;
                    }
                    let indent: usize = widths.iter().sum();
                    let content = if matches!(kind, ListKind::Task(_)) { 2 } else { marker.len() };
                    let text = inline_markdown(spans);
                    out.push(format!("{}{}{}", " ".repeat(indent), marker, text.trim_start()).trim_end().to_string());
                    widths.push(content);
                    i += 1;
                }
                blocks.push(out.join("\n"));
                continue;
            }
            Block::Table(table) => {
                if !table.rows.is_empty() && table.columns() > 0 {
                    blocks.push(table_markdown(table));
                }
            }
            Block::Image(image) => blocks.push(image_markdown(image)),
            Block::Heading(level) => {
                let text = inline_markdown(&line.spans);
                blocks.push(format!("{} {}", "#".repeat(*level as usize), text.trim()));
            }
            Block::Paragraph => {
                if !line.is_empty() {
                    blocks.push(escape_line_start(&inline_markdown(&line.spans)));
                }
            }
        }
        i += 1;
    }

    let mut md = blocks.join("\n\n");
    md.push('\n');
    md
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(src: &str) -> String {
        to_markdown(&parse(src))
    }

    #[test]
    fn an_image_on_its_own_line_is_an_image_block() {
        let lines = parse("Before\n\n![A cat](pics/cat%20one.png \"Tabby\")\n\nAfter\n");
        assert_eq!(
            lines[1].block,
            Block::Image(Image { alt: "A cat".into(), src: "pics/cat%20one.png".into(), title: "Tabby".into() })
        );
        assert_eq!(roundtrip("![A cat](pics/cat.png \"Tabby\")\n"), "![A cat](pics/cat.png \"Tabby\")\n");
        assert_eq!(roundtrip("![a \\[b\\]](<x y.png>)\n"), "![a \\[b\\]](<x y.png>)\n");
        assert_eq!(roundtrip("![](x.png)\n"), "![](x.png)\n");
    }

    #[test]
    fn images_inside_text_or_lists_stay_raw() {
        for src in ["Look: ![cat](cat.png) here\n", "- ![cat](cat.png)\n", "> ![cat](cat.png)\n"] {
            assert!(!parse(src).iter().any(|l| matches!(l.block, Block::Image(_))), "{src}");
            assert_eq!(roundtrip(src), src);
        }
    }

    #[test]
    fn simple_document_is_stable() {
        let src = "# Title\n\nSome **bold** and *italic* and `code` and ~~gone~~.\n\n\
                   > a quote\n\n- one\n- two\n  - nested\n\n1. first\n2. second\n\n\
                   - [ ] todo\n- [x] done\n\n```rust\nfn main() {}\n```\n\n[link](https://example.com)\n";
        assert_eq!(roundtrip(src), src);
    }

    #[test]
    fn unsupported_blocks_are_kept_verbatim() {
        let src = "---\ntitle: x\n---\n\n<div>hi</div>\n\n![img](a.png) text\n";
        assert_eq!(roundtrip(src), src);
    }

    #[test]
    fn tables_roundtrip() {
        let src = "| A | **B** |\n| :-- | --: |\n| 1 | x \\| y |\n|  | z |\n";
        let lines = parse(src);
        assert_eq!(
            lines,
            vec![Line::new(Block::Table(Table {
                align: vec![Align::Left, Align::Right],
                rows: vec![
                    vec!["A".into(), "**B**".into()],
                    vec!["1".into(), "x | y".into()],
                    vec!["".into(), "z".into()],
                ],
            }))]
        );
        assert_eq!(to_markdown(&lines), src);
    }

    #[test]
    fn plain_text_is_escaped() {
        let lines = vec![Line {
            block: Block::Paragraph,
            spans: vec![Span { text: "# not a heading *really*".into(), styles: vec![] }],
        }];
        let md = to_markdown(&lines);
        assert_eq!(parse(&md), lines);
    }

    #[test]
    fn emphasis_moves_trailing_space_outside() {
        let spans = vec![
            Span { text: "bold ".into(), styles: vec![Inline::Bold] },
            Span { text: "plain".into(), styles: vec![] },
        ];
        assert_eq!(inline_markdown(&spans), "**bold** plain");
    }
}
