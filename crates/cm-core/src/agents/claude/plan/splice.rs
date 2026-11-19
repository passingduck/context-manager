//! Edits of a strict JSON text by span. Settings files are strict JSON (spec 6.2), so a small
//! scanner that records where each value starts and ends is enough. An edit inserts or cuts the
//! text of one entry and its separator, and it copies the whitespace and the key separator of the
//! entry's neighbours, so the bytes of every other entry stay as they are.

use serde_json::Value;

pub(super) struct Node {
    pub start: usize,
    pub end: usize,
    pub kind: Kind,
}

pub(super) enum Kind {
    Object(Vec<Member>),
    Array(Vec<Node>),
    Scalar,
}

pub(super) struct Member {
    pub key: String,
    /// Where the key's opening quote is, and where its closing quote ends.
    pub start: usize,
    pub key_end: usize,
    pub value: Node,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Seg {
    Key(String),
    Index(usize),
}

pub(super) fn parse(text: &str) -> Option<Node> {
    let mut p = Scanner {
        text,
        bytes: text.as_bytes(),
        at: 0,
    };
    p.skip();
    let node = p.value()?;
    p.skip();
    (p.at == p.bytes.len()).then_some(node)
}

struct Scanner<'a> {
    text: &'a str,
    bytes: &'a [u8],
    at: usize,
}

impl Scanner<'_> {
    fn skip(&mut self) {
        while self.at < self.bytes.len() && self.bytes[self.at].is_ascii_whitespace() {
            self.at += 1;
        }
    }

    fn eat(&mut self, byte: u8) -> Option<()> {
        (self.bytes.get(self.at) == Some(&byte)).then(|| self.at += 1)
    }

    fn string(&mut self) -> Option<(usize, usize, String)> {
        let start = self.at;
        self.eat(b'"')?;
        while let Some(&b) = self.bytes.get(self.at) {
            self.at += 1;
            match b {
                b'\\' => self.at += 1,
                b'"' => {
                    let raw = self.text.get(start..self.at)?;
                    return Some((start, self.at, serde_json::from_str(raw).ok()?));
                }
                _ => {}
            }
        }
        None
    }

    fn value(&mut self) -> Option<Node> {
        let start = self.at;
        let kind = match *self.bytes.get(self.at)? {
            b'{' => {
                self.at += 1;
                let mut members = Vec::new();
                self.skip();
                if self.eat(b'}').is_none() {
                    loop {
                        self.skip();
                        let (key_start, key_end, key) = self.string()?;
                        self.skip();
                        self.eat(b':')?;
                        self.skip();
                        let value = self.value()?;
                        members.push(Member {
                            key,
                            start: key_start,
                            key_end,
                            value,
                        });
                        self.skip();
                        if self.eat(b',').is_some() {
                            continue;
                        }
                        self.eat(b'}')?;
                        break;
                    }
                }
                Kind::Object(members)
            }
            b'[' => {
                self.at += 1;
                let mut items = Vec::new();
                self.skip();
                if self.eat(b']').is_none() {
                    loop {
                        self.skip();
                        items.push(self.value()?);
                        self.skip();
                        if self.eat(b',').is_some() {
                            continue;
                        }
                        self.eat(b']')?;
                        break;
                    }
                }
                Kind::Array(items)
            }
            b'"' => {
                self.string()?;
                Kind::Scalar
            }
            _ => {
                while self.at < self.bytes.len()
                    && !matches!(self.bytes[self.at], b',' | b'}' | b']')
                    && !self.bytes[self.at].is_ascii_whitespace()
                {
                    self.at += 1;
                }
                (self.at > start).then_some(Kind::Scalar)?
            }
        };
        Some(Node {
            start,
            end: self.at,
            kind,
        })
    }
}

impl Node {
    pub fn find(&self, path: &[Seg]) -> Option<&Node> {
        let Some((first, rest)) = path.split_first() else {
            return Some(self);
        };
        match (&self.kind, first) {
            (Kind::Object(members), Seg::Key(key)) => members
                .iter()
                .find(|m| m.key == *key)
                .and_then(|m| m.value.find(rest)),
            (Kind::Array(items), Seg::Index(at)) => items.get(*at).and_then(|n| n.find(rest)),
            _ => None,
        }
    }

    pub fn len(&self) -> usize {
        match &self.kind {
            Kind::Object(members) => members.len(),
            Kind::Array(items) => items.len(),
            Kind::Scalar => 0,
        }
    }
}

/// A pointer as path segments. Numbers are array indexes where the node is an array, so the
/// caller passes `Seg`s that it knows the shape of.
pub(super) fn path(pointer: &str, array_at: impl Fn(usize) -> bool) -> Vec<Seg> {
    if pointer.is_empty() {
        return Vec::new();
    }
    pointer
        .trim_start_matches('/')
        .split('/')
        .enumerate()
        .map(|(depth, raw)| {
            let key = raw.replace("~1", "/").replace("~0", "~");
            match key.parse::<usize>() {
                Ok(index) if array_at(depth) => Seg::Index(index),
                _ => Seg::Key(key),
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------------------------
// Layout of the text around a node
// ---------------------------------------------------------------------------------------------

/// The whitespace that ends right before `at`.
fn ws_before(text: &str, at: usize) -> &str {
    let bytes = text.as_bytes();
    let mut from = at;
    while from > 0 && bytes[from - 1].is_ascii_whitespace() {
        from -= 1;
    }
    &text[from..at]
}

/// The leading whitespace of the line that holds `at`.
fn line_indent(text: &str, at: usize) -> &str {
    let line = text[..at].rfind('\n').map_or(0, |n| n + 1);
    let end = text[line..]
        .find(|c: char| c != ' ' && c != '\t')
        .map_or(text.len(), |n| line + n);
    &text[line..end]
}

/// One level of indentation: the first indented line of the file, else two spaces.
fn indent_unit(text: &str) -> String {
    text.lines()
        .map(|line| &line[..line.len() - line.trim_start_matches([' ', '\t']).len()])
        .find(|indent| !indent.is_empty())
        .unwrap_or("  ")
        .to_owned()
}

/// The line ending the file uses.
fn newline(text: &str) -> &'static str {
    if text.contains("\r\n") { "\r\n" } else { "\n" }
}

fn render(value: &Value, multiline: bool, unit: &str, base: &str, nl: &str) -> String {
    if !multiline {
        return serde_json::to_string(value).expect("a value serializes");
    }
    let mut out = Vec::new();
    let formatter = serde_json::ser::PrettyFormatter::with_indent(unit.as_bytes());
    let mut ser = serde_json::Serializer::with_formatter(&mut out, formatter);
    serde::Serialize::serialize(value, &mut ser).expect("a value serializes");
    String::from_utf8(out)
        .expect("JSON is UTF-8")
        .replace('\n', &format!("{nl}{base}"))
}

fn splice(text: &str, from: usize, to: usize, with: &str) -> String {
    format!("{}{with}{}", &text[..from], &text[to..])
}

/// What follows `{` or `[` when a container with nothing in it gets its first entry.
fn first_entry(text: &str, open: usize, rendered: impl Fn(&str) -> String) -> (String, bool) {
    let multiline = text.contains('\n');
    if !multiline {
        return (rendered(""), false);
    }
    let base = line_indent(text, open).to_owned();
    let unit = indent_unit(text);
    let inner = format!("{base}{unit}");
    let nl = newline(text);
    (format!("{nl}{inner}{}{nl}{base}", rendered(&inner)), true)
}

// ---------------------------------------------------------------------------------------------
// Edits
// ---------------------------------------------------------------------------------------------

/// Inserts `value` into the array before element `at`, or at the end.
pub(super) fn array_insert(text: &str, array: &Node, at: Option<usize>, value: &Value) -> String {
    let Kind::Array(items) = &array.kind else {
        unreachable!("the caller checked the kind");
    };
    let unit = indent_unit(text);
    let Some(last) = items.last() else {
        let (inner, _) = first_entry(text, array.start, |base| {
            render(value, text.contains('\n'), &unit, base, newline(text))
        });
        return splice(text, array.start + 1, array.end - 1, &inner);
    };
    match at.filter(|at| *at < items.len()) {
        Some(at) => {
            let start = items[at].start;
            let lead = ws_before(text, start);
            let rendered = render(
                value,
                lead.contains('\n'),
                &unit,
                base_of(lead),
                newline(text),
            );
            splice(text, start, start, &format!("{rendered},{lead}"))
        }
        None => {
            let lead = ws_before(text, last.start);
            let rendered = render(
                value,
                lead.contains('\n'),
                &unit,
                base_of(lead),
                newline(text),
            );
            splice(text, last.end, last.end, &format!(",{lead}{rendered}"))
        }
    }
}

fn base_of(lead: &str) -> &str {
    lead.rfind('\n').map_or("", |n| &lead[n + 1..])
}

/// Inserts `"key": value` into the object before member `at`, or at the end.
pub(super) fn object_insert(
    text: &str,
    object: &Node,
    at: Option<usize>,
    key: &str,
    value: &Value,
) -> String {
    let Kind::Object(members) = &object.kind else {
        unreachable!("the caller checked the kind");
    };
    let unit = indent_unit(text);
    let key_json = serde_json::to_string(key).expect("a key serializes");
    let Some(last) = members.last() else {
        let (inner, _) = first_entry(text, object.start, |base| {
            format!(
                "{key_json}: {}",
                render(value, text.contains('\n'), &unit, base, newline(text))
            )
        });
        return splice(text, object.start + 1, object.end - 1, &inner);
    };
    let sep = |m: &Member| text[m.key_end..m.value.start].to_owned();
    match at.filter(|at| *at < members.len()) {
        Some(at) => {
            let member = &members[at];
            let lead = ws_before(text, member.start);
            let rendered = render(
                value,
                lead.contains('\n'),
                &unit,
                base_of(lead),
                newline(text),
            );
            splice(
                text,
                member.start,
                member.start,
                &format!("{key_json}{}{rendered},{lead}", sep(member)),
            )
        }
        None => {
            let lead = ws_before(text, last.start);
            let rendered = render(
                value,
                lead.contains('\n'),
                &unit,
                base_of(lead),
                newline(text),
            );
            splice(
                text,
                last.value.end,
                last.value.end,
                &format!(",{lead}{key_json}{}{rendered}", sep(last)),
            )
        }
    }
}

/// Cuts element or member `at` and its separator.
pub(super) fn remove_entry(text: &str, container: &Node, at: usize) -> String {
    let spans: Vec<(usize, usize)> = match &container.kind {
        Kind::Array(items) => items.iter().map(|n| (n.start, n.end)).collect(),
        Kind::Object(members) => members.iter().map(|m| (m.start, m.value.end)).collect(),
        Kind::Scalar => unreachable!("the caller checked the kind"),
    };
    let n = spans.len();
    if n == 1 {
        return splice(text, container.start + 1, container.end - 1, "");
    }
    if at + 1 == n {
        splice(text, spans[at - 1].1, spans[at].1, "")
    } else {
        splice(text, spans[at].0, spans[at + 1].0, "")
    }
}

/// Replaces the text of `node` with `value`, laid out like the node was.
pub(super) fn replace(text: &str, node: &Node, value: &Value) -> String {
    let unit = indent_unit(text);
    let multiline =
        ws_before(text, node.start).contains('\n') || text[node.start..node.end].contains('\n');
    let rendered = render(
        value,
        multiline,
        &unit,
        line_indent(text, node.start),
        newline(text),
    );
    splice(text, node.start, node.end, &rendered)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn array(text: &str) -> Node {
        parse(text).unwrap()
    }

    #[test]
    fn the_scanner_records_spans_and_rejects_what_is_not_json() {
        let text = " {\"a\" : [1, \"x,]\" ,{}], \"b\":null}\n";
        let root = parse(text).unwrap();
        let a = root.find(&[Seg::Key("a".into())]).unwrap();
        assert_eq!(&text[a.start..a.end], "[1, \"x,]\" ,{}]");
        assert_eq!(a.len(), 3);
        assert!(parse("{\"a\":}").is_none());
        assert!(parse("{\"a\":1} x").is_none());
    }

    #[test]
    fn an_insert_copies_the_whitespace_of_its_neighbours_and_a_cut_undoes_it() {
        for text in [
            "[1,2]",
            "[ 1, 2 ]",
            "[\n  1,\n  2\n]",
            "[\r\n\t1,\r\n\t2\r\n]",
        ] {
            let before = array(text);
            let added = array_insert(text, &before, None, &json!(3));
            assert!(serde_json::from_str::<Value>(&added).is_ok(), "{added:?}");
            let again = array(&added);
            assert_eq!(remove_entry(&added, &again, 2), text, "{text:?}");
            // Before the first element: the cut of the first element undoes it too.
            let front = array_insert(text, &before, Some(0), &json!(0));
            assert_eq!(remove_entry(&front, &array(&front), 0), text, "{text:?}");
        }
    }

    #[test]
    fn an_object_insert_keeps_the_key_separator_of_its_neighbours() {
        for text in ["{\"a\":1}", "{ \"a\" : 1 }", "{\n  \"a\": 1\n}"] {
            let object = array(text);
            let added = object_insert(text, &object, None, "b", &json!([1]));
            assert!(serde_json::from_str::<Value>(&added).is_ok(), "{added:?}");
            assert_eq!(remove_entry(&added, &array(&added), 1), text, "{text:?}");
        }
    }

    #[test]
    fn the_first_entry_of_an_empty_container_follows_the_file_layout() {
        let compact = array("{\"a\":[]}");
        let a = compact.find(&[Seg::Key("a".into())]).unwrap();
        assert_eq!(
            array_insert("{\"a\":[]}", a, None, &json!(1)),
            "{\"a\":[1]}"
        );
        let text = "{\n    \"a\": []\n}\n";
        let root = array(text);
        let a = root.find(&[Seg::Key("a".into())]).unwrap();
        assert_eq!(
            array_insert(text, a, None, &json!({"k": 1})),
            "{\n    \"a\": [\n        {\n            \"k\": 1\n        }\n    ]\n}\n"
        );
    }
}
