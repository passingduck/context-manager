//! Where a new context file, command, skill or subagent would load (design section 5).

use super::world::World;
use crate::model::*;

fn slot(id: &str, place: Place, new_kind: NewItemKind, scope: Scope, rule: &'static str) -> Slot {
    Slot {
        id: SlotId(format!("gemini:{id}")),
        place,
        new_kind,
        scope,
        rule: super::util::rule(rule),
    }
}

pub(super) fn slots(w: &World) -> Vec<Slot> {
    let project = Scope::Project { dir: w.cwd.clone() };
    // The CLI reads and writes the first configured name in `~/.gemini` (spec 2, 8).
    let first = w.names.first().map_or("GEMINI.md", String::as_str);
    let mut slots = vec![
        slot(
            "instructions:user",
            Place::File {
                path: w.gdir.join(first),
            },
            NewItemKind::Instructions,
            Scope::User,
            "gemini.slot.global_context",
        ),
        slot(
            "instructions:project",
            Place::File {
                path: w.cwd.join("GEMINI.md"),
            },
            NewItemKind::Instructions,
            project.clone(),
            "gemini.slot.project_context",
        ),
        slot(
            "instructions:subdir",
            Place::InSubdir {
                dir: w.cwd.clone(),
                file: "GEMINI.md".to_owned(),
            },
            NewItemKind::Instructions,
            project.clone(),
            "gemini.slot.subdir_context",
        ),
        slot(
            "command:user",
            Place::NamedFile {
                dir: w.gdir.join("commands"),
                ext: "toml".to_owned(),
            },
            NewItemKind::Command,
            Scope::User,
            "gemini.slot.command",
        ),
        slot(
            "skill:user",
            Place::NamedDir {
                dir: w.gdir.join("skills"),
                file: "SKILL.md".to_owned(),
            },
            NewItemKind::Skill,
            Scope::User,
            "gemini.slot.skill",
        ),
        slot(
            "subagent:user",
            Place::NamedFile {
                dir: w.gdir.join("agents"),
                ext: "md".to_owned(),
            },
            NewItemKind::Subagent,
            Scope::User,
            "gemini.slot.subagent",
        ),
    ];
    // Project items load only from a folder that is not the home directory (spec 6).
    if !w.cwd_is_home {
        slots.extend([
            slot(
                "command:project",
                Place::NamedFile {
                    dir: w.cwd.join(".gemini/commands"),
                    ext: "toml".to_owned(),
                },
                NewItemKind::Command,
                project.clone(),
                "gemini.slot.command",
            ),
            slot(
                "skill:project",
                Place::NamedDir {
                    dir: w.cwd.join(".gemini/skills"),
                    file: "SKILL.md".to_owned(),
                },
                NewItemKind::Skill,
                project.clone(),
                "gemini.slot.skill",
            ),
            slot(
                "subagent:project",
                Place::NamedFile {
                    dir: w.cwd.join(".gemini/agents"),
                    ext: "md".to_owned(),
                },
                NewItemKind::Subagent,
                project,
                "gemini.slot.subagent",
            ),
        ]);
    }
    if let Some(dir) = super::memory::memory_dir(w) {
        slots.push(slot(
            "memory:user",
            Place::NamedFile {
                dir,
                ext: "md".to_owned(),
            },
            NewItemKind::Memory,
            Scope::User,
            "gemini.slot.memory",
        ));
    }
    slots
}
