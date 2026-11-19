//! The edits that mean something specific to Gemini CLI (design section 4): a new item, a
//! memory note, hooks and the hook switch. Each plan returns only operations and warnings. The
//! core recomputes `before` and the digest and re-checks the boundary, the base and the guard.

use super::util::rule;
use super::{Gemini, hook_edit, memory_edit};
use crate::agents::{Agent, PlanCx};
use crate::edit::digest_of;
use crate::model::*;

pub(super) fn id() -> AgentId {
    Gemini.id()
}

/// `None` leaves the edit to the generic plan or to another agent.
pub(super) fn plan(cx: &PlanCx, edit: &Edit) -> Option<Result<Preview, ApiError>> {
    match edit {
        Edit::NewItem { agent, slot, name } if *agent == id() => {
            Some(new_item(cx, slot, name.as_deref()))
        }
        Edit::AddMemory {
            agent,
            store,
            title,
            description,
            body,
        } if *agent == id() => Some(memory_edit::add(cx, store, title, description, body)),
        Edit::AddHook { agent, file, draft } if *agent == id() => {
            Some(hook_edit::add(cx, file, draft))
        }
        Edit::UpdateHook { hook, draft } if is_ours(hook) => {
            Some(hook_edit::update(cx, hook, draft))
        }
        Edit::RemoveHook { hook } if is_ours(hook) => Some(hook_edit::remove(cx, hook)),
        Edit::SetHookEnabled { hook, enabled } if is_ours(hook) => {
            Some(hook_edit::set_enabled(cx, hook, *enabled))
        }
        Edit::Trash { path, base } => memory_edit::trash(cx, path, base),
        _ => None,
    }
}

/// A hook id of this adapter starts with `gemini:` (see `hooks::hook_id`).
fn is_ours(hook: &HookId) -> bool {
    hook.0.starts_with("gemini:")
}

pub(super) fn view<'a>(cx: &'a PlanCx) -> Result<&'a AgentView, ApiError> {
    cx.snapshot
        .agents
        .iter()
        .find(|v| v.agent == id())
        .ok_or_else(|| invalid("agent", "edit.unknown_agent"))
}

pub(super) fn invalid(field: &str, why: &str) -> ApiError {
    ApiError::InvalidEdit {
        field: field.to_owned(),
        why: RuleRef::new(why),
    }
}

/// The file's text, `None` when it does not exist.
pub(super) fn read(cx: &PlanCx, path: &AbsPath) -> Result<Option<String>, ApiError> {
    match cx.fs.read(path) {
        Err(err) if err.is_not_found() => Ok(None),
        Err(err) => Err(ApiError::Io { detail: err.detail }),
        Ok(bytes) => String::from_utf8(bytes.as_ref().clone())
            .map(Some)
            .map_err(|_| ApiError::ReadOnly {
                why: RuleRef::new("edit.read_only.not_utf8"),
            }),
    }
}

pub(super) fn exists(cx: &PlanCx, path: &AbsPath) -> Result<bool, ApiError> {
    match cx.fs.stat(path) {
        Ok(_) => Ok(true),
        Err(err) if err.is_not_found() => Ok(false),
        Err(err) => Err(ApiError::Io { detail: err.detail }),
    }
}

pub(super) fn base_of(disk: &Option<String>) -> Base {
    match disk {
        Some(text) => Base::Rev {
            rev: Revision::of(text.as_bytes()),
        },
        None => Base::Absent,
    }
}

pub(super) fn write_op(path: &AbsPath, disk: &Option<String>, text: String) -> FileOp {
    FileOp {
        path: path.clone(),
        expect: base_of(disk),
        action: Action::Write { text },
    }
}

pub(super) fn takes_effect(when: &'static str) -> Warning {
    Warning::TakesEffect {
        agent: id(),
        when: rule(when),
    }
}

pub(super) fn preview(edit: ChangeEdit, ops: Vec<FileOp>, warnings: Vec<Warning>) -> Preview {
    let change_set = ChangeSet {
        summary: ChangeSummary {
            edit,
            agent: Some(id()),
            targets: ops.iter().map(|op| op.path.clone()).collect(),
        },
        ops,
    };
    Preview {
        digest: digest_of(&change_set),
        change_set,
        before: Default::default(),
        warnings,
        conflicts: Vec::new(),
    }
}

/// Spec 6: a name becomes a file or a folder name, and a subagent name that starts with `_` is
/// skipped by the CLI.
fn valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphanumeric())
        && name.len() <= 64
        && !name.ends_with('.')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
}

fn new_item(cx: &PlanCx, slot: &SlotId, name: Option<&str>) -> Result<Preview, ApiError> {
    let slot = view(cx)?
        .slots
        .iter()
        .find(|s| s.id == *slot)
        .ok_or_else(|| invalid("slot", "gemini.edit.unknown_slot"))?;
    let skeleton_for = |name: &str| -> Result<String, ApiError> {
        match slot.new_kind {
            NewItemKind::Instructions => Ok(String::new()),
            // Spec 6, custom commands: `prompt` is required, `description` is optional.
            NewItemKind::Command => Ok("description = \"\"\nprompt = \"\"\n".to_owned()),
            // Spec 6, skills and subagents: string `name` and `description` in frontmatter.
            NewItemKind::Skill | NewItemKind::Subagent => {
                Ok(format!("---\nname: {name}\ndescription: \"\"\n---\n"))
            }
            NewItemKind::Memory => Err(invalid("slot", "gemini.edit.use_add_memory")),
            NewItemKind::Rule => Err(invalid("slot", "gemini.edit.slot_unsupported")),
        }
    };
    let named = || -> Result<&str, ApiError> {
        let name = name
            .map(str::trim)
            .filter(|n| !n.is_empty())
            .ok_or_else(|| invalid("name", "gemini.edit.name_required"))?;
        if valid_name(name) {
            Ok(name)
        } else {
            Err(invalid("name", "gemini.edit.name_invalid"))
        }
    };
    let (path, label) = match &slot.place {
        Place::File { path } => (
            path.clone(),
            path.file_name().unwrap_or_default().to_owned(),
        ),
        Place::NamedFile { dir, ext } => {
            let name = named()?;
            (
                dir.join(&format!("{name}.{}", ext.trim_start_matches('.'))),
                name.to_owned(),
            )
        }
        Place::NamedDir { dir, file } => {
            let name = named()?;
            (dir.join(name).join(file), name.to_owned())
        }
    };
    let text = skeleton_for(&label)?;
    if exists(cx, &path)? {
        return Err(invalid("name", "gemini.edit.name_taken"));
    }
    let when = match slot.new_kind {
        NewItemKind::Instructions => "gemini.takes_effect.memory_reload",
        _ => "gemini.takes_effect.restart",
    };
    Ok(preview(
        ChangeEdit::NewItem,
        vec![write_op(&path, &None, text)],
        vec![takes_effect(when)],
    ))
}
