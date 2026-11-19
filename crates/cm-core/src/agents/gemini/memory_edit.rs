//! Private project memory (spec 4): a note in `~/.gemini/tmp/<slug>/memory/` and its line in
//! `MEMORY.md`. The folder belongs to a project slug that only Gemini CLI creates, in
//! `projects.json`, so a project without one has no store to write to.

use super::memory::INDEX;
use super::plan::{exists, invalid, preview, read, takes_effect, view, write_op};
use crate::agents::PlanCx;
use crate::model::*;

pub(super) fn add(
    cx: &PlanCx,
    store: &str,
    title: &str,
    description: &str,
    body: &str,
) -> Result<Preview, ApiError> {
    let view = view(cx)?;
    let Some(store) = view.memory.iter().find(|s| s.id == store) else {
        return Err(if store == "gemini.private" {
            // Spec 4, slug row: the CLI creates the folder and the entry on its first run.
            invalid("store", "gemini.memory.no_slug")
        } else {
            invalid("store", "gemini.edit.unknown_store")
        });
    };
    if !matches!(store.write, MemoryWrite::Direct) {
        return Err(ApiError::ReadOnly {
            why: RuleRef::new("gemini.state_file"),
        });
    }
    let title = one_line(title);
    if title.is_empty() {
        return Err(invalid("title", "gemini.memory.title_required"));
    }
    let description = one_line(description);

    let stem = slug(&title);
    let mut n = 1;
    let note = loop {
        let name = if n == 1 {
            format!("{stem}.md")
        } else {
            format!("{stem}-{n}.md")
        };
        let path = store.root.join(&name);
        if !exists(cx, &path)? {
            break path;
        }
        n += 1;
        if n > 1000 {
            return Err(invalid("title", "gemini.edit.name_taken"));
        }
    };
    let file_name = note.file_name().unwrap_or_default().to_owned();

    let mut text = format!(
        "---\nname: {}\ndescription: {}\n---\n",
        quote(&title),
        quote(&description)
    );
    text.push_str(&format!("# {title}\n"));
    if !body.trim().is_empty() {
        text.push('\n');
        text.push_str(body.trim_end_matches('\n'));
        text.push('\n');
    }

    let index_path = store
        .index
        .clone()
        .unwrap_or_else(|| store.root.join(INDEX));
    let index_disk = read(cx, &index_path)?;
    let mut index = index_disk.clone().unwrap_or_default();
    if !index.is_empty() && !index.ends_with('\n') {
        index.push('\n');
    }
    index.push_str(&index_line(&title, &file_name, &description));
    index.push('\n');

    Ok(preview(
        ChangeEdit::AddMemory,
        vec![
            write_op(&note, &None, text),
            write_op(&index_path, &index_disk, index),
        ],
        vec![
            takes_effect("gemini.takes_effect.memory_reload"),
            Warning::AgentAlsoWrites {
                agent: super::plan::id(),
            },
        ],
    ))
}

/// Trashing a note also removes its line from the index, in the same plan. `None`: not a note
/// of a store, so the generic trash applies.
pub(super) fn trash(
    cx: &PlanCx,
    path: &AbsPath,
    base: &Revision,
) -> Option<Result<Preview, ApiError>> {
    let view = view(cx).ok()?;
    let store = view.memory.iter().find(|s| {
        s.entries
            .iter()
            .any(|e| e.file == *path && e.role == MemoryRole::Note)
    })?;
    Some(trash_note(cx, store, path, base))
}

fn trash_note(
    cx: &PlanCx,
    store: &MemoryStore,
    path: &AbsPath,
    base: &Revision,
) -> Result<Preview, ApiError> {
    if read(cx, path)?.is_none() {
        return Ok(preview(ChangeEdit::Trash, Vec::new(), Vec::new()));
    }
    let mut ops = vec![FileOp {
        path: path.clone(),
        expect: Base::Rev { rev: base.clone() },
        action: Action::Trash,
    }];
    let file_name = path.file_name().unwrap_or_default();
    if let Some(index_path) = &store.index
        && let Some(index) = read(cx, index_path)?
    {
        let kept = without_line_for(&index, file_name);
        if kept != index {
            ops.push(write_op(index_path, &Some(index), kept));
        }
    }
    Ok(preview(
        ChangeEdit::Trash,
        ops,
        vec![takes_effect("gemini.takes_effect.memory_reload")],
    ))
}

/// Every index line that links to `file_name` goes.
fn without_line_for(index: &str, file_name: &str) -> String {
    let target = format!("]({file_name})");
    index
        .split_inclusive('\n')
        .filter(|line| !(line.trim_start().starts_with('-') && line.contains(&target)))
        .collect()
}

fn index_line(title: &str, file: &str, description: &str) -> String {
    let title = title.replace(['[', ']'], "");
    if description.is_empty() {
        format!("- [{title}]({file})")
    } else {
        format!("- [{title}]({file}) \u{2014} {description}")
    }
}

fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// A YAML scalar that cannot be misread: always double-quoted.
fn quote(text: &str) -> String {
    format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""))
}

/// Lowercase ASCII words joined by `-`. A title with none (Korean, for one) is `note`. `memory`
/// would collide with `MEMORY.md` on a case-insensitive disk.
fn slug(title: &str) -> String {
    let mut out = String::new();
    for ch in title.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    let out = out.trim_matches('-');
    match out {
        "" => "note".to_owned(),
        "memory" => "memory-note".to_owned(),
        other => other
            .chars()
            .take(60)
            .collect::<String>()
            .trim_end_matches('-')
            .to_owned(),
    }
}
