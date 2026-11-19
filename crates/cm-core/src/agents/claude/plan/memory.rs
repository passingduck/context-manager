//! `AddMemory` and the trash of a memory topic (spec 2.6, 6.2): a topic file and its line in
//! `MEMORY.md` change together.

use super::{invalid, preview, read_text, takes_effect, write};
use crate::agents::PlanCx;
use crate::agents::claude::memory::link_targets;
use crate::model::*;

const AUTO_STORE: &str = "claude.auto";

/// What Claude Code's own writer does to a name (spec 2.6): lowercase, and every run of
/// characters outside `[a-z0-9]` becomes one `-`. A title with none of them, such as one in
/// Korean, gets a name from its hash so the file still has one.
pub(super) fn slug(title: &str) -> String {
    let mut out = String::new();
    let mut gap = false;
    for c in title.chars().flat_map(char::to_lowercase) {
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            if gap && !out.is_empty() {
                out.push('-');
            }
            gap = false;
            out.push(c);
        } else {
            gap = true;
        }
    }
    if out.is_empty() {
        out = format!("memory-{}", &sha256_hex(title.as_bytes())[..8]);
    }
    out
}

/// A YAML scalar that reads back as the same string. Plain when that is safe, else a JSON
/// string, which is a YAML double-quoted scalar.
pub(super) fn yaml_scalar(value: &str) -> String {
    let plain = !value.is_empty()
        && value.chars().all(|c| {
            c.is_alphanumeric() || matches!(c, ' ' | ',' | '.' | '(' | ')' | '/' | '_' | '-' | '\'')
        })
        && value.chars().next().is_some_and(char::is_alphanumeric)
        && !value.ends_with(' ')
        && value.parse::<f64>().is_err()
        && !matches!(
            value.to_ascii_lowercase().as_str(),
            "true" | "false" | "null" | "yes" | "no" | "on" | "off" | "y" | "n"
        );
    if plain {
        value.to_owned()
    } else {
        serde_json::to_string(value).expect("a string serializes")
    }
}

fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub(super) fn add(
    cx: &PlanCx,
    edit: &Edit,
    store_id: &str,
    title: &str,
    description: &str,
    body: &str,
) -> Result<Preview, ApiError> {
    let store = cx
        .snapshot
        .agents
        .iter()
        .filter(|view| view.agent == super::claude())
        .flat_map(|view| &view.memory)
        .find(|store| store.id == store_id)
        .ok_or_else(|| invalid("store", "claude.plan.store_unknown"))?;
    if store.id != AUTO_STORE {
        return Err(invalid("store", "claude.plan.store_agent_written"));
    }
    if let MemoryWrite::ReadOnly { why } = &store.write {
        return Err(ApiError::ReadOnly { why: why.clone() });
    }
    let title = one_line(title);
    if title.is_empty() {
        return Err(invalid("title", "claude.plan.title_invalid"));
    }
    let name = slug(&title);
    let topic = store.root.join(&format!("{name}.md"));
    if cx.fs.stat(&topic).is_ok() {
        return Err(invalid("title", "claude.plan.name_exists"));
    }
    let description = one_line(description);
    let hook = if description.is_empty() {
        title.clone()
    } else {
        description.clone()
    };

    // Spec 2.6: the shape Claude Code writes, without a session id.
    let mut file = format!("---\nname: {}\n", yaml_scalar(&name));
    file.push_str(&format!("description: {}\n", yaml_scalar(&description)));
    file.push_str("metadata:\n  node_type: memory\n  type: project\n---\n\n");
    file.push_str(body.trim());
    file.push('\n');

    let index = store
        .index
        .clone()
        .unwrap_or_else(|| store.root.join("MEMORY.md"));
    let line = format!(
        "- [{}]({name}.md) \u{2014} {hook}\n",
        title.replace('[', "(").replace(']', ")")
    );
    let (index_text, index_base) = match read_text(cx, &index)? {
        Some((mut text, base)) => {
            if !text.is_empty() && !text.ends_with('\n') {
                text.push('\n');
            }
            text.push_str(&line);
            (text, Some(base))
        }
        None => (line, None),
    };
    Ok(preview(
        edit,
        vec![
            write(&topic, None, file),
            write(&index, index_base.as_ref(), index_text),
        ],
        vec![
            takes_effect("claude.takes_effect.next_session"),
            takes_effect("claude.takes_effect.memory_topic"),
        ],
    ))
}

/// Trashing a topic file of the main store also removes its lines from the index. Any other file
/// is the generic plan's.
pub(super) fn trash(
    cx: &PlanCx,
    edit: &Edit,
    path: &AbsPath,
    base: &Revision,
) -> Option<Result<Preview, ApiError>> {
    let store = cx
        .snapshot
        .agents
        .iter()
        .filter(|view| view.agent == super::claude())
        .flat_map(|view| &view.memory)
        .find(|store| {
            store.id == AUTO_STORE
                && store
                    .entries
                    .iter()
                    .any(|e| e.file == *path && e.role == MemoryRole::Topic)
        })?;
    Some(trash_topic(cx, edit, store, path, base))
}

fn trash_topic(
    cx: &PlanCx,
    edit: &Edit,
    store: &MemoryStore,
    path: &AbsPath,
    base: &Revision,
) -> Result<Preview, ApiError> {
    let Some((_, _)) = read_text(cx, path)? else {
        return Ok(preview(edit, Vec::new(), Vec::new()));
    };
    let mut ops = vec![FileOp {
        path: path.clone(),
        expect: Base::Rev { rev: base.clone() },
        action: Action::Trash,
    }];
    let name = path.file_name().unwrap_or("");
    if let Some(index) = &store.index
        && let Some((text, index_base)) = read_text(cx, index)?
    {
        let lines: Vec<u32> = link_targets(&text)
            .iter()
            .filter(|link| link.target == name)
            .map(|link| link.line)
            .collect();
        if !lines.is_empty() {
            let kept: String = text
                .split_inclusive('\n')
                .enumerate()
                .filter(|(at, _)| !lines.contains(&(*at as u32 + 1)))
                .map(|(_, line)| line)
                .collect();
            ops.push(write(index, Some(&index_base), kept));
        }
    }
    Ok(preview(edit, ops, Vec::new()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_title_becomes_the_slug_claude_code_would_write() {
        assert_eq!(slug("Prefer Small PRs!"), "prefer-small-prs");
        assert_eq!(slug("  --a__b  "), "a-b");
        assert!(slug("작은 PR 선호").starts_with("pr") || slug("작은 PR 선호") == "pr");
        assert!(slug("선호").starts_with("memory-"));
    }

    #[test]
    fn a_description_that_yaml_would_misread_is_quoted() {
        assert_eq!(yaml_scalar("Plain words, fine"), "Plain words, fine");
        assert_eq!(yaml_scalar("key: value"), "\"key: value\"");
        assert_eq!(yaml_scalar("123"), "\"123\"");
        assert_eq!(yaml_scalar("yes"), "\"yes\"");
        assert_eq!(yaml_scalar("#tag"), "\"#tag\"");
        assert_eq!(yaml_scalar(""), "\"\"");
    }
}
