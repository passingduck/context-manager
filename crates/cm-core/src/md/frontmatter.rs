//! YAML frontmatter. It exists only when `---` is the first line and a later `---` line closes
//! it.

use std::ops::Range;

use serde_json::{Map, Value};

/// A block over this many bytes is not parsed. The largest of 2,748 frontmatter blocks in
/// installed agent files was 15.7 KB.
pub const MAX_BLOCK_BYTES: usize = 64 * 1024;

/// A block with more `[` and `{` than this is not parsed. The YAML parser's work grows with the
/// square of how deep these nest, and their count bounds the depth. The count includes brackets
/// inside strings, so a `]` in a string cannot hide a level. The largest count in 2,748
/// frontmatter blocks of installed agent files was 112.
pub const MAX_BLOCK_BRACKETS: usize = 512;

#[derive(Debug, Clone, PartialEq)]
pub struct Frontmatter {
    /// The top-level keys, unknown ones included, in file order. `None` when there is no
    /// frontmatter, when the YAML is broken, or when it is not a mapping.
    pub map: Option<Map<String, Value>>,
    /// The byte range of the text after the closing line. The whole text when there is no
    /// frontmatter.
    pub body: Range<usize>,
    /// The byte range of the frontmatter block, delimiters included. `None` when absent.
    pub block: Option<Range<usize>>,
    /// The YAML parse error, when the block exists and does not parse.
    pub error: Option<String>,
}

impl Frontmatter {
    pub fn is_present(&self) -> bool {
        self.block.is_some()
    }
}

pub fn parse(text: &str) -> Frontmatter {
    let absent = Frontmatter {
        map: None,
        body: 0..text.len(),
        block: None,
        error: None,
    };
    let text_start = usize::from(text.starts_with('\u{feff}')) * '\u{feff}'.len_utf8();
    let mut lines = LineStarts::new(text, text_start);
    let Some((first_start, first_end)) = lines.next() else {
        return absent;
    };
    if text[first_start..first_end].trim_end() != "---" {
        return absent;
    }
    let yaml_start = first_end;
    for (start, end) in lines {
        if text[start..end].trim_end() == "---" {
            let yaml = &text[yaml_start..start];
            let (map, error) = parse_yaml(yaml);
            return Frontmatter {
                map,
                body: end..text.len(),
                block: Some(text_start..end),
                error,
            };
        }
    }
    absent
}

fn parse_yaml(yaml: &str) -> (Option<Map<String, Value>>, Option<String>) {
    if yaml.trim().is_empty() {
        return (Some(Map::new()), None);
    }
    if yaml.len() > MAX_BLOCK_BYTES {
        return (
            None,
            Some(format!("frontmatter is over {MAX_BLOCK_BYTES} bytes")),
        );
    }
    if yaml.bytes().filter(|b| matches!(b, b'[' | b'{')).count() > MAX_BLOCK_BRACKETS {
        return (
            None,
            Some(format!(
                "frontmatter has over {MAX_BLOCK_BRACKETS} opening brackets"
            )),
        );
    }
    match serde_yaml_ng::from_str::<Value>(yaml) {
        Ok(Value::Object(map)) => (Some(map), None),
        Ok(Value::Null) => (Some(Map::new()), None),
        Ok(_) => (None, Some("frontmatter is not a mapping".to_owned())),
        Err(err) => (None, Some(err.to_string())),
    }
}

/// `(start, end)` of each line, `end` after the newline.
struct LineStarts<'a> {
    text: &'a str,
    at: usize,
}

impl<'a> LineStarts<'a> {
    fn new(text: &'a str, at: usize) -> Self {
        LineStarts { text, at }
    }
}

impl Iterator for LineStarts<'_> {
    type Item = (usize, usize);
    fn next(&mut self) -> Option<(usize, usize)> {
        if self.at >= self.text.len() {
            return None;
        }
        let start = self.at;
        let end = self.text[start..]
            .find('\n')
            .map_or(self.text.len(), |i| start + i + 1);
        self.at = end;
        Some((start, end))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frontmatter_parses_with_unknown_keys_kept_and_a_body_range() {
        let text = "---\npaths:\n  - src/**\nowner: me\n---\n# Body\n";
        let fm = parse(text);
        let map = fm.map.unwrap();
        assert_eq!(map.keys().collect::<Vec<_>>(), ["paths", "owner"]);
        assert_eq!(map["paths"], serde_json::json!(["src/**"]));
        assert_eq!(&text[fm.body], "# Body\n");
        assert_eq!(fm.error, None);
    }

    #[test]
    fn it_exists_only_when_the_delimiter_is_the_first_line() {
        for text in ["\n---\na: 1\n---\nx", "x\n---\na: 1\n---\n", "---\na: 1\n"] {
            let fm = parse(text);
            assert!(!fm.is_present(), "{text:?}");
            assert_eq!(fm.body, 0..text.len());
        }
    }

    #[test]
    fn broken_yaml_reports_an_error_and_still_finds_the_body() {
        let text = "---\npaths: [unclosed\n---\nbody";
        let fm = parse(text);
        assert!(fm.map.is_none());
        assert!(fm.error.is_some());
        assert_eq!(&text[fm.body], "body");
    }

    #[test]
    fn an_empty_block_is_an_empty_map() {
        let text = "---\n---\nbody";
        let fm = parse(text);
        assert_eq!(fm.map.unwrap().len(), 0);
        assert_eq!(&text[fm.body], "body");
    }

    /// The YAML parser's work grows with the square of the nesting depth: 3.5 seconds at 20,000
    /// levels, 52 seconds at 80,000.
    #[test]
    fn a_deeply_nested_block_is_an_error_returned_at_once() {
        let deep = |open: &str, close: &str, n: usize| {
            format!(
                "---\npaths: {}{}\n---\nbody\n",
                open.repeat(n),
                close.repeat(n)
            )
        };
        for text in [
            deep("[", "]", 50_000),
            deep("[", "]", 10_000),
            // A closing bracket inside a string does not close anything.
            deep("[\"]\",", "]", 10_000),
        ] {
            let started = std::time::Instant::now();
            let fm = parse(&text);
            let elapsed = started.elapsed();
            assert!(fm.map.is_none());
            assert!(fm.error.is_some());
            assert_eq!(&text[fm.body], "body\n");
            assert!(
                elapsed < std::time::Duration::from_millis(250),
                "{} bytes took {elapsed:?}",
                text.len()
            );
        }
    }

    #[test]
    fn a_block_with_many_bracketed_values_still_parses() {
        // The largest count of `[` and `{` in 2,748 frontmatter blocks of installed agent files
        // was 112.
        let line = "- [a, {b: c}]\n";
        let text = format!("---\nitems:\n{}---\nbody\n", line.repeat(100));
        let fm = parse(&text);
        assert_eq!(fm.error, None);
        assert_eq!(fm.map.unwrap()["items"].as_array().unwrap().len(), 100);
    }

    #[test]
    fn a_closing_line_without_a_trailing_newline_ends_the_text() {
        let text = "---\na: 1\n---";
        let fm = parse(text);
        assert!(fm.is_present());
        assert_eq!(fm.body, text.len()..text.len());
    }
}
