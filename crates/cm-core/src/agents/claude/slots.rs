//! Where a new item would load (spec 1.1, 2.1, 4).

use super::memory::MemDir;
use super::setup::Setup;
use crate::agents::ScanCx;
use crate::model::*;

pub(super) fn slots(_cx: &ScanCx, setup: &Setup, memdir: &MemDir) -> Vec<Slot> {
    let project = Scope::Project {
        dir: setup.cwd.clone(),
    };
    let cwd = &setup.cwd;
    let config = &setup.config;
    let mut out = Vec::new();
    let mut add = |id: &str, place: Place, new_kind: NewItemKind, scope: Scope, rule: &str| {
        out.push(Slot {
            id: SlotId(format!("claude:{id}")),
            place,
            new_kind,
            scope,
            rule: RuleRef::new(rule),
        });
    };
    // Spec 1.1: instruction files.
    add(
        "instructions:project",
        Place::File {
            path: cwd.join("CLAUDE.md"),
        },
        NewItemKind::Instructions,
        project.clone(),
        "claude.slot.project_claude_md",
    );
    add(
        "instructions:project-dot",
        Place::File {
            path: cwd.join(".claude/CLAUDE.md"),
        },
        NewItemKind::Instructions,
        project.clone(),
        "claude.slot.project_dotclaude_md",
    );
    add(
        "instructions:local",
        Place::File {
            path: cwd.join("CLAUDE.local.md"),
        },
        NewItemKind::Instructions,
        Scope::Local,
        "claude.slot.local_claude_md",
    );
    add(
        "instructions:user",
        Place::File {
            path: config.join("CLAUDE.md"),
        },
        NewItemKind::Instructions,
        Scope::User,
        "claude.slot.user_claude_md",
    );
    // Spec 1.4: rules.
    add(
        "rule:project",
        Place::NamedFile {
            dir: cwd.join(".claude/rules"),
            ext: "md".to_owned(),
        },
        NewItemKind::Rule,
        project.clone(),
        "claude.slot.project_rule",
    );
    add(
        "rule:user",
        Place::NamedFile {
            dir: config.join("rules"),
            ext: "md".to_owned(),
        },
        NewItemKind::Rule,
        Scope::User,
        "claude.slot.user_rule",
    );
    // Spec 4: skills, commands, subagents.
    add(
        "skill:project",
        Place::NamedDir {
            dir: cwd.join(".claude/skills"),
            file: "SKILL.md".to_owned(),
        },
        NewItemKind::Skill,
        project.clone(),
        "claude.slot.project_skill",
    );
    add(
        "skill:user",
        Place::NamedDir {
            dir: config.join("skills"),
            file: "SKILL.md".to_owned(),
        },
        NewItemKind::Skill,
        Scope::User,
        "claude.slot.user_skill",
    );
    add(
        "command:project",
        Place::NamedFile {
            dir: cwd.join(".claude/commands"),
            ext: "md".to_owned(),
        },
        NewItemKind::Command,
        project.clone(),
        "claude.slot.project_command",
    );
    add(
        "command:user",
        Place::NamedFile {
            dir: config.join("commands"),
            ext: "md".to_owned(),
        },
        NewItemKind::Command,
        Scope::User,
        "claude.slot.user_command",
    );
    add(
        "subagent:project",
        Place::NamedFile {
            dir: cwd.join(".claude/agents"),
            ext: "md".to_owned(),
        },
        NewItemKind::Subagent,
        project,
        "claude.slot.project_subagent",
    );
    add(
        "subagent:user",
        Place::NamedFile {
            dir: config.join("agents"),
            ext: "md".to_owned(),
        },
        NewItemKind::Subagent,
        Scope::User,
        "claude.slot.user_subagent",
    );
    // Spec 2.1: a topic memory file.
    add(
        "memory:auto",
        Place::NamedFile {
            dir: memdir.dir.clone(),
            ext: "md".to_owned(),
        },
        NewItemKind::Memory,
        Scope::User,
        "claude.slot.memory_topic",
    );
    out
}
