//! Text-in, text-out edits of a TOML file over `toml_edit`. Comments, key order and the
//! formatting of untouched lines survive. A path is a list of keys, so
//! `["hooks", "state", "my-key"]` is the table `[hooks.state."my-key"]`. A missing table on a
//! path is created as a standard table, never as an inline table.

use std::fmt;

use serde_json::Value;
use toml_edit::{ArrayOfTables, DocumentMut, Item, Table, TableLike};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TomlError {
    Parse(String),
    /// A value on the path has the wrong type for the edit, or the value cannot be TOML.
    Shape(String),
}

impl fmt::Display for TomlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TomlError::Parse(m) => write!(f, "parse error: {m}"),
            TomlError::Shape(m) => write!(f, "shape: {m}"),
        }
    }
}

impl std::error::Error for TomlError {}

fn parse(text: &str) -> Result<DocumentMut, TomlError> {
    text.parse::<DocumentMut>()
        .map_err(|e| TomlError::Parse(e.to_string()))
}

/// The value at `path`, or `None` when it does not exist.
pub fn get(text: &str, path: &[&str]) -> Result<Option<Value>, TomlError> {
    let doc = parse(text)?;
    let mut item: &Item = doc.as_item();
    for key in path {
        item = match step(item, key) {
            Some(next) => next,
            None => return Ok(None),
        };
    }
    Ok(Some(to_json(item)))
}

fn step<'a>(item: &'a Item, key: &str) -> Option<&'a Item> {
    item.as_table_like()?.get(key)
}

/// Sets the value at `path`, creating missing tables as standard tables.
pub fn set(text: &str, path: &[&str], value: &Value) -> Result<String, TomlError> {
    let mut doc = parse(text)?;
    let Some((last, parents)) = path.split_last() else {
        return Err(TomlError::Shape("the path is empty".to_owned()));
    };
    let parent = table_at(&mut doc, parents)?;
    let mut item = to_item(value)?;
    // The comments around the old value belong to the line, so the new value inherits them.
    match (parent.get(last), &mut item) {
        (Some(Item::Value(old)), Item::Value(new)) => *new.decor_mut() = old.decor().clone(),
        (Some(Item::Table(old)), Item::Table(new)) => *new.decor_mut() = old.decor().clone(),
        _ => {}
    }
    // Replace in place, because `insert` would also replace the key and lose its decoration.
    match parent.get_mut(last) {
        Some(slot) => *slot = item,
        None => {
            parent.insert(last, item);
        }
    }
    Ok(doc.to_string())
}

/// Removes the key at `path`. A missing key leaves the text as it was.
pub fn remove(text: &str, path: &[&str]) -> Result<String, TomlError> {
    let mut doc = parse(text)?;
    let Some((last, parents)) = path.split_last() else {
        return Err(TomlError::Shape("the path is empty".to_owned()));
    };
    let mut table: &mut dyn TableLike = doc.as_table_mut();
    for key in parents {
        let Some(next) = table.get_mut(key).and_then(Item::as_table_like_mut) else {
            return Ok(text.to_owned());
        };
        table = next;
    }
    table.remove(last);
    Ok(doc.to_string())
}

/// Appends a table to the array of tables at `path` (`[[a.b]]`), creating the array and any
/// missing tables. `value` must be an object.
pub fn aot_append(text: &str, path: &[&str], value: &Value) -> Result<String, TomlError> {
    let mut doc = parse(text)?;
    let Some((last, parents)) = path.split_last() else {
        return Err(TomlError::Shape("the path is empty".to_owned()));
    };
    let Item::Table(entry) = to_item(value)? else {
        return Err(TomlError::Shape(
            "an array element must be an object".to_owned(),
        ));
    };
    let parent = table_at(&mut doc, parents)?;
    if !parent.contains_key(last) {
        parent.insert(last, Item::ArrayOfTables(ArrayOfTables::new()));
    }
    parent
        .get_mut(last)
        .and_then(Item::as_array_of_tables_mut)
        .ok_or_else(|| TomlError::Shape(format!("{last} is not an array of tables")))?
        .push(entry);
    Ok(doc.to_string())
}

/// Removes element `index` of the array of tables at `path`. A missing element leaves the text
/// as it was. An array left empty is removed.
pub fn aot_remove(text: &str, path: &[&str], index: usize) -> Result<String, TomlError> {
    let mut doc = parse(text)?;
    let Some((last, parents)) = path.split_last() else {
        return Err(TomlError::Shape("the path is empty".to_owned()));
    };
    let mut table: &mut dyn TableLike = doc.as_table_mut();
    for key in parents {
        let Some(next) = table.get_mut(key).and_then(Item::as_table_like_mut) else {
            return Ok(text.to_owned());
        };
        table = next;
    }
    let Some(aot) = table.get_mut(last).and_then(Item::as_array_of_tables_mut) else {
        return Ok(text.to_owned());
    };
    if index >= aot.len() {
        return Ok(text.to_owned());
    }
    aot.remove(index);
    if aot.is_empty() {
        table.remove(last);
    }
    Ok(doc.to_string())
}

/// The table at `path`, created as a standard table where missing. Tables made only to hold
/// others are implicit, so `[hooks]` is not written for `[hooks.state."k"]`.
fn table_at<'a>(
    doc: &'a mut DocumentMut,
    path: &[&str],
) -> Result<&'a mut dyn TableLike, TomlError> {
    let mut table: &mut dyn TableLike = doc.as_table_mut();
    for key in path {
        if !table.contains_key(key) {
            let mut made = Table::new();
            made.set_implicit(true);
            table.insert(key, Item::Table(made));
        }
        table = table
            .get_mut(key)
            .and_then(Item::as_table_like_mut)
            .ok_or_else(|| TomlError::Shape(format!("{key} is not a table")))?;
    }
    Ok(table)
}

fn to_item(value: &Value) -> Result<Item, TomlError> {
    match value {
        Value::Object(map) => {
            let mut table = Table::new();
            for (key, v) in map {
                table.insert(key, to_item(v)?);
            }
            Ok(Item::Table(table))
        }
        Value::Array(items) if !items.is_empty() && items.iter().all(Value::is_object) => {
            let mut aot = ArrayOfTables::new();
            for item in items {
                let Item::Table(t) = to_item(item)? else {
                    unreachable!("an object becomes a table");
                };
                aot.push(t);
            }
            Ok(Item::ArrayOfTables(aot))
        }
        other => Ok(Item::Value(to_value(other)?)),
    }
}

fn to_value(value: &Value) -> Result<toml_edit::Value, TomlError> {
    Ok(match value {
        Value::Null => return Err(TomlError::Shape("TOML has no null".to_owned())),
        Value::Bool(b) => (*b).into(),
        Value::Number(n) => match n.as_i64() {
            Some(i) => i.into(),
            None => n
                .as_f64()
                .ok_or_else(|| TomlError::Shape("number out of range".to_owned()))?
                .into(),
        },
        Value::String(s) => s.as_str().into(),
        Value::Array(items) => {
            let mut array = toml_edit::Array::new();
            for item in items {
                array.push(to_value(item)?);
            }
            toml_edit::Value::Array(array)
        }
        Value::Object(map) => {
            let mut table = toml_edit::InlineTable::new();
            for (key, v) in map {
                table.insert(key, to_value(v)?);
            }
            toml_edit::Value::InlineTable(table)
        }
    })
}

fn to_json(item: &Item) -> Value {
    match item {
        Item::None => Value::Null,
        Item::Value(v) => value_to_json(v),
        Item::Table(t) => {
            Value::Object(t.iter().map(|(k, v)| (k.to_owned(), to_json(v))).collect())
        }
        Item::ArrayOfTables(a) => Value::Array(
            a.iter()
                .map(|t| Value::Object(t.iter().map(|(k, v)| (k.to_owned(), to_json(v))).collect()))
                .collect(),
        ),
    }
}

fn value_to_json(value: &toml_edit::Value) -> Value {
    use toml_edit::Value as V;
    match value {
        V::String(s) => Value::String(s.value().clone()),
        V::Integer(i) => Value::from(*i.value()),
        V::Float(f) => serde_json::Number::from_f64(*f.value()).map_or(Value::Null, Value::Number),
        V::Boolean(b) => Value::Bool(*b.value()),
        V::Datetime(d) => Value::String(d.value().to_string()),
        V::Array(a) => Value::Array(a.iter().map(value_to_json).collect()),
        V::InlineTable(t) => Value::Object(
            t.iter()
                .map(|(k, v)| (k.to_owned(), value_to_json(v)))
                .collect(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const FILE: &str = r##"# Codex config
model = "gpt-5"   # default model

[features]
hooks = true # keep

# state for one hook
[hooks.state."a:b"]
trusted_hash = "sha256:1"  # recorded

[[hooks.PreToolUse]]
matcher = "shell"   # first

[[hooks.PreToolUse.hooks]]
command = "a.sh"

[[hooks.PreToolUse]]
matcher = "edit"

[tail]
x = 1
"##;

    const COMMENTS: [&str; 6] = [
        "# Codex config",
        "# default model",
        "# keep",
        "# state for one hook",
        "# recorded",
        "# first",
    ];

    fn parses(text: &str) -> toml::Value {
        toml::from_str(text).unwrap_or_else(|e| panic!("does not parse: {e}\n{text}"))
    }

    fn all_comments_survive(text: &str) {
        for c in COMMENTS {
            assert!(text.contains(c), "lost {c}\n{text}");
        }
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
    fn removing_a_table_keeps_comments_and_untouched_lines() {
        let out = remove(FILE, &["hooks", "state", "a:b"]).unwrap();
        assert!(!out.contains("trusted_hash"));
        assert!(parses(&out).get("hooks").unwrap().get("state").is_none());
        assert!(
            out.contains("# keep") && out.contains("# first") && out.contains("# default model")
        );
        untouched_lines_survive(FILE, &out, &["a:b", "trusted_hash", "# state for one hook"]);
    }

    #[test]
    fn changing_a_value_keeps_its_comment_and_every_other_line() {
        let out = set(FILE, &["model"], &json!("gpt-6")).unwrap();
        assert!(out.contains(r#"model = "gpt-6""#));
        all_comments_survive(&out);
        untouched_lines_survive(FILE, &out, &["model ="]);
        assert_eq!(parses(&out)["model"].as_str(), Some("gpt-6"));
    }

    #[test]
    fn a_missing_table_is_created_as_a_standard_table_never_inline() {
        let out = set(
            FILE,
            &["hooks", "state", "c:d"],
            &json!({"trusted_hash": "sha256:2"}),
        )
        .unwrap();
        assert!(out.contains("[hooks.state.\"c:d\"]"), "{out}");
        assert!(!out.contains('{'), "no inline table: {out}");
        all_comments_survive(&out);
        untouched_lines_survive(FILE, &out, &[]);
        assert_eq!(
            parses(&out)["hooks"]["state"]["c:d"]["trusted_hash"].as_str(),
            Some("sha256:2")
        );
        // From nothing, the parent tables stay implicit.
        let out = set("", &["hooks", "state", "k"], &json!({"trusted_hash": "h"})).unwrap();
        assert_eq!(out, "[hooks.state.k]\ntrusted_hash = \"h\"\n");
    }

    #[test]
    fn appending_an_array_of_tables_entry_keeps_comments() {
        let out = aot_append(FILE, &["hooks", "PreToolUse"], &json!({"matcher": "new"})).unwrap();
        all_comments_survive(&out);
        untouched_lines_survive(FILE, &out, &[]);
        let pre = &parses(&out)["hooks"]["PreToolUse"];
        assert_eq!(pre.as_array().unwrap().len(), 3);
        assert_eq!(pre[2]["matcher"].as_str(), Some("new"));
        let fresh = aot_append("", &["a", "b"], &json!({"k": 1})).unwrap();
        assert_eq!(fresh, "[[a.b]]\nk = 1\n");
    }

    #[test]
    fn removing_an_array_of_tables_entry_keeps_the_others() {
        let out = aot_remove(FILE, &["hooks", "PreToolUse"], 1).unwrap();
        let pre = &parses(&out)["hooks"]["PreToolUse"];
        assert_eq!(pre.as_array().unwrap().len(), 1);
        assert_eq!(pre[0]["matcher"].as_str(), Some("shell"));
        all_comments_survive(&out);
        assert_eq!(aot_remove(FILE, &["hooks", "PreToolUse"], 9).unwrap(), FILE);
    }

    #[test]
    fn get_reads_values_tables_and_missing_paths() {
        assert_eq!(get(FILE, &["model"]).unwrap(), Some(json!("gpt-5")));
        assert_eq!(
            get(FILE, &["hooks", "state", "a:b"]).unwrap(),
            Some(json!({"trusted_hash": "sha256:1"}))
        );
        assert_eq!(get(FILE, &["nope", "x"]).unwrap(), None);
        assert!(matches!(get("a = ", &["a"]), Err(TomlError::Parse(_))));
    }

    #[test]
    fn removing_a_missing_key_changes_nothing() {
        assert_eq!(remove(FILE, &["nope", "x"]).unwrap(), FILE);
        assert_eq!(remove(FILE, &["tail", "y"]).unwrap(), FILE);
    }
}
