//! Slots, known projects, the trust guard, and the hook run helpers.

use std::collections::BTreeMap;

use serde_json::{Value as Json, json};

use super::out::Out;
use super::setup::{Setup, get_path};
use crate::agents::ScanCx;
use crate::model::*;
use crate::probe_fs::EntryKind;

pub(super) const RULE_GUARD: &str = "codex.guard.trusted_hash";
pub(super) const RULE_RUN_MCP: &str = "codex.hooks.run_mcp_unsupported";

fn slot(id: &str, place: Place, kind: NewItemKind, scope: Scope, rule: &str) -> Slot {
    Slot {
        id: SlotId(id.to_owned()),
        place,
        new_kind: kind,
        scope,
        rule: RuleRef::new(rule),
    }
}

/// Where a new item would load (spec 1.1, 5.1, 5.3, 3.2, 4).
pub(super) fn slots(setup: &Setup, out: &mut Out) {
    let user = Scope::User;
    let here = Scope::Project {
        dir: setup.cwd.clone(),
    };
    out.slots.push(slot(
        "codex.user.instructions",
        Place::File {
            path: setup.codex_home.join("AGENTS.md"),
        },
        NewItemKind::Instructions,
        user.clone(),
        "codex.slot.global_instructions",
    ));
    out.slots.push(slot(
        "codex.project.instructions",
        Place::File {
            path: setup.cwd.join("AGENTS.md"),
        },
        NewItemKind::Instructions,
        here.clone(),
        "codex.slot.project_instructions",
    ));
    if setup.md_root_found && setup.md_root != setup.cwd {
        out.slots.push(slot(
            "codex.root.instructions",
            Place::File {
                path: setup.md_root.join("AGENTS.md"),
            },
            NewItemKind::Instructions,
            Scope::Project {
                dir: setup.md_root.clone(),
            },
            "codex.slot.project_instructions",
        ));
    }
    out.slots.push(slot(
        "codex.user.skill",
        Place::NamedDir {
            dir: setup.codex_home.join("skills"),
            file: "SKILL.md".to_owned(),
        },
        NewItemKind::Skill,
        user.clone(),
        "codex.slot.skill",
    ));
    out.slots.push(slot(
        "codex.user.agents_skill",
        Place::NamedDir {
            dir: setup.home.join(".agents/skills"),
            file: "SKILL.md".to_owned(),
        },
        NewItemKind::Skill,
        user.clone(),
        "codex.slot.skill",
    ));
    out.slots.push(slot(
        "codex.project.skill",
        Place::NamedDir {
            dir: setup.cwd.join(".agents/skills"),
            file: "SKILL.md".to_owned(),
        },
        NewItemKind::Skill,
        here.clone(),
        "codex.slot.skill",
    ));
    out.slots.push(slot(
        "codex.user.subagent",
        Place::NamedFile {
            dir: setup.codex_home.join("agents"),
            ext: "toml".to_owned(),
        },
        NewItemKind::Subagent,
        user.clone(),
        "codex.slot.subagent",
    ));
    out.slots.push(slot(
        "codex.project.subagent",
        Place::NamedFile {
            dir: setup.cwd.join(".codex/agents"),
            ext: "toml".to_owned(),
        },
        NewItemKind::Subagent,
        here,
        "codex.slot.subagent_project",
    ));
    out.slots.push(slot(
        "codex.user.rule",
        Place::NamedFile {
            dir: setup.codex_home.join("rules"),
            ext: "rules".to_owned(),
        },
        NewItemKind::Rule,
        user.clone(),
        "codex.slot.rule",
    ));
    if let Some(store) = out.memory.first() {
        let place = match &store.write {
            MemoryWrite::NotesDir { dir } => Some(dir.clone()),
            _ => None,
        };
        if let Some(dir) = place {
            out.slots.push(slot(
                "codex.memory.note",
                Place::NamedFile {
                    dir,
                    ext: "md".to_owned(),
                },
                NewItemKind::Memory,
                user,
                "codex.slot.memory_note",
            ));
        }
    }
}

/// The projects in the user config's `[projects]` table (spec 6.3).
pub(super) fn known_projects(cx: &ScanCx, setup: &Setup) -> Vec<AbsPath> {
    let Some(table) = setup
        .user_layer()
        .config
        .as_ref()
        .and_then(|c| get_path(c, &["projects"]))
        .and_then(toml::Value::as_table)
    else {
        return Vec::new();
    };
    let mut projects: Vec<AbsPath> = table
        .keys()
        .filter_map(|key| AbsPath::parse(key).ok())
        .collect();
    projects.sort();
    projects.dedup();
    let _ = cx;
    projects
}

// ---------------------------------------------------------------------------------------------
// Guard (spec 2.5, 7.2)
// ---------------------------------------------------------------------------------------------

/// `hooks.state.<key>.trusted_hash` and `.trusted_content_hash`, by key.
fn trust_hashes(text: &str) -> Option<BTreeMap<(String, String), String>> {
    let table: toml::Table = toml::from_str(text).ok()?;
    let mut out = BTreeMap::new();
    if let Some(state) = get_path(&table, &["hooks", "state"]).and_then(toml::Value::as_table) {
        for (key, entry) in state {
            for field in ["trusted_hash", "trusted_content_hash"] {
                if let Some(value) = entry.get(field) {
                    out.insert((key.clone(), field.to_owned()), value.to_string());
                }
            }
        }
    }
    Some(out)
}

fn is_codex_config(path: &AbsPath) -> bool {
    path.file_name()
        .is_some_and(|n| n == "config.toml" || n.ends_with(".config.toml"))
}

/// Refuses any write that adds, changes or removes a hook trust hash. Writing one would
/// approve code on the user's behalf.
pub(super) fn guard(
    path: &AbsPath,
    before: Option<&str>,
    after: Option<&str>,
) -> Result<(), ApiError> {
    if !is_codex_config(path) {
        return Ok(());
    }
    let Some(after) = after else {
        // A deletion removes every hash the file held.
        return match before.and_then(trust_hashes) {
            Some(hashes) if !hashes.is_empty() => Err(forbidden()),
            _ => Ok(()),
        };
    };
    let Some(after_hashes) = trust_hashes(after) else {
        return Ok(());
    };
    let before_hashes = before.and_then(trust_hashes).unwrap_or_default();
    if after_hashes == before_hashes {
        Ok(())
    } else {
        Err(forbidden())
    }
}

fn forbidden() -> ApiError {
    ApiError::Forbidden {
        why: RuleRef::new(RULE_GUARD),
    }
}

// ---------------------------------------------------------------------------------------------
// Hook runs
// ---------------------------------------------------------------------------------------------

/// The input a hook of `event` receives (`codex-rs/hooks/schema/generated/*.input.schema.json`).
pub(super) fn sample_input(event: &str, cwd: &AbsPath) -> Json {
    let mut input = json!({
        "session_id": "00000000-0000-0000-0000-000000000000",
        "transcript_path": null,
        "cwd": cwd.as_str(),
        "hook_event_name": event,
        "model": "gpt-5",
        "permission_mode": "default",
    });
    let obj = input.as_object_mut().expect("object");
    let mut add = |pairs: Json| {
        if let Some(map) = pairs.as_object() {
            for (k, v) in map {
                obj.insert(k.clone(), v.clone());
            }
        }
    };
    let turn = json!({ "turn_id": "turn-0" });
    match event {
        "SessionStart" => add(json!({ "source": "startup" })),
        "SessionEnd" => add(json!({ "reason": "other" })),
        "PreToolUse" | "PermissionRequest" => add(json!({
            "tool_name": "Bash",
            "tool_input": { "command": "echo hello" },
            "tool_use_id": "call-0",
            "turn_id": "turn-0",
        })),
        "PostToolUse" => add(json!({
            "tool_name": "Bash",
            "tool_input": { "command": "echo hello" },
            "tool_response": { "output": "hello\n" },
            "tool_use_id": "call-0",
            "turn_id": "turn-0",
        })),
        "PreCompact" | "PostCompact" => add(json!({ "trigger": "manual", "turn_id": "turn-0" })),
        "UserPromptSubmit" => add(json!({ "prompt": "hello", "turn_id": "turn-0" })),
        "Stop" => add(json!({
            "stop_hook_active": false,
            "last_assistant_message": null,
            "turn_id": "turn-0",
        })),
        "SubagentStart" => add(json!({
            "agent_id": "agent-0",
            "agent_type": "default",
            "turn_id": "turn-0",
        })),
        "SubagentStop" => add(json!({
            "agent_id": "agent-0",
            "agent_type": "default",
            "agent_transcript_path": null,
            "last_assistant_message": null,
            "stop_hook_active": false,
            "turn_id": "turn-0",
        })),
        "PreModelRequest" | "PostModelRequest" | "Interrupt" => add(turn),
        _ => {}
    }
    input
}

/// The invocation Codex uses for a command hook: the login shell with `-lc`, the session cwd,
/// and for plugin hooks the `PLUGIN_ROOT` variables (spec 2.1).
pub(super) fn exec_spec(
    cx: &ScanCx,
    setup: &Setup,
    hook: &Hook,
    input: &Json,
) -> Result<ExecSpec, RuleRef> {
    if hook.handler.handler_kind != "command" {
        return Err(RuleRef::new(RULE_RUN_MCP));
    }
    let mut command = hook.handler.summary.clone();
    let mut env = Vec::new();
    if let Scope::Plugin { id } = &hook.source.scope {
        let plugins = super::plugins::installed(cx, setup);
        if let Some(plugin) = plugins.iter().find(|p| &p.id == id) {
            let root = plugin.root.to_string();
            let data = setup
                .codex_home
                .join("plugins/data")
                .join(&id.replace('@', "-"))
                .to_string();
            for (key, value) in [
                ("PLUGIN_ROOT", &root),
                ("CLAUDE_PLUGIN_ROOT", &root),
                ("PLUGIN_DATA", &data),
                ("CLAUDE_PLUGIN_DATA", &data),
            ] {
                command = command.replace(&format!("${{{key}}}"), value);
                env.push((key.to_owned(), value.clone()));
            }
        }
    }
    Ok(ExecSpec {
        argv: vec!["/bin/sh".to_owned(), "-lc".to_owned(), command],
        cwd: cx.env.cwd.clone(),
        env,
        stdin: input.to_string(),
    })
}

#[allow(dead_code)]
fn unused(_: EntryKind) {}
