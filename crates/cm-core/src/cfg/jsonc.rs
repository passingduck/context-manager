//! Text-in, text-out edits of a JSON file that may hold comments and trailing commas, built on
//! the `jsonc-parser` syntax tree. Comments, key order and indentation survive. A pointer is an
//! RFC 6901 JSON pointer, so `/hooks/PreToolUse/0/matcher`.

use std::fmt;

use jsonc_parser::ParseOptions;
use jsonc_parser::cst::{CstInputValue, CstNode, CstRootNode};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JsoncError {
    /// The text does not parse.
    Parse(String),
    /// The pointer is not valid.
    BadPointer(String),
    /// A value on the way has the wrong type for the edit, or an index is out of range.
    Shape(String),
}

impl fmt::Display for JsoncError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            JsoncError::Parse(m) => write!(f, "parse error: {m}"),
            JsoncError::BadPointer(m) => write!(f, "bad pointer: {m}"),
            JsoncError::Shape(m) => write!(f, "shape: {m}"),
        }
    }
}

impl std::error::Error for JsoncError {}

/// A trailing comma. Gemini CLI fails to parse a file that has one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrailingComma {
    /// 1-based.
    pub line: u32,
    pub col: u32,
    pub offset: usize,
}

fn tokens(pointer: &str) -> Result<Vec<String>, JsoncError> {
    if pointer.is_empty() {
        return Ok(Vec::new());
    }
    let rest = pointer
        .strip_prefix('/')
        .ok_or_else(|| JsoncError::BadPointer(pointer.to_owned()))?;
    Ok(rest
        .split('/')
        .map(|t| t.replace("~1", "/").replace("~0", "~"))
        .collect())
}

fn parse(text: &str) -> Result<CstRootNode, JsoncError> {
    CstRootNode::parse(text, &ParseOptions::default()).map_err(|e| JsoncError::Parse(e.to_string()))
}

/// The value at `pointer`, or `None` when it does not exist.
pub fn get(text: &str, pointer: &str) -> Result<Option<Value>, JsoncError> {
    let root = parse(text)?;
    let Some(value) = root.value().and_then(|node| node.to_serde_value()) else {
        return Ok(None);
    };
    Ok(value.pointer(pointer).cloned())
}

/// Sets the value at `pointer`, creating missing objects on the way.
pub fn set(text: &str, pointer: &str, value: &Value) -> Result<String, JsoncError> {
    let path = tokens(pointer)?;
    let root = parse(text)?;
    let input = to_input(value);
    let Some((last, parents)) = path.split_last() else {
        root.set_value(input);
        return Ok(root.to_string());
    };
    match descend(&root, parents, Create::Objects)? {
        Container::Obj(obj) => match obj.get(last) {
            Some(prop) => prop.set_value(input),
            None => {
                obj.append(last, input);
            }
        },
        Container::Arr(arr) => {
            let at = index(last, &arr)?;
            if at < arr.elements().len() {
                // Insert the new value, then drop the old one that moved up by one.
                arr.insert(at, input);
                arr.elements()[at + 1].clone().remove();
            } else {
                arr.append(input);
            }
        }
    }
    Ok(root.to_string())
}

/// Removes an object key or an array element. A missing target leaves the text as it was.
pub fn remove(text: &str, pointer: &str) -> Result<String, JsoncError> {
    let path = tokens(pointer)?;
    let root = parse(text)?;
    let Some((last, parents)) = path.split_last() else {
        return Err(JsoncError::BadPointer("cannot remove the root".to_owned()));
    };
    let Some(container) = descend_existing(&root, parents) else {
        return Ok(text.to_owned());
    };
    match container {
        Container::Obj(obj) => {
            if let Some(prop) = obj.get(last) {
                prop.remove();
            }
        }
        Container::Arr(arr) => {
            if let Ok(at) = last.parse::<usize>()
                && let Some(element) = arr.elements().get(at)
            {
                element.clone().remove();
            }
        }
    }
    Ok(root.to_string())
}

/// Appends to the array at `pointer`, creating it and any missing objects.
pub fn array_append(text: &str, pointer: &str, value: &Value) -> Result<String, JsoncError> {
    let root = parse(text)?;
    array_at(&root, pointer)?.append(to_input(value));
    Ok(root.to_string())
}

/// Inserts into the array at `pointer` before `at`, creating it and any missing objects.
pub fn array_insert(
    text: &str,
    pointer: &str,
    at: usize,
    value: &Value,
) -> Result<String, JsoncError> {
    let root = parse(text)?;
    let arr = array_at(&root, pointer)?;
    if at > arr.elements().len() {
        return Err(JsoncError::Shape(format!("index {at} is past the end")));
    }
    arr.insert(at, to_input(value));
    Ok(root.to_string())
}

fn array_at(root: &CstRootNode, pointer: &str) -> Result<jsonc_parser::cst::CstArray, JsoncError> {
    let path = tokens(pointer)?;
    let Some((last, parents)) = path.split_last() else {
        return root
            .array_value()
            .or_else(|| root.value().is_none().then(|| root.array_value_or_set()))
            .ok_or_else(|| JsoncError::Shape("the root is not an array".to_owned()));
    };
    match descend(root, parents, Create::Objects)? {
        Container::Obj(obj) => match obj.get(last) {
            Some(prop) => prop
                .array_value()
                .ok_or_else(|| JsoncError::Shape(format!("{last} is not an array"))),
            None => Ok(obj.array_value_or_set(last)),
        },
        Container::Arr(arr) => {
            let at = index(last, &arr)?;
            arr.elements()
                .get(at)
                .and_then(CstNode::as_array)
                .ok_or_else(|| JsoncError::Shape(format!("element {at} is not an array")))
        }
    }
}

/// Every trailing comma in the text, in order.
pub fn trailing_commas(text: &str) -> Vec<TrailingComma> {
    let bytes = text.as_bytes();
    let mut found = Vec::new();
    let mut i = 0;
    let mut last_comma: Option<usize> = None;
    while i < bytes.len() {
        match bytes[i] {
            b'"' | b'\'' => {
                let quote = bytes[i];
                i += 1;
                while i < bytes.len() && bytes[i] != quote {
                    i += if bytes[i] == b'\\' { 2 } else { 1 };
                }
                last_comma = None;
            }
            b'/' if bytes.get(i + 1) == Some(&b'/') => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
                continue;
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                i += 2;
                while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                    i += 1;
                }
                i += 1;
            }
            b',' => last_comma = Some(i),
            b'}' | b']' => {
                if let Some(at) = last_comma.take() {
                    let before = &text[..at];
                    let line = before.matches('\n').count() as u32 + 1;
                    let col =
                        before.rsplit('\n').next().map_or(0, |l| l.chars().count()) as u32 + 1;
                    found.push(TrailingComma {
                        line,
                        col,
                        offset: at,
                    });
                }
            }
            b if b.is_ascii_whitespace() => {}
            _ => last_comma = None,
        }
        i += 1;
    }
    found
}

enum Container {
    Obj(jsonc_parser::cst::CstObject),
    Arr(jsonc_parser::cst::CstArray),
}

#[derive(Clone, Copy)]
enum Create {
    Objects,
}

fn index(token: &str, arr: &jsonc_parser::cst::CstArray) -> Result<usize, JsoncError> {
    let at: usize = token
        .parse()
        .map_err(|_| JsoncError::Shape(format!("{token} is not an array index")))?;
    if at > arr.elements().len() {
        return Err(JsoncError::Shape(format!("index {at} is past the end")));
    }
    Ok(at)
}

fn container_of(node: &CstNode) -> Option<Container> {
    node.as_object()
        .map(Container::Obj)
        .or_else(|| node.as_array().map(Container::Arr))
}

/// Walks `path`, creating missing objects.
fn descend(root: &CstRootNode, path: &[String], _create: Create) -> Result<Container, JsoncError> {
    let mut current = match root.value() {
        None => Container::Obj(root.object_value_or_set()),
        Some(node) => container_of(&node)
            .ok_or_else(|| JsoncError::Shape("the root is not an object or array".to_owned()))?,
    };
    for token in path {
        current = match current {
            Container::Obj(obj) => match obj.get(token) {
                Some(prop) => {
                    let node = prop
                        .value()
                        .ok_or_else(|| JsoncError::Shape(format!("{token} has no value")))?;
                    container_of(&node)
                        .ok_or_else(|| JsoncError::Shape(format!("{token} is not a container")))?
                }
                None => Container::Obj(obj.object_value_or_set(token)),
            },
            Container::Arr(arr) => {
                let at = index(token, &arr)?;
                let node = arr
                    .elements()
                    .get(at)
                    .cloned()
                    .ok_or_else(|| JsoncError::Shape(format!("index {at} is past the end")))?;
                container_of(&node)
                    .ok_or_else(|| JsoncError::Shape(format!("element {at} is not a container")))?
            }
        };
    }
    Ok(current)
}

/// Walks `path` without creating anything.
fn descend_existing(root: &CstRootNode, path: &[String]) -> Option<Container> {
    let mut current = container_of(&root.value()?)?;
    for token in path {
        let node = match &current {
            Container::Obj(obj) => obj.get(token)?.value()?,
            Container::Arr(arr) => arr.elements().get(token.parse::<usize>().ok()?)?.clone(),
        };
        current = container_of(&node)?;
    }
    Some(current)
}

fn to_input(value: &Value) -> CstInputValue {
    match value {
        Value::Null => CstInputValue::Null,
        Value::Bool(b) => CstInputValue::Bool(*b),
        Value::Number(n) => CstInputValue::Number(n.to_string()),
        Value::String(s) => CstInputValue::String(s.clone()),
        Value::Array(items) => CstInputValue::Array(items.iter().map(to_input).collect()),
        Value::Object(map) => {
            CstInputValue::Object(map.iter().map(|(k, v)| (k.clone(), to_input(v))).collect())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const FILE: &str = r#"{
  // keep the model
  "model": "gemini-2.5-pro",
  "hooks": {
    /* block comment */
    "BeforeTool": [
      // first hook
      { "matcher": "shell", "command": "a.sh" },
      { "matcher": "edit", "command": "b.sh" } // trailing note
    ]
  },
  "other": 1 // tail
}
"#;

    fn comments(text: &str) -> Vec<&str> {
        [
            "// keep the model",
            "/* block comment */",
            "// first hook",
            "// trailing note",
            "// tail",
        ]
        .into_iter()
        .filter(|c| text.contains(c))
        .collect()
    }

    fn parses(text: &str) -> Value {
        parse(text)
            .unwrap()
            .value()
            .unwrap()
            .to_serde_value()
            .unwrap()
    }

    fn untouched_lines_survive(before: &str, after: &str, changed: &[&str]) {
        for line in before.lines() {
            if !changed.iter().any(|c| line.contains(c)) {
                assert!(
                    after.lines().any(|l| l == line),
                    "line lost: {line:?}\n{after}"
                );
            }
        }
    }

    #[test]
    fn removing_an_array_element_keeps_every_other_comment_and_line() {
        let out = remove(FILE, "/hooks/BeforeTool/1").unwrap();
        assert_eq!(
            comments(&out).len(),
            4,
            "only the removed element's own note may go\n{out}"
        );
        assert!(out.contains("// first hook") && out.contains("// keep the model"));
        assert_eq!(
            parses(&out)["hooks"]["BeforeTool"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        // The comma after the surviving element goes with the removed one.
        untouched_lines_survive(FILE, &out, &["edit", "a.sh"]);
    }

    #[test]
    fn changing_a_value_keeps_comments_order_and_untouched_lines() {
        let out = set(FILE, "/hooks/BeforeTool/0/command", &json!("z.sh")).unwrap();
        assert_eq!(comments(&out).len(), 5);
        assert!(out.contains(r#""command": "z.sh""#));
        assert_eq!(parses(&out)["hooks"]["BeforeTool"][0]["command"], "z.sh");
        untouched_lines_survive(FILE, &out, &["a.sh"]);
        let keys: Vec<_> = parses(&out).as_object().unwrap().keys().cloned().collect();
        assert_eq!(keys, ["model", "hooks", "other"]);
    }

    #[test]
    fn appending_to_an_array_keeps_comments_and_parses() {
        let out = array_append(
            FILE,
            "/hooks/BeforeTool",
            &json!({"matcher": "x", "command": "c.sh"}),
        )
        .unwrap();
        assert_eq!(comments(&out).len(), 5);
        assert_eq!(
            parses(&out)["hooks"]["BeforeTool"]
                .as_array()
                .unwrap()
                .len(),
            3
        );
        untouched_lines_survive(FILE, &out, &["b.sh"]);
    }

    #[test]
    fn missing_objects_and_arrays_are_created_on_the_way() {
        let out = array_append(FILE, "/hooks/AfterTool", &json!("x")).unwrap();
        assert_eq!(parses(&out)["hooks"]["AfterTool"], json!(["x"]));
        assert_eq!(comments(&out).len(), 5);
        let out = set("{}", "/a/b/c", &json!(1)).unwrap();
        assert_eq!(parses(&out), json!({"a": {"b": {"c": 1}}}));
        let out = set("", "/a", &json!(true)).unwrap();
        assert_eq!(parses(&out), json!({"a": true}));
    }

    #[test]
    fn insert_puts_the_element_at_the_index() {
        let out = array_insert(FILE, "/hooks/BeforeTool", 0, &json!({"matcher": "first"})).unwrap();
        assert_eq!(parses(&out)["hooks"]["BeforeTool"][0]["matcher"], "first");
        assert!(array_insert(FILE, "/hooks/BeforeTool", 9, &json!(1)).is_err());
    }

    #[test]
    fn removing_an_object_key_and_a_missing_target() {
        let out = remove(FILE, "/other").unwrap();
        assert!(parses(&out).get("other").is_none());
        assert_eq!(remove(FILE, "/nope/deeper").unwrap(), FILE);
    }

    #[test]
    fn get_reads_through_a_commented_file() {
        assert_eq!(
            get(FILE, "/hooks/BeforeTool/1/command").unwrap(),
            Some(json!("b.sh"))
        );
        assert_eq!(get(FILE, "/nope").unwrap(), None);
        assert!(matches!(get("{", "/a"), Err(JsoncError::Parse(_))));
    }

    #[test]
    fn trailing_commas_are_reported_with_positions_and_strings_and_comments_are_ignored() {
        let text = "{\n  \"a\": [1, 2,],\n  \"b\": \"x,}\", // c,}\n  \"c\": 1,\n}\n";
        let found = trailing_commas(text);
        let at: Vec<_> = found.iter().map(|t| (t.line, t.col)).collect();
        assert_eq!(at, [(2, 13), (4, 9)]);
        assert!(trailing_commas(FILE).is_empty());
    }
}
