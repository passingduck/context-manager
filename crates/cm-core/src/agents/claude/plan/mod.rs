//! What the Claude adapter plans for the edits that mean something to Claude Code: a new item in
//! its slot, a memory with its index line, and hooks (spec 2.6, 3, 6.2). A plan returns only
//! operations, warnings and conflicts. The core recomputes `before` and the digest and checks
//! the boundary, the base of every operation and the guard.

mod hook_text;
mod hooks;
mod memory;
mod new_item;
mod splice;

use crate::agents::PlanCx;
use crate::edit::{change_edit_of, digest_of};
use crate::model::*;

use hook_text::invalid;

pub(super) fn claude() -> AgentId {
    AgentId::builtin("claude")
}

pub(super) fn plan(cx: &PlanCx, edit: &Edit) -> Option<Result<Preview, ApiError>> {
    match edit {
        Edit::NewItem { agent, slot, name } if *agent == claude() => {
            Some(new_item::plan(cx, edit, slot, name.as_deref()))
        }
        Edit::AddMemory {
            agent,
            store,
            title,
            description,
            body,
        } if *agent == claude() => Some(memory::add(cx, edit, store, title, description, body)),
        Edit::Trash { path, base } => memory::trash(cx, edit, path, base),
        Edit::AddHook { agent, file, draft } if *agent == claude() => {
            Some(hooks::add(cx, edit, file, draft))
        }
        Edit::UpdateHook { hook, draft } => hooks::update(cx, edit, hook, draft),
        Edit::RemoveHook { hook } => hooks::remove(cx, edit, hook),
        Edit::SetHookEnabled { hook, enabled } => hooks::set_enabled(cx, edit, hook, *enabled),
        _ => None,
    }
}

/// The text of a file and the base an operation on it expects. `None` when it is absent.
pub(super) fn read_text(cx: &PlanCx, path: &AbsPath) -> Result<Option<(String, Base)>, ApiError> {
    match cx.fs.stat(path) {
        Err(err) if err.is_not_found() => return Ok(None),
        Err(err) => return Err(ApiError::Io { detail: err.detail }),
        Ok(_) => {}
    }
    match cx.fs.read(path) {
        Err(err) if err.is_not_found() => Ok(None),
        Err(err) => Err(ApiError::Io { detail: err.detail }),
        Ok(bytes) => match String::from_utf8(bytes.as_ref().clone()) {
            Ok(text) => {
                let rev = Revision::of(text.as_bytes());
                Ok(Some((text, Base::Rev { rev })))
            }
            Err(_) => Err(ApiError::ReadOnly {
                why: RuleRef::new("edit.read_only.not_utf8"),
            }),
        },
    }
}

/// A write that creates the file or replaces the text it was read as.
pub(super) fn write(path: &AbsPath, was: Option<&Base>, text: String) -> FileOp {
    FileOp {
        path: path.clone(),
        expect: was.cloned().unwrap_or(Base::Absent),
        action: Action::Write { text },
    }
}

pub(super) fn preview(edit: &Edit, ops: Vec<FileOp>, warnings: Vec<Warning>) -> Preview {
    let change_set = ChangeSet {
        summary: ChangeSummary {
            edit: change_edit_of(edit),
            agent: Some(claude()),
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

pub(super) fn takes_effect(rule: &str) -> Warning {
    Warning::TakesEffect {
        agent: claude(),
        when: RuleRef::new(rule),
    }
}

#[cfg(test)]
mod tests;
