//! The document the editor works on: the markdown model's lines, with code
//! blocks gathered into one paragraph each and tables and images kept whole, plus the
//! text operations the editor needs on a line's styled spans.
//!
//! Positions are counted in characters, not bytes, within a text paragraph.

use std::ops::Range;

use crate::markdown::{Block, Image, Inline, Line, Span, Table};

/// One paragraph of the document.
#[derive(Debug, Clone, PartialEq)]
pub enum Para {
    /// A paragraph, heading, quote line, list item or raw markdown line.
    Text(Line),
    Table(Table),
    Code {
        lang: String,
        code: String,
    },
    Image(Image),
}

impl Para {
    pub fn text(block: Block) -> Self {
        Para::Text(Line { block, spans: Vec::new() })
    }

    pub fn as_text(&self) -> Option<&Line> {
        match self {
            Para::Text(line) => Some(line),
            _ => None,
        }
    }

    pub fn as_text_mut(&mut self) -> Option<&mut Line> {
        match self {
            Para::Text(line) => Some(line),
            _ => None,
        }
    }
}

/// Where the cursor is: a character offset within a text paragraph.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub struct Pos {
    pub para: usize,
    pub col: usize,
}

impl Pos {
    pub fn new(para: usize, col: usize) -> Self {
        Self { para, col }
    }
}

/// Gathers consecutive code lines of one language into a code paragraph.
pub fn from_lines(lines: Vec<Line>) -> Vec<Para> {
    let mut paras: Vec<Para> = Vec::new();
    for line in lines {
        match line.block {
            Block::Code(lang) => {
                let text = plain(&line.spans);
                match paras.last_mut() {
                    Some(Para::Code { lang: l, code }) if *l == lang => {
                        code.push('\n');
                        code.push_str(&text);
                    }
                    _ => paras.push(Para::Code { lang, code: text }),
                }
            }
            Block::Table(table) => paras.push(Para::Table(table)),
            Block::Image(image) => paras.push(Para::Image(image)),
            _ => paras.push(Para::Text(line)),
        }
    }
    normalize(&mut paras);
    paras
}

/// Back to the markdown model's lines.
pub fn to_lines(paras: &[Para]) -> Vec<Line> {
    let mut lines = Vec::new();
    for para in paras {
        match para {
            Para::Text(line) => lines.push(line.clone()),
            Para::Table(table) => lines.push(Line { block: Block::Table(table.clone()), spans: Vec::new() }),
            Para::Image(image) => lines.push(Line { block: Block::Image(image.clone()), spans: Vec::new() }),
            Para::Code { lang, code } => {
                for l in code.split('\n') {
                    lines.push(Line {
                        block: Block::Code(lang.clone()),
                        spans: vec![Span { text: l.to_string(), styles: Vec::new() }],
                    });
                }
            }
        }
    }
    lines
}

/// Keeps the document editable: never empty, and ending in a text
/// paragraph so there's always somewhere to type after a table or code.
pub fn normalize(paras: &mut Vec<Para>) {
    if !matches!(paras.last(), Some(Para::Text(_))) {
        paras.push(Para::text(Block::Paragraph));
    }
}

// ---------------------------------------------------------------------------
// Text operations on a line's spans
// ---------------------------------------------------------------------------

pub fn plain(spans: &[Span]) -> String {
    spans.iter().map(|s| s.text.as_str()).collect()
}

pub fn len(spans: &[Span]) -> usize {
    spans.iter().map(|s| s.text.chars().count()).sum()
}

fn byte_at(text: &str, col: usize) -> usize {
    text.char_indices().nth(col).map_or(text.len(), |(i, _)| i)
}

/// Merges neighbours with the same styles and drops empty spans.
fn tidy(spans: &mut Vec<Span>) {
    let mut out: Vec<Span> = Vec::with_capacity(spans.len());
    for mut span in spans.drain(..) {
        if span.text.is_empty() {
            continue;
        }
        span.styles.sort();
        span.styles.dedup();
        match out.last_mut() {
            Some(last) if last.styles == span.styles => last.text.push_str(&span.text),
            _ => out.push(span),
        }
    }
    *spans = out;
}

/// Splits so a span boundary falls at `col`; returns the index of the span
/// starting there (or `spans.len()` at the end).
fn split_at(spans: &mut Vec<Span>, col: usize) -> usize {
    let mut start = 0;
    for i in 0..spans.len() {
        let n = spans[i].text.chars().count();
        if col == start {
            return i;
        }
        if col < start + n {
            let at = byte_at(&spans[i].text, col - start);
            let tail = spans[i].text.split_off(at);
            let styles = spans[i].styles.clone();
            spans.insert(i + 1, Span { text: tail, styles });
            return i + 1;
        }
        start += n;
    }
    spans.len()
}

/// The styles text typed at `col` takes: those of the character before it,
/// or at the start of the line, of the first character. Links and raw
/// markdown don't extend to new text.
pub fn styles_at(spans: &[Span], col: usize) -> Vec<Inline> {
    let mut start = 0;
    let mut found = spans.first().map(|s| s.styles.clone()).unwrap_or_default();
    for span in spans {
        let n = span.text.chars().count();
        if col > start && col <= start + n {
            found = span.styles.clone();
            break;
        }
        start += n;
    }
    found.retain(|s| !matches!(s, Inline::Link(_) | Inline::Raw));
    found
}

pub fn insert(spans: &mut Vec<Span>, col: usize, text: &str, styles: &[Inline]) {
    let i = split_at(spans, col);
    spans.insert(i, Span { text: text.to_string(), styles: styles.to_vec() });
    tidy(spans);
}

pub fn delete(spans: &mut Vec<Span>, range: Range<usize>) {
    if range.is_empty() {
        return;
    }
    split_at(spans, range.end);
    split_at(spans, range.start);
    let mut col = 0;
    spans.retain(|s| {
        let n = s.text.chars().count();
        let keep = col + n <= range.start || col >= range.end;
        col += n;
        keep
    });
    tidy(spans);
}

/// Removes and returns everything from `col` on.
pub fn split_off(spans: &mut Vec<Span>, col: usize) -> Vec<Span> {
    let i = split_at(spans, col);
    let tail = spans.split_off(i);
    tidy(spans);
    tail
}

pub fn append(spans: &mut Vec<Span>, more: Vec<Span>) {
    spans.extend(more);
    tidy(spans);
}

/// A copy of the spans in `range`.
pub fn slice(spans: &[Span], range: Range<usize>) -> Vec<Span> {
    let mut copy = spans.to_vec();
    let mut tail = split_off(&mut copy, range.start);
    split_off(&mut tail, range.end - range.start);
    tail
}

/// Whether every character in `range` has `style`.
pub fn all_have(spans: &[Span], range: Range<usize>, style: &Inline) -> bool {
    let mut col = 0;
    let mut any = false;
    for span in spans {
        let n = span.text.chars().count();
        if col < range.end && col + n > range.start {
            any = true;
            if !span.styles.contains(style) {
                return false;
            }
        }
        col += n;
    }
    any
}

/// Adds or removes `style` over `range`.
pub fn set_style(spans: &mut Vec<Span>, range: Range<usize>, style: &Inline, on: bool) {
    if range.is_empty() {
        return;
    }
    split_at(spans, range.end);
    split_at(spans, range.start);
    let mut col = 0;
    for span in spans.iter_mut() {
        let n = span.text.chars().count();
        if col >= range.start && col + n <= range.end {
            span.styles.retain(|s| s != style);
            if on {
                span.styles.push(style.clone());
            }
        }
        col += n;
    }
    tidy(spans);
}

/// The link under `col`, if any.
pub fn link_at(spans: &[Span], col: usize) -> Option<String> {
    let mut start = 0;
    for span in spans {
        let n = span.text.chars().count();
        if col >= start && col < start + n {
            return span.styles.iter().find_map(|s| match s {
                Inline::Link(url) => Some(url.clone()),
                _ => None,
            });
        }
        start += n;
    }
    None
}

/// The word around `col`, as a character range.
pub fn word_at(text: &str, col: usize) -> Range<usize> {
    let chars: Vec<char> = text.chars().collect();
    let is_word = |c: char| c.is_alphanumeric() || c == '_';
    let col = col.min(chars.len());
    let on_word = chars.get(col).is_some_and(|c| is_word(*c)) || (col > 0 && is_word(chars[col - 1]));
    if !on_word {
        return col..(col + 1).min(chars.len());
    }
    let mut start = col;
    while start > 0 && is_word(chars[start - 1]) {
        start -= 1;
    }
    let mut end = col;
    while end < chars.len() && is_word(chars[end]) {
        end += 1;
    }
    start..end
}

/// Where Ctrl+Left / Ctrl+Right from `col` lands.
pub fn word_boundary(text: &str, col: usize, forward: bool) -> usize {
    let chars: Vec<char> = text.chars().collect();
    let is_word = |c: char| c.is_alphanumeric() || c == '_';
    let mut i = col.min(chars.len());
    if forward {
        while i < chars.len() && !is_word(chars[i]) {
            i += 1;
        }
        while i < chars.len() && is_word(chars[i]) {
            i += 1;
        }
    } else {
        while i > 0 && !is_word(chars[i - 1]) {
            i -= 1;
        }
        while i > 0 && is_word(chars[i - 1]) {
            i -= 1;
        }
    }
    i
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::markdown::{self, ListKind};

    fn span(text: &str, styles: &[Inline]) -> Span {
        Span { text: text.into(), styles: styles.to_vec() }
    }

    fn md(paras: &[Para]) -> String {
        markdown::to_markdown(&to_lines(paras))
    }

    #[test]
    fn code_lines_become_one_paragraph_and_back() {
        let src = "Intro\n\n```rust\nfn a() {}\n\nfn b() {}\n```\n";
        let paras = from_lines(markdown::parse(src));
        assert_eq!(paras[1], Para::Code { lang: "rust".into(), code: "fn a() {}\n\nfn b() {}".into() });
        assert!(matches!(paras.last(), Some(Para::Text(_))), "ends in a text paragraph");
        assert_eq!(md(&paras), src);
    }

    #[test]
    fn insert_takes_the_given_styles_and_merges() {
        let mut spans = vec![span("hello", &[Inline::Bold])];
        insert(&mut spans, 5, "!", &[Inline::Bold]);
        assert_eq!(spans, vec![span("hello!", &[Inline::Bold])]);
        insert(&mut spans, 0, ">", &[]);
        assert_eq!(spans, vec![span(">", &[]), span("hello!", &[Inline::Bold])]);
    }

    #[test]
    fn delete_across_spans() {
        let mut spans = vec![span("ab", &[]), span("cd", &[Inline::Bold]), span("ef", &[])];
        delete(&mut spans, 1..5);
        assert_eq!(spans, vec![span("af", &[])]);
    }

    #[test]
    fn delete_handles_multibyte_characters() {
        let mut spans = vec![span("héllo wörld", &[])];
        delete(&mut spans, 1..4);
        assert_eq!(plain(&spans), "ho wörld");
    }

    #[test]
    fn toggling_a_style_over_part_of_a_span() {
        let mut spans = vec![span("one two three", &[])];
        set_style(&mut spans, 4..7, &Inline::Bold, true);
        assert_eq!(spans, vec![span("one ", &[]), span("two", &[Inline::Bold]), span(" three", &[])]);
        assert!(all_have(&spans, 4..7, &Inline::Bold));
        assert!(!all_have(&spans, 3..7, &Inline::Bold));
        set_style(&mut spans, 0..13, &Inline::Bold, false);
        assert_eq!(spans, vec![span("one two three", &[])]);
    }

    #[test]
    fn typing_continues_the_previous_style_but_not_links() {
        let spans = vec![span("bold", &[Inline::Bold]), span("link", &[Inline::Link("u".into())])];
        assert_eq!(styles_at(&spans, 4), vec![Inline::Bold]);
        assert_eq!(styles_at(&spans, 0), vec![Inline::Bold]);
        assert!(styles_at(&spans, 8).is_empty());
    }

    #[test]
    fn split_and_slice() {
        let mut spans = vec![span("ab", &[]), span("cd", &[Inline::Italic])];
        assert_eq!(slice(&spans, 1..3), vec![span("b", &[]), span("c", &[Inline::Italic])]);
        let tail = split_off(&mut spans, 3);
        assert_eq!(spans, vec![span("ab", &[]), span("c", &[Inline::Italic])]);
        assert_eq!(tail, vec![span("d", &[Inline::Italic])]);
    }

    #[test]
    fn words() {
        assert_eq!(word_at("hello big world", 7), 6..9);
        assert_eq!(word_boundary("hello big world", 0, true), 5);
        assert_eq!(word_boundary("hello big world", 10, false), 6);
    }

    #[test]
    fn tasks_survive_the_round_trip() {
        let paras = from_lines(markdown::parse("- [ ] todo\n- [x] done\n"));
        assert!(matches!(&paras[0], Para::Text(Line { block: Block::Item { kind: ListKind::Task(false), .. }, .. })));
        assert_eq!(md(&paras), "- [ ] todo\n- [x] done\n");
    }
}
