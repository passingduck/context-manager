//! Text-in, text-out edits of a TOML file over `toml_edit`. Comments, key order and the
//! formatting of untouched lines survive. A path is a list of keys, so
//! `["hooks", "state", "my-key"]` is the table `[hooks.state."my-key"]`. A missing table on a
//! path is created as a standard table, never as an inline table.

use std::fmt;

use serde_json::Value;

use crate::model::TomlSeg;
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

/// Removes the key at `path`, then removes each parent table the removal left empty and that is
/// implicit (made only to hold others), so a key `set` made from nothing leaves no header
/// behind. The table that held the key goes when it is empty, written or not.
pub fn remove_pruned(text: &str, path: &[&str]) -> Result<String, TomlError> {
    let mut doc = parse(text)?;
    let Some((last, parents)) = path.split_last() else {
        return Err(TomlError::Shape("the path is empty".to_owned()));
    };
    prune(doc.as_table_mut(), parents, last);
    Ok(doc.to_string())
}

fn prune(table: &mut dyn TableLike, parents: &[&str], last: &str) {
    let Some((first, rest)) = parents.split_first() else {
        table.remove(last);
        return;
    };
    let Some(item) = table.get_mut(first) else {
        return;
    };
    let Some(child) = item.as_table_like_mut() else {
        return;
    };
    prune(child, rest, last);
    let empty = child.is_empty();
    let implicit = item.as_table().is_some_and(|t| t.is_implicit());
    // The innermost parent goes when empty. Higher ones go only when implicit.
    if empty && (rest.is_empty() || implicit) {
        table.remove(first);
    }
}

fn element<'a>(
    doc: &'a mut DocumentMut,
    path: &[TomlSeg],
) -> Result<&'a mut dyn TableLike, TomlError> {
    let mut table: &mut dyn TableLike = doc.as_table_mut();
    let mut at = 0;
    while at < path.len() {
        let TomlSeg::Key { name } = &path[at] else {
            return Err(TomlError::Shape("an index must follow a key".to_owned()));
        };
        let item = table
            .get_mut(name)
            .ok_or_else(|| TomlError::Shape(format!("{name} is missing")))?;
        if let Some(TomlSeg::Index { index }) = path.get(at + 1) {
            let aot = item
                .as_array_of_tables_mut()
                .ok_or_else(|| TomlError::Shape(format!("{name} is not an array of tables")))?;
            table = aot
                .get_mut(*index as usize)
                .ok_or_else(|| TomlError::Shape(format!("{name}[{index}] is missing")))?;
            at += 2;
        } else {
            table = item
                .as_table_like_mut()
                .ok_or_else(|| TomlError::Shape(format!("{name} is not a table")))?;
            at += 1;
        }
    }
    Ok(table)
}

/// Sets `key` of the table at `path` (keys and array-of-tables indices), or removes it when
/// `value` is `None`. The rest of the table, its comments included, stays.
pub fn table_set_key(
    text: &str,
    path: &[TomlSeg],
    key: &str,
    value: Option<&Value>,
) -> Result<String, TomlError> {
    let mut doc = parse(text)?;
    let table = element(&mut doc, path)?;
    match value {
        None => {
            table.remove(key);
        }
        Some(value) => put(table, key, to_item(value)?),
    }
    Ok(doc.to_string())
}

/// Makes the table at `path` hold exactly the keys of `value`. A key whose value is the same
/// keeps its line and its comments. A key `value` lacks is removed.
pub fn table_merge(text: &str, path: &[TomlSeg], value: &Value) -> Result<String, TomlError> {
    let Value::Object(map) = value else {
        return Err(TomlError::Shape("a table needs an object".to_owned()));
    };
    let mut doc = parse(text)?;
    let table = element(&mut doc, path)?;
    let stale: Vec<String> = table
        .iter()
        .map(|(k, _)| k.to_owned())
        .filter(|k| !map.contains_key(k))
        .collect();
    for key in stale {
        table.remove(&key);
    }
    for (key, v) in map {
        if table.get(key).map(to_json).as_ref() != Some(v) {
            put(table, key, to_item(v)?);
        }
    }
    Ok(doc.to_string())
}

/// Removes the array-of-tables element that `path` ends on (`[..., Key, Index]`). An array left
/// empty is removed. A missing element leaves the text as it was.
pub fn element_remove(text: &str, path: &[TomlSeg]) -> Result<String, TomlError> {
    let (Some(TomlSeg::Index { index }), Some(TomlSeg::Key { name })) =
        (path.last(), path.len().checked_sub(2).map(|i| &path[i]))
    else {
        return Err(TomlError::Shape("the path must end on an index".to_owned()));
    };
    let mut doc = parse(text)?;
    let table = match element(&mut doc, &path[..path.len() - 2]) {
        Ok(table) => table,
        Err(_) => return Ok(text.to_owned()),
    };
    let Some(aot) = table.get_mut(name).and_then(Item::as_array_of_tables_mut) else {
        return Ok(text.to_owned());
    };
    if *index as usize >= aot.len() {
        return Ok(text.to_owned());
    }
    aot.remove(*index as usize);
    if aot.is_empty() {
        table.remove(name);
    }
    Ok(doc.to_string())
}

/// Replaces a value in place, keeping the comments on its line.
fn put(table: &mut dyn TableLike, key: &str, mut item: Item) {
    match (table.get(key), &mut item) {
        (Some(Item::Value(old)), Item::Value(new)) => *new.decor_mut() = old.decor().clone(),
        (Some(Item::Table(old)), Item::Table(new)) => *new.decor_mut() = old.decor().clone(),
        _ => {}
    }
    match table.get_mut(key) {
        Some(slot) => *slot = item,
        None => {
            table.insert(key, item);
        }
    }
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
    fn removing_a_key_that_set_created_leaves_no_header() {
        let made = set(
            "a = 1\n",
            &["hooks", "state", "k"],
            &json!({"enabled": false}),
        )
        .unwrap();
        assert_eq!(
            remove_pruned(&made, &["hooks", "state", "k", "enabled"]).unwrap(),
            "a = 1\n"
        );
        let kept = "[hooks.state.k]\ntrusted_hash = \"h\"   # keep\nenabled = false\n";
        assert_eq!(
            remove_pruned(kept, &["hooks", "state", "k", "enabled"]).unwrap(),
            "[hooks.state.k]\ntrusted_hash = \"h\"   # keep\n"
        );
    }

    #[test]
    fn an_element_of_a_nested_array_of_tables_is_merged_and_removed_by_path() {
        let text = "[[hooks.E]]\nmatcher = \"m\"\n[[hooks.E.hooks]]\ncommand = \"a\"  # c\ntimeout = 1\n[[hooks.E.hooks]]\ncommand = \"b\"\n";
        let k = |n: &str| TomlSeg::Key { name: n.into() };
        let i = |index: u32| TomlSeg::Index { index };
        let handler = vec![k("hooks"), k("E"), i(0), k("hooks"), i(0)];
        let merged = table_merge(text, &handler, &json!({"command": "a", "timeout": 2})).unwrap();
        assert!(merged.contains("command = \"a\"  # c") && merged.contains("timeout = 2"));
        let second = vec![k("hooks"), k("E"), i(0), k("hooks"), i(1)];
        let removed = element_remove(text, &second).unwrap();
        assert!(!removed.contains("\"b\"") && removed.contains("# c"));
        let group = vec![k("hooks"), k("E"), i(0)];
        assert_eq!(element_remove(text, &group).unwrap(), "");
        let set_matcher = table_set_key(text, &group, "matcher", None).unwrap();
        assert!(!set_matcher.contains("matcher"));
    }
}
