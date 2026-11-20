//! `NewItem` (spec 1.1, 1.4, 2.1, 4): a new file in the slot the operator picked, with the
//! smallest valid skeleton for its kind.

use super::memory::{self, yaml_scalar};
use super::{invalid, preview, takes_effect, write};
use crate::agents::PlanCx;
use crate::model::*;

/// A name that is one path segment and that Claude Code's loaders accept: no separator, no
/// leading `-` or `.`, no `:` and no `<` or `>` (spec 4, Frontmatter and skipped files).
fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        && !name.starts_with(['-', '.'])
        && !name.contains("..")
}

pub(super) fn plan(
    cx: &PlanCx,
    edit: &Edit,
    slot: &SlotId,
    name: Option<&str>,
) -> Result<Preview, ApiError> {
    let slot = cx
        .snapshot
        .agents
        .iter()
        .filter(|view| view.agent == super::claude())
        .flat_map(|view| &view.slots)
        .find(|s| s.id == *slot)
        .ok_or_else(|| invalid("slot", "claude.plan.slot_unknown"))?;
    if slot.new_kind == NewItemKind::Memory {
        let name = name
            .map(str::trim)
            .filter(|n| !n.is_empty())
            .ok_or_else(|| invalid("name", "claude.plan.name_required"))?;
        return memory::add(cx, edit, "claude.auto", name, name, "");
    }
    let name = match &slot.place {
        Place::File { .. } => None,
        _ => {
            let raw = name
                .map(str::trim)
                .filter(|n| !n.is_empty())
                .ok_or_else(|| invalid("name", "claude.plan.name_required"))?;
            let name = raw.strip_suffix(".md").unwrap_or(raw);
            if !valid_name(name) {
                return Err(invalid("name", "claude.plan.name_invalid"));
            }
            Some(name)
        }
    };
    let path = match (&slot.place, name) {
        (Place::File { path }, _) => path.clone(),
        (Place::NamedFile { dir, ext }, Some(name)) => {
            dir.join(&format!("{name}.{}", ext.trim_start_matches('.')))
        }
        (Place::NamedDir { dir, file } | Place::InSubdir { dir, file }, Some(name)) => {
            dir.join(name).join(file)
        }
        _ => return Err(invalid("name", "claude.plan.name_required")),
    };
    // A skill folder that already exists collides even when its entry file is missing.
    let taken = match &slot.place {
        Place::NamedDir { dir, .. } => cx.fs.stat(&dir.join(name.unwrap_or_default())).is_ok(),
        _ => cx.fs.stat(&path).is_ok(),
    };
    if taken {
        return Err(invalid(
            if name.is_some() { "name" } else { "slot" },
            "claude.plan.name_exists",
        ));
    }
    let described = |name: &str| format!("Describe what {name} does and when to use it");
    let text = match (slot.new_kind, name) {
        // An instruction file starts empty, a rule too: it loads at session start until it has paths.
        (NewItemKind::Instructions | NewItemKind::Rule, _) => String::new(),
        (NewItemKind::Skill | NewItemKind::Subagent, Some(name)) => format!(
            "---\nname: {}\ndescription: {}\n---\n\n",
            yaml_scalar(name),
            yaml_scalar(&described(name))
        ),
        // A command takes its name from the file, so it has no `name` key.
        (NewItemKind::Command, Some(name)) => {
            format!(
                "---\ndescription: {}\n---\n\n",
                yaml_scalar(&described(name))
            )
        }
        _ => return Err(invalid("slot", "claude.plan.slot_unknown")),
    };
    let when = match slot.new_kind {
        NewItemKind::Skill | NewItemKind::Command | NewItemKind::Subagent => {
            "claude.takes_effect.live_reload"
        }
        _ => "claude.takes_effect.next_session",
    };
    Ok(preview(
        edit,
        vec![write(&path, None, text)],
        vec![takes_effect(when)],
    ))
}
