//! Finds the parts of a markdown text the adapters care about, with byte ranges, lines and
//! columns: links and their targets, autolinks, `@token`s, code spans, fenced code blocks,
//! block-level HTML comments and bare path-like tokens. Callers ask which spans lie inside
//! code or comments.
//!
//! This is a scanner, not a full CommonMark parser. Indented code blocks are not code here,
//! because neither spec lists them as a skipped region (Claude 1.5, Gemini 3).

use std::ops::Range;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Span {
    pub range: Range<usize>,
    /// 1-based.
    pub line: u32,
    /// 1-based, in characters.
    pub col: u32,
}

/// A link or an autolink. `target` is the text between the parentheses or angle brackets,
/// without a title.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    pub span: Span,
    pub target: String,
    pub autolink: bool,
}

/// An `@token` or a path-like token. `text` is the token without its `@`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    pub span: Span,
    pub text: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Spans {
    /// Fenced code blocks, backtick and tilde, delimiter lines included.
    pub fences: Vec<Span>,
    /// Block-level HTML comments: `<!--` at the start of a line, up to `-->`.
    pub comments: Vec<Span>,
    /// Backtick spans: a run of N backticks up to the next run of N.
    pub code_spans: Vec<Span>,
    pub links: Vec<Link>,
    /// `@token`s as Claude Code reads them (spec 1.5).
    pub at_tokens: Vec<Token>,
    /// Bare tokens that look like paths: they contain `/` or end in `.md`.
    pub paths: Vec<Token>,
}

impl Spans {
    /// Whether `offset` lies inside a fence, a code span or a block comment.
    pub fn in_code_or_comment(&self, offset: usize) -> bool {
        self.fences
            .iter()
            .chain(&self.code_spans)
            .chain(&self.comments)
            .any(|s| s.range.contains(&offset))
    }
}

/// Maps byte offsets to lines and columns.
pub struct LineIndex<'a> {
    text: &'a str,
    starts: Vec<usize>,
}

impl<'a> LineIndex<'a> {
    pub fn new(text: &'a str) -> LineIndex<'a> {
        let mut starts = vec![0];
        starts.extend(text.match_indices('\n').map(|(i, _)| i + 1));
        LineIndex { text, starts }
    }

    pub fn span(&self, range: Range<usize>) -> Span {
        let line = self.starts.partition_point(|s| *s <= range.start) - 1;
        let col = self.text[self.starts[line]..range.start].chars().count();
        Span {
            line: line as u32 + 1,
            col: col as u32 + 1,
            range,
        }
    }
}

pub fn scan(text: &str) -> Spans {
    let index = LineIndex::new(text);
    let (fences, comments) = blocks(text, &index);
    let mut masked = mask(
        text,
        fences.iter().chain(&comments).map(|s| s.range.clone()),
    );
    let code_spans = code_spans(&masked, &index);
    mask_in_place(&mut masked, code_spans.iter().map(|s| s.range.clone()));
    let links = links(&masked, &index);
    let at = at_tokens(&masked, AtStyle::Claude, &index);
    let paths = path_tokens(&masked, &links, &index);
    Spans {
        fences,
        comments,
        code_spans,
        links,
        at_tokens: at,
        paths,
    }
}

/// Where a token may start and what ends it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AtStyle {
    /// After the start of the text or any whitespace. `\ ` is part of the token (spec 1.5).
    Claude,
    /// After the start of the text, a space, a tab, CR or LF. The token runs to the next
    /// whitespace (Gemini spec 3).
    Gemini,
}

/// Every `@token`. Callers decide which tokens are imports and which regions to skip.
pub fn at_tokens_of(text: &str, style: AtStyle) -> Vec<Token> {
    at_tokens(text, style, &LineIndex::new(text))
}

fn at_tokens(text: &str, style: AtStyle, index: &LineIndex) -> Vec<Token> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'@' {
            let boundary = i == 0
                || match style {
                    AtStyle::Claude => text[..i]
                        .chars()
                        .next_back()
                        .is_some_and(char::is_whitespace),
                    AtStyle::Gemini => matches!(bytes[i - 1], b' ' | b'\t' | b'\r' | b'\n'),
                };
            if boundary {
                let end = token_end(text, i + 1, style);
                if end > i + 1 {
                    out.push(Token {
                        span: index.span(i..end),
                        text: text[i + 1..end].to_owned(),
                    });
                    i = end;
                    continue;
                }
            }
        }
        i += 1;
    }
    out
}

fn token_end(text: &str, from: usize, style: AtStyle) -> usize {
    let mut chars = text[from..].char_indices().peekable();
    let mut end = from;
    while let Some((at, ch)) = chars.next() {
        if ch.is_whitespace() {
            break;
        }
        if ch == '\\' && style == AtStyle::Claude {
            if matches!(chars.peek(), Some((_, ' '))) {
                chars.next();
                end = from + at + 2;
                continue;
            }
            break;
        }
        end = from + at + ch.len_utf8();
    }
    end
}

/// Ranges covered by a run of N backticks up to the next run of N, anywhere in the text. This
/// is the region Gemini CLI protects from imports (Gemini spec 3). It covers inline code and
/// backtick fences, and not tilde fences.
pub fn backtick_regions(text: &str) -> Vec<Range<usize>> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'`' {
            i += 1;
            continue;
        }
        let n = run_len(bytes, i);
        match find_run(bytes, i + n, n, bytes.len()) {
            Some(close) => {
                out.push(i..close + n);
                i = close + n;
            }
            None => i += n,
        }
    }
    out
}

fn run_len(bytes: &[u8], at: usize) -> usize {
    bytes[at..].iter().take_while(|b| **b == b'`').count()
}

/// The start of the next run of exactly `n` backticks in `bytes[from..limit]`.
fn find_run(bytes: &[u8], from: usize, n: usize, limit: usize) -> Option<usize> {
    let mut j = from;
    while j < limit {
        if bytes[j] == b'`' {
            let len = run_len(&bytes[..limit], j);
            if len == n {
                return Some(j);
            }
            j += len;
        } else {
            j += 1;
        }
    }
    None
}

fn code_spans(masked: &str, index: &LineIndex) -> Vec<Span> {
    let bytes = masked.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'`' {
            i += 1;
            continue;
        }
        let n = run_len(bytes, i);
        let limit = paragraph_end(masked, i + n);
        match find_run(bytes, i + n, n, limit) {
            Some(close) => {
                out.push(index.span(i..close + n));
                i = close + n;
            }
            None => i += n,
        }
    }
    out
}

/// The offset of the newline that ends the paragraph holding `from`: the first one followed
/// by a blank line. The end of the text when there is none.
fn paragraph_end(text: &str, from: usize) -> usize {
    let mut pos = from;
    while let Some(nl) = text[pos..].find('\n') {
        let next = pos + nl + 1;
        let line_end = text[next..].find('\n').map_or(text.len(), |i| next + i);
        if text[next..line_end].trim().is_empty() {
            return pos + nl;
        }
        pos = next;
    }
    text.len()
}

fn mask(text: &str, ranges: impl Iterator<Item = Range<usize>>) -> String {
    let mut out = text.to_owned();
    mask_in_place(&mut out, ranges);
    out
}

/// Replaces every byte in the ranges with a space, except newlines, so offsets stay valid.
fn mask_in_place(text: &mut String, ranges: impl Iterator<Item = Range<usize>>) {
    let mut bytes = std::mem::take(text).into_bytes();
    for range in ranges {
        for b in &mut bytes[range] {
            if *b != b'\n' {
                *b = b' ';
            }
        }
    }
    *text = String::from_utf8(bytes).expect("masking replaces whole characters");
}

/// Fenced code blocks and block-level HTML comments.
fn blocks(text: &str, index: &LineIndex) -> (Vec<Span>, Vec<Span>) {
    let mut fences = Vec::new();
    let mut comments = Vec::new();
    let mut at = 0;
    let mut open: Option<(usize, u8, usize)> = None;
    while at < text.len() {
        let end = text[at..].find('\n').map_or(text.len(), |i| at + i + 1);
        let line = &text[at..end];
        let indent = line.len() - line.trim_start_matches(' ').len();
        let body = &line[indent..];
        if let Some((start, ch, len)) = open {
            let run = body.bytes().take_while(|b| *b == ch).count();
            if indent <= 3 && run >= len && body[run..].trim().is_empty() {
                fences.push(index.span(start..end));
                open = None;
            }
            at = end;
            continue;
        }
        if indent <= 3 {
            let ch = body.bytes().next().unwrap_or(0);
            if ch == b'`' || ch == b'~' {
                let run = body.bytes().take_while(|b| *b == ch).count();
                if run >= 3 && !(ch == b'`' && body[run..].contains('`')) {
                    open = Some((at, ch, run));
                    at = end;
                    continue;
                }
            }
            if body.starts_with("<!--") {
                let from = at + indent;
                let stop = text[from + 4..]
                    .find("-->")
                    .map_or(text.len(), |i| from + 4 + i + 3);
                comments.push(index.span(from..stop));
                // Text after `-->` on the same line is ordinary text, so continue there.
                at = if stop >= text.len() { text.len() } else { stop };
                // Move to the start of the next line only when the rest of this one is empty.
                if text[at..].starts_with('\n') {
                    at += 1;
                }
                continue;
            }
        }
        at = end;
    }
    if let Some((start, _, _)) = open {
        fences.push(index.span(start..text.len()));
    }
    (fences, comments)
}

fn links(masked: &str, index: &LineIndex) -> Vec<Link> {
    let bytes = masked.as_bytes();
    let mut out = Vec::new();
    // Inline links: `[text](target "title")`.
    let mut i = 0;
    while let Some(found) = masked[i..].find("](") {
        let close = i + found;
        i = close + 2;
        let Some(open) = matching_open(masked, close) else {
            continue;
        };
        let Some((target, end)) = inline_target(masked, close + 2) else {
            continue;
        };
        out.push(Link {
            span: index.span(open..end),
            target,
            autolink: false,
        });
        i = end;
    }
    // Reference definitions: `[label]: target`.
    let mut at = 0;
    while at < masked.len() {
        let end = masked[at..].find('\n').map_or(masked.len(), |j| at + j + 1);
        let line = masked[at..end].trim_end();
        let indent = line.len() - line.trim_start().len();
        if indent <= 3
            && let Some(rest) = line[indent..].strip_prefix('[')
            && let Some(label_end) = rest.find("]:")
            && !rest[..label_end].contains(']')
        {
            let after = &rest[label_end + 2..];
            let target = after.split_whitespace().next().unwrap_or("");
            let target = target.trim_matches(['<', '>']);
            if !target.is_empty() {
                out.push(Link {
                    span: index.span(at + indent..at + line.len()),
                    target: target.to_owned(),
                    autolink: false,
                });
            }
        }
        at = end;
    }
    // Autolinks: `<scheme:rest>` and `<user@host>`.
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'<'
            && let Some(len) = masked[i + 1..].find(['>', '<', ' ', '\n'])
            && masked.as_bytes()[i + 1 + len] == b'>'
        {
            let inner = &masked[i + 1..i + 1 + len];
            if is_autolink(inner) {
                out.push(Link {
                    span: index.span(i..i + len + 2),
                    target: inner.to_owned(),
                    autolink: true,
                });
                i += len + 2;
                continue;
            }
        }
        i += 1;
    }
    out.sort_by_key(|l| l.span.range.start);
    out
}

fn is_autolink(inner: &str) -> bool {
    if let Some((scheme, rest)) = inner.split_once(':') {
        return !rest.is_empty()
            && scheme
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_alphabetic())
            && scheme
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '.' | '-'));
    }
    inner
        .split_once('@')
        .is_some_and(|(user, host)| !user.is_empty() && host.contains('.'))
}

/// The `[` that matches the `]` at `close`, on the same paragraph.
fn matching_open(text: &str, close: usize) -> Option<usize> {
    let mut depth = 0usize;
    for (i, ch) in text[..close].char_indices().rev() {
        match ch {
            ']' => depth += 1,
            '[' if depth == 0 => return Some(i),
            '[' => depth -= 1,
            '\n' if text[..i].ends_with('\n') => return None,
            _ => {}
        }
    }
    None
}

/// Reads `target "title")` from `from`. Returns the target and the offset after `)`.
fn inline_target(text: &str, from: usize) -> Option<(String, usize)> {
    let rest = &text[from..];
    let trimmed = rest.trim_start_matches([' ', '\t', '\n']);
    let start = from + (rest.len() - trimmed.len());
    let (target, mut at) = if let Some(inner) = trimmed.strip_prefix('<') {
        let end = inner.find(['>', '\n'])?;
        if inner.as_bytes()[end] != b'>' {
            return None;
        }
        (inner[..end].to_owned(), start + 1 + end + 1)
    } else {
        let mut depth = 0usize;
        let mut end = trimmed.len();
        let mut chars = trimmed.char_indices();
        while let Some((i, ch)) = chars.next() {
            match ch {
                '\\' => {
                    chars.next();
                }
                '(' => depth += 1,
                ')' if depth == 0 => {
                    end = i;
                    break;
                }
                ')' => depth -= 1,
                c if c.is_whitespace() => {
                    end = i;
                    break;
                }
                _ => {}
            }
        }
        (trimmed[..end].replace("\\", ""), start + end)
    };
    // A title may follow. The link ends at the next `)` on the same line.
    let line_end = text[at..].find('\n').map_or(text.len(), |i| at + i);
    let close = text[at..line_end].find(')')?;
    at += close + 1;
    Some((target, at))
}

fn path_tokens(masked: &str, links: &[Link], index: &LineIndex) -> Vec<Token> {
    let mut out = Vec::new();
    let mut offset = 0;
    for piece in masked.split_inclusive(char::is_whitespace) {
        let start = offset;
        offset += piece.len();
        let word = piece.trim_end();
        let lead = word.len() - word.trim_start_matches(is_edge).len();
        let core = word[lead..].trim_end_matches(is_edge);
        if core.is_empty() {
            continue;
        }
        let from = start + lead;
        let range = from..from + core.len();
        if links
            .iter()
            .any(|l| l.span.range.start < range.end && range.start < l.span.range.end)
        {
            continue;
        }
        if looks_like_path(core) {
            out.push(Token {
                span: index.span(range),
                text: core.to_owned(),
            });
        }
    }
    out
}

fn is_edge(c: char) -> bool {
    matches!(
        c,
        '(' | ')' | '[' | ']' | '<' | '>' | '"' | '\'' | ',' | ';' | ':' | '.' | '!' | '?' | '*'
    )
}

fn looks_like_path(token: &str) -> bool {
    if token.contains("://") || token.starts_with('@') || token == "/" {
        return false;
    }
    let charset = token
        .chars()
        .all(|c| c.is_alphanumeric() || matches!(c, '.' | '_' | '-' | '/' | '~' | '+'));
    charset && (token.contains('/') || token.ends_with(".md"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(text: &str, spans: &[Span]) -> Vec<String> {
        spans
            .iter()
            .map(|s| text[s.range.clone()].to_owned())
            .collect()
    }

    #[test]
    fn links_report_target_line_and_column() {
        let text = "intro\nsee [guide](docs/guide.md \"title\") and ![img](a.png)\n";
        let spans = scan(text);
        let targets: Vec<_> = spans.links.iter().map(|l| l.target.as_str()).collect();
        assert_eq!(targets, ["docs/guide.md", "a.png"]);
        let first = &spans.links[0].span;
        assert_eq!((first.line, first.col), (2, 5));
        assert_eq!(
            &text[first.range.clone()],
            "[guide](docs/guide.md \"title\")"
        );
    }

    #[test]
    fn reference_definitions_and_autolinks_are_links() {
        let text = "[ref]: ./other.md\n\nvisit <https://example.com/x> or <me@host.dev>\n";
        let spans = scan(text);
        let found: Vec<_> = spans
            .links
            .iter()
            .map(|l| (l.target.as_str(), l.autolink))
            .collect();
        assert_eq!(
            found,
            [
                ("./other.md", false),
                ("https://example.com/x", true),
                ("me@host.dev", true)
            ]
        );
    }

    #[test]
    fn fences_cover_backtick_and_tilde_with_matching_closers() {
        let text = "a\n```rust\n@x.md\n~~~\n```\nb\n~~~~\n@y.md\n~~~~\nc\n";
        let spans = scan(text);
        assert_eq!(
            texts(text, &spans.fences),
            ["```rust\n@x.md\n~~~\n```\n", "~~~~\n@y.md\n~~~~\n"]
        );
        assert!(spans.at_tokens.is_empty());
    }

    #[test]
    fn an_unclosed_fence_runs_to_the_end() {
        let text = "```\ncode @a.md\n";
        assert_eq!(texts(text, &scan(text).fences), [text]);
    }

    #[test]
    fn block_comments_hide_tokens_but_inline_comments_do_not() {
        let text = "<!-- @docs/hidden.md\nstill hidden -->\ntext <!-- @docs/inline.md --> after\n";
        let spans = scan(text);
        assert_eq!(
            texts(text, &spans.comments),
            ["<!-- @docs/hidden.md\nstill hidden -->"]
        );
        let at: Vec<_> = spans.at_tokens.iter().map(|t| t.text.as_str()).collect();
        // The inline comment is not block-level, so the scanner does not hide it.
        assert_eq!(at, ["docs/inline.md"]);
    }

    #[test]
    fn code_spans_match_runs_of_equal_length() {
        let text = "a `@x.md` b ``a ` @y.md`` c `unclosed @z.md";
        let spans = scan(text);
        assert_eq!(texts(text, &spans.code_spans), ["`@x.md`", "``a ` @y.md``"]);
        let at: Vec<_> = spans.at_tokens.iter().map(|t| t.text.as_str()).collect();
        assert_eq!(at, ["z.md"]);
    }

    #[test]
    fn a_code_span_does_not_cross_a_blank_line() {
        let text = "`open\n\nclose` @a.md";
        let spans = scan(text);
        assert!(spans.code_spans.is_empty());
        assert_eq!(spans.at_tokens.len(), 1);
    }

    #[test]
    fn at_tokens_follow_claude_rules() {
        let text = "@a.md foo@docs/email.md @with\\ space.md (@paren.md) @\n";
        let at: Vec<_> = scan(text)
            .at_tokens
            .iter()
            .map(|t| t.text.clone())
            .collect();
        assert_eq!(at, ["a.md", "with\\ space.md"]);
    }

    #[test]
    fn gemini_tokens_run_to_whitespace_and_start_after_blank_characters_only() {
        let toks = at_tokens_of("@a.md. x@b.md\t@c\\d.md @", AtStyle::Gemini);
        let at: Vec<_> = toks.iter().map(|t| t.text.as_str()).collect();
        assert_eq!(at, ["a.md.", "c\\d.md"]);
    }

    #[test]
    fn backtick_regions_cover_fences_and_not_tilde_fences() {
        let text = "`a @1` ```\n@2\n``` ~~~\n@3\n~~~";
        let regions = backtick_regions(text);
        let covered: Vec<_> = regions.iter().map(|r| &text[r.clone()]).collect();
        assert_eq!(covered, ["`a @1`", "```\n@2\n```"]);
    }

    #[test]
    fn path_like_tokens_skip_urls_links_and_code() {
        let text =
            "see docs/a.md, notes.md and (src/lib/x.rs). https://e.com/a.md [l](b/c.md) `d/e.md`\n";
        let spans = scan(text);
        let found: Vec<_> = spans.paths.iter().map(|t| t.text.as_str()).collect();
        assert_eq!(found, ["docs/a.md", "notes.md", "src/lib/x.rs"]);
    }

    #[test]
    fn spans_can_be_asked_whether_they_lie_in_code_or_comments() {
        let text = "`x` plain\n```\nf\n```\n<!-- c -->\n";
        let spans = scan(text);
        assert!(spans.in_code_or_comment(1));
        assert!(!spans.in_code_or_comment(5));
        assert!(spans.in_code_or_comment(text.find('f').unwrap()));
        assert!(spans.in_code_or_comment(text.find("c -->").unwrap()));
    }

    #[test]
    fn columns_count_characters_not_bytes() {
        let spans = scan("é @a.md\n");
        assert_eq!(spans.at_tokens[0].span.col, 3);
    }
}
