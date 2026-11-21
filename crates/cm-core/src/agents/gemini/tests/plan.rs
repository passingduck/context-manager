//! Planner tests for Gemini CLI. They go through the core `Planner`, so the boundary, the base
//! check and the guard run as they do in the host, and "apply" writes what the plan says.

use serde_json::{Value, json};

use super::*;
use crate::agents::Agent;
use crate::cfg::jsonc;
use crate::edit::{ChangeSource, Planner};
use crate::scan::scan_with;

struct NoHistory;
impl ChangeSource for NoHistory {
    fn detail(&self, _: &ChangeId) -> Result<ChangeDetail, ApiError> {
        Err(ApiError::NotFound)
    }
}

fn plan(fx: &Fixture, env: &ScanEnv, edit: &Edit) -> Result<Preview, ApiError> {
    let scanned = scan_with(&[&Gemini as &dyn Agent], env);
    let parked = fx.root().join("data/parked");
    Planner {
        agents: &[&Gemini as &dyn Agent],
        scanned: &scanned,
        env,
        parked_dir: &parked,
        changes: &NoHistory,
    }
    .plan(edit)
}

fn apply(preview: &Preview) {
    for op in &preview.change_set.ops {
        match &op.action {
            Action::Write { text } => {
                std::fs::create_dir_all(op.path.parent().unwrap().as_path()).unwrap();
                std::fs::write(op.path.as_path(), text).unwrap();
            }
            Action::Trash => std::fs::remove_file(op.path.as_path()).unwrap(),
            Action::TrashDir => unreachable!(),
        }
    }
}

fn read(path: &AbsPath) -> String {
    std::fs::read_to_string(path.as_path()).unwrap()
}

fn settings(fx: &Fixture) -> AbsPath {
    fx.home_path(".gemini/settings.json")
}

fn hook_id(env: &ScanEnv, command: &str) -> HookId {
    scan(env)
        .view
        .hooks
        .iter()
        .find(|h| h.handler.summary == command)
        .unwrap_or_else(|| panic!("no hook {command}"))
        .id
        .clone()
}

fn draft(event: &str, matcher: Option<&str>, handler: Value) -> HookDraft {
    HookDraft {
        event: event.to_owned(),
        matcher: matcher.map(str::to_owned),
        handler,
    }
}

fn add(fx: &Fixture, env: &ScanEnv, d: HookDraft) -> Preview {
    plan(
        fx,
        env,
        &Edit::AddHook {
            agent: Gemini.id(),
            file: settings(fx),
            draft: d,
        },
    )
    .unwrap()
}

/// Odd on purpose: tab indentation, comments in every position, an inline array and a key that
/// follows the hooks.
const ODD: &str = "// my settings\n{\n\t/* theme */ \"ui\": { \"theme\": \"dark\" }, // keep\n\t\"hooks\": {\n\t\t// before tools\n\t\t\"BeforeTool\": [\n\t\t\t{ \"matcher\": \"^write\", \"hooks\": [ { \"type\": \"command\", \"command\": \"keep.sh\" } ] }\n\t\t]\n\t},\n\t\"hooksConfig\": { \"notifications\": true }, // tail\n\t\"other\": [1, 2, 3]\n}\n";

fn is_valid_gemini_json(text: &str) {
    assert!(
        jsonc::trailing_commas(text).is_empty(),
        "trailing comma in\n{text}"
    );
    assert!(jsonc::get(text, "").unwrap().is_some(), "parses\n{text}");
}

#[test]
fn adding_then_removing_a_hook_leaves_every_other_byte_alone() {
    let fx = Fixture::new();
    fx.write(&settings(&fx), ODD);
    let env = fx.env();

    // A new event and a new group in an existing event.
    for (event, matcher) in [("AfterTool", None), ("BeforeTool", Some("^read"))] {
        let preview = add(
            &fx,
            &env,
            draft(
                event,
                matcher,
                json!({"type": "command", "command": "new.sh", "name": "n", "extra": {"a": 1}}),
            ),
        );
        assert_eq!(preview.change_set.ops.len(), 1);
        let added = match &preview.change_set.ops[0].action {
            Action::Write { text } => text.clone(),
            _ => panic!(),
        };
        is_valid_gemini_json(&added);
        for kept in [
            "// my settings",
            "/* theme */",
            "// keep",
            "// before tools",
            "// tail",
            "[1, 2, 3]",
            "keep.sh",
        ] {
            assert!(added.contains(kept), "{kept} lost in\n{added}");
        }
        apply(&preview);
        let rescanned = scan(&env);
        let hook = rescanned
            .view
            .hooks
            .iter()
            .find(|h| h.handler.summary == "new.sh")
            .unwrap();
        assert_eq!(hook.event, event);
        assert_eq!(hook.matcher.raw.as_deref(), matcher);
        // Unknown handler keys are written as given.
        assert_eq!(hook.handler.raw["extra"], json!({"a": 1}));

        let removed = plan(
            &fx,
            &env,
            &Edit::RemoveHook {
                hook: hook.id.clone(),
            },
        )
        .unwrap();
        apply(&removed);
        assert_eq!(read(&settings(&fx)), ODD, "after add and remove at {event}");
    }
}

#[test]
fn updating_a_hook_changes_only_its_own_entry() {
    let fx = Fixture::new();
    fx.write(&settings(&fx), ODD);
    let env = fx.env();
    let id = hook_id(&env, "keep.sh");
    let preview = plan(
        &fx,
        &env,
        &Edit::UpdateHook {
            hook: id,
            draft: draft(
                "BeforeTool",
                Some("^write"),
                json!({"type": "command", "command": "changed.sh", "timeout": 5000}),
            ),
        },
    )
    .unwrap();
    apply(&preview);
    let now = read(&settings(&fx));
    let old_entry = "{ \"type\": \"command\", \"command\": \"keep.sh\" }";
    let start = ODD.find(old_entry).unwrap();
    let end = start + old_entry.len();
    assert_eq!(&now[..start], &ODD[..start]);
    assert_eq!(&now[now.len() - (ODD.len() - end)..], &ODD[end..]);
    is_valid_gemini_json(&now);
    let hooks = scan(&env).view.hooks;
    assert_eq!(hooks.len(), 1);
    assert_eq!(hooks[0].handler.raw["command"], "changed.sh");
    assert_eq!(hooks[0].handler.raw["timeout"], 5000);

    // The same draft again plans to nothing.
    let id = hooks[0].id.clone();
    let again = plan(
        &fx,
        &env,
        &Edit::UpdateHook {
            hook: id,
            draft: draft(
                "BeforeTool",
                Some("^write"),
                json!({"type": "command", "command": "changed.sh", "timeout": 5000}),
            ),
        },
    )
    .unwrap();
    assert!(again.change_set.ops.is_empty());
}

#[test]
fn updating_the_matcher_or_the_event_moves_only_that_handler() {
    let fx = Fixture::new();
    fx.write(
        &settings(&fx),
        r#"{"hooks":{"BeforeTool":[{"matcher":"a","hooks":[{"type":"command","command":"one.sh"},{"type":"command","command":"two.sh"}]}]}}"#,
    );
    let env = fx.env();
    // The group holds two handlers, so a new matcher moves one of them to a group of its own.
    let preview = plan(
        &fx,
        &env,
        &Edit::UpdateHook {
            hook: hook_id(&env, "two.sh"),
            draft: draft(
                "BeforeTool",
                Some("b"),
                json!({"type": "command", "command": "two.sh"}),
            ),
        },
    )
    .unwrap();
    apply(&preview);
    let hooks = scan(&env).view.hooks;
    let by = |c: &str| hooks.iter().find(|h| h.handler.summary == c).unwrap();
    assert_eq!(by("one.sh").matcher.raw.as_deref(), Some("a"));
    assert_eq!(by("two.sh").matcher.raw.as_deref(), Some("b"));

    // A group with one handler takes a new matcher in place, and a new event moves it.
    let preview = plan(
        &fx,
        &env,
        &Edit::UpdateHook {
            hook: by("two.sh").id.clone(),
            draft: draft(
                "AfterTool",
                None,
                json!({"type": "command", "command": "two.sh"}),
            ),
        },
    )
    .unwrap();
    apply(&preview);
    let hooks = scan(&env).view.hooks;
    let two = hooks
        .iter()
        .find(|h| h.handler.summary == "two.sh")
        .unwrap();
    assert_eq!(two.event, "AfterTool");
    let text = read(&settings(&fx));
    assert!(!text.contains("\"b\""), "the emptied group is gone: {text}");
    is_valid_gemini_json(&text);
}

#[test]
fn removing_the_last_handler_removes_its_group_and_the_event_key() {
    let fx = Fixture::new();
    fx.write(
        &settings(&fx),
        "{\n  \"hooks\": {\n    \"AfterTool\": [\n      { \"hooks\": [ { \"type\": \"command\", \"command\": \"x.sh\" } ] }\n    ],\n    \"BeforeTool\": [\n      { \"hooks\": [ { \"type\": \"command\", \"command\": \"keep.sh\" } ] }\n    ]\n  }\n}\n",
    );
    let env = fx.env();
    let preview = plan(
        &fx,
        &env,
        &Edit::RemoveHook {
            hook: hook_id(&env, "x.sh"),
        },
    )
    .unwrap();
    apply(&preview);
    let text = read(&settings(&fx));
    assert!(!text.contains("AfterTool"), "{text}");
    assert!(text.contains("keep.sh"));
    is_valid_gemini_json(&text);
}

#[test]
fn a_stale_hook_id_is_not_found_and_writes_nothing() {
    let fx = Fixture::new();
    fx.write(&settings(&fx), ODD);
    let env = fx.env();
    let id = hook_id(&env, "keep.sh");
    // The entry changes after the scan that produced the id.
    fx.write(&settings(&fx), &ODD.replace("keep.sh", "other.sh"));
    for edit in [
        Edit::RemoveHook { hook: id.clone() },
        Edit::SetHookEnabled {
            hook: id.clone(),
            enabled: false,
        },
        Edit::UpdateHook {
            hook: id,
            draft: draft(
                "BeforeTool",
                None,
                json!({"type": "command", "command": "z.sh"}),
            ),
        },
    ] {
        assert_eq!(plan(&fx, &env, &edit).unwrap_err(), ApiError::NotFound);
    }
    assert!(read(&settings(&fx)).contains("other.sh"));
}

#[test]
fn a_draft_with_an_unknown_event_or_no_type_is_invalid() {
    let fx = Fixture::new();
    fx.write(&settings(&fx), "{}");
    let env = fx.env();
    let bad = |d: HookDraft| {
        plan(
            &fx,
            &env,
            &Edit::AddHook {
                agent: Gemini.id(),
                file: settings(&fx),
                draft: d,
            },
        )
        .unwrap_err()
    };
    assert!(matches!(
        bad(draft("PreToolUse", None, json!({"type": "command", "command": "x"}))),
        ApiError::InvalidEdit { field, .. } if field == "event"
    ));
    assert!(matches!(
        bad(draft("BeforeTool", None, json!({"command": "x"}))),
        ApiError::InvalidEdit { field, .. } if field == "handler"
    ));
}

#[test]
fn a_hook_may_be_added_to_a_settings_file_that_does_not_exist_yet() {
    let fx = Fixture::new();
    fx.mkdir(&fx.home_path(".gemini"));
    let env = fx.env();
    let preview = add(
        &fx,
        &env,
        draft(
            "SessionStart",
            Some("startup"),
            json!({"type": "command", "command": "hi.sh"}),
        ),
    );
    assert_eq!(preview.change_set.ops[0].expect, Base::Absent);
    apply(&preview);
    is_valid_gemini_json(&read(&settings(&fx)));
    assert_eq!(scan(&env).view.hooks.len(), 1);
}

#[test]
fn a_hook_edit_warns_that_a_running_session_may_overwrite_the_file() {
    let fx = Fixture::new();
    fx.write(&settings(&fx), ODD);
    let env = fx.env();
    let preview = add(
        &fx,
        &env,
        draft(
            "AfterTool",
            None,
            json!({"type": "command", "command": "n.sh"}),
        ),
    );
    assert!(
        preview
            .warnings
            .contains(&Warning::SessionMayOverwrite { agent: Gemini.id() })
    );
    assert!(
        preview
            .warnings
            .contains(&Warning::AgentAlsoWrites { agent: Gemini.id() })
    );
    assert!(
        preview
            .warnings
            .iter()
            .any(|w| matches!(w, Warning::TakesEffect { .. }))
    );
}

#[test]
fn a_trailing_comma_in_the_result_is_refused() {
    // The file already has one, so the scan is fatal and sees no hook file; a settings file
    // whose text would gain one is refused by the same rule.
    let fx = Fixture::new();
    fx.write(&settings(&fx), "{\n  \"a\": 1,\n}\n");
    let env = fx.env();
    assert!(
        plan(
            &fx,
            &env,
            &Edit::AddHook {
                agent: Gemini.id(),
                file: settings(&fx),
                draft: draft(
                    "AfterTool",
                    None,
                    json!({"type": "command", "command": "x"})
                ),
            },
        )
        .is_err()
    );
}

// -------------------------------------------------------------------------------------------
// The switch
// -------------------------------------------------------------------------------------------

fn switch(fx: &Fixture, env: &ScanEnv, command: &str, enabled: bool) -> Preview {
    plan(
        fx,
        env,
        &Edit::SetHookEnabled {
            hook: hook_id(env, command),
            enabled,
        },
    )
    .unwrap()
}

#[test]
fn switching_a_hook_off_and_on_returns_the_file_to_its_bytes() {
    for original in [
        ODD,
        "{\"hooks\":{\"BeforeTool\":[{\"hooks\":[{\"type\":\"command\",\"command\":\"keep.sh\"}]}]}}",
        "{\n    \"hooks\": {\"BeforeTool\": [{\"hooks\": [{\"type\": \"command\", \"command\": \"keep.sh\"}]}]},\n    // note\n    \"hooksConfig\": {\n        \"notifications\": false\n    }\n}\n",
    ] {
        let fx = Fixture::new();
        fx.write(&settings(&fx), original);
        let env = trusted(&fx);
        let off = switch(&fx, &env, "keep.sh", false);
        assert_eq!(off.change_set.ops.len(), 1);
        apply(&off);
        let text = read(&settings(&fx));
        is_valid_gemini_json(&text);
        let hook = &scan(&env).view.hooks[0];
        assert!(
            matches!(
                hook.state,
                HookState::Disabled {
                    by: DisabledBy::Native { .. }
                }
            ),
            "{:?}",
            hook.state
        );
        assert!(text.contains("keep.sh\""), "the key is the command");

        // A second off plans to nothing.
        assert!(
            switch(&fx, &env, "keep.sh", false)
                .change_set
                .ops
                .is_empty()
        );

        let on = switch(&fx, &env, "keep.sh", true);
        apply(&on);
        assert_eq!(read(&settings(&fx)), original);
        assert!(matches!(scan(&env).view.hooks[0].state, HookState::Active));
        assert!(switch(&fx, &env, "keep.sh", true).change_set.ops.is_empty());
    }
}

#[test]
fn switching_an_extension_hook_writes_the_user_settings_and_no_other_file() {
    let fx = Fixture::new();
    let ext = fx.home_path(".gemini/extensions/e");
    fx.write(
        &ext.join("gemini-extension.json"),
        r#"{"name":"e","version":"1"}"#,
    );
    fx.write(
        &ext.join("hooks/hooks.json"),
        r#"{"hooks":{"SessionStart":[{"hooks":[{"type":"command","command":"ext.sh"}]}]}}"#,
    );
    let env = trusted(&fx);
    let preview = switch(&fx, &env, "ext.sh", false);
    let written: Vec<_> = preview.change_set.ops.iter().map(|op| &op.path).collect();
    assert_eq!(written, [&settings(&fx)]);
}

#[test]
fn a_plan_that_writes_two_settings_layers_warns_once_about_each_thing() {
    let fx = Fixture::new();
    fx.write(
        &settings(&fx),
        r#"{"hooks":{"BeforeTool":[{"hooks":[{"type":"command","command":"keep.sh"}]}]},"hooksConfig":{"disabled":["keep.sh"]}}"#,
    );
    fx.write(
        &fx.project_path(".gemini/settings.json"),
        r#"{"hooksConfig":{"disabled":["keep.sh"]}}"#,
    );
    let env = trusted(&fx);
    let preview = switch(&fx, &env, "keep.sh", true);
    assert_eq!(preview.change_set.ops.len(), 2, "both layers are rewritten");
    let overwrite = Warning::SessionMayOverwrite { agent: Gemini.id() };
    assert_eq!(
        preview.warnings.iter().filter(|w| **w == overwrite).count(),
        1,
        "{:?}",
        preview.warnings
    );
}

#[test]
fn the_switch_key_is_the_name_when_there_is_one_and_it_is_added_to_an_existing_list() {
    let fx = Fixture::new();
    fx.write(
        &settings(&fx),
        "{\n  \"hooksConfig\": { \"disabled\": [\"x\"] }, // c\n  \"hooks\": {\"AfterTool\": [{\"hooks\": [{\"type\": \"command\", \"name\": \"guard\", \"command\": \"g.sh\"}]}]}\n}\n",
    );
    let env = trusted(&fx);
    let off = switch(&fx, &env, "g.sh", false);
    apply(&off);
    assert_eq!(
        jsonc::get(&read(&settings(&fx)), "/hooksConfig/disabled").unwrap(),
        Some(json!(["x", "guard"]))
    );
    assert!(read(&settings(&fx)).contains("// c"));
    let on = switch(&fx, &env, "g.sh", true);
    apply(&on);
    assert_eq!(
        jsonc::get(&read(&settings(&fx)), "/hooksConfig/disabled").unwrap(),
        Some(json!(["x"]))
    );
}

#[test]
fn a_shared_switch_key_warns_with_how_many_other_hooks_it_reaches() {
    let fx = Fixture::new();
    fx.write(
        &settings(&fx),
        r#"{"hooks":{"BeforeTool":[{"hooks":[{"type":"command","name":"same","command":"a.sh"}]}],"AfterTool":[{"hooks":[{"type":"command","name":"same","command":"b.sh"}]}]}}"#,
    );
    let env = trusted(&fx);
    let off = switch(&fx, &env, "a.sh", false);
    assert!(off.warnings.contains(&Warning::HookKeyShared { others: 1 }));
    apply(&off);
    // Both hooks went off, as the warning said.
    assert!(
        scan(&env)
            .view
            .hooks
            .iter()
            .all(|h| matches!(h.state, HookState::Disabled { .. }))
    );
}

#[test]
fn a_managed_layer_that_disables_a_hook_cannot_be_overridden() {
    let fx = Fixture::new();
    fx.write(
        &settings(&fx),
        r#"{"hooks":{"BeforeTool":[{"hooks":[{"type":"command","command":"a.sh"}]}]}}"#,
    );
    let managed = fx.root().join("system.json");
    fx.write(&managed, r#"{"hooksConfig":{"disabled":["a.sh"]}}"#);
    let env = fx
        .env()
        .with_var("GEMINI_CLI_SYSTEM_SETTINGS_PATH", managed.as_str());
    assert!(matches!(
        plan(
            &fx,
            &env,
            &Edit::SetHookEnabled {
                hook: hook_id(&env, "a.sh"),
                enabled: true
            }
        )
        .unwrap_err(),
        ApiError::ReadOnly { .. }
    ));
}

// -------------------------------------------------------------------------------------------
// New items
// -------------------------------------------------------------------------------------------

fn new_item(
    fx: &Fixture,
    env: &ScanEnv,
    slot: &str,
    name: Option<&str>,
) -> Result<Preview, ApiError> {
    plan(
        fx,
        env,
        &Edit::NewItem {
            agent: Gemini.id(),
            slot: SlotId(format!("gemini:{slot}")),
            name: name.map(str::to_owned),
        },
    )
}

#[test]
fn a_new_command_skill_and_subagent_start_from_a_valid_skeleton_and_load() {
    let fx = Fixture::new();
    fx.mkdir(&fx.home_path(".gemini"));
    let env = trusted(&fx);
    for (slot, name, path) in [
        ("command:user", "deploy", ".gemini/commands/deploy.toml"),
        ("skill:user", "triage", ".gemini/skills/triage/SKILL.md"),
        ("subagent:user", "helper", ".gemini/agents/helper.md"),
    ] {
        let preview = new_item(&fx, &env, slot, Some(name)).unwrap();
        assert_eq!(preview.change_set.ops.len(), 1, "{slot}");
        assert_eq!(preview.change_set.ops[0].path, fx.home_path(path));
        assert_eq!(preview.change_set.ops[0].expect, Base::Absent);
        apply(&preview);
    }
    let scanned = scan(&env);
    let exts: Vec<_> = scanned
        .view
        .extensions
        .iter()
        .map(|e| (e.ext_kind, e.name.as_str(), e.status.clone()))
        .collect();
    assert_eq!(exts.len(), 3, "{exts:?}");
    assert!(
        exts.iter()
            .all(|(_, _, status)| *status == ExtStatus::Active),
        "{exts:?}"
    );
    assert!(
        !problems(&scanned).iter().any(|p| matches!(
            p,
            Problem::ParseError { .. } | Problem::FrontmatterInvalid { .. }
        )),
        "{:?}",
        problems(&scanned)
    );
}

#[test]
fn a_new_instructions_file_is_empty_and_a_name_that_collides_is_invalid() {
    let fx = Fixture::new();
    fx.mkdir(&fx.home_path(".gemini"));
    fx.write(
        &fx.home_path(".gemini/commands/deploy.toml"),
        "prompt = \"x\"\n",
    );
    let env = trusted(&fx);
    let preview = new_item(&fx, &env, "instructions:project", None).unwrap();
    assert_eq!(
        preview.change_set.ops[0].action,
        Action::Write {
            text: String::new()
        }
    );
    assert_eq!(preview.change_set.ops[0].path, fx.project_path("GEMINI.md"));
    fx.write(&fx.project_path("GEMINI.md"), "x");
    assert!(matches!(
        new_item(&fx, &env, "instructions:project", None).unwrap_err(),
        ApiError::InvalidEdit { .. }
    ));
    assert!(matches!(
        new_item(&fx, &env, "command:user", Some("deploy")).unwrap_err(),
        ApiError::InvalidEdit { field, .. } if field == "name"
    ));
    for bad in [None, Some(""), Some("a/b"), Some("../x"), Some("_x")] {
        assert!(
            matches!(
                new_item(&fx, &env, "command:user", bad).unwrap_err(),
                ApiError::InvalidEdit { .. }
            ),
            "{bad:?}"
        );
    }
}

// -------------------------------------------------------------------------------------------
// Memory
// -------------------------------------------------------------------------------------------

fn add_memory(
    fx: &Fixture,
    env: &ScanEnv,
    title: &str,
    description: &str,
) -> Result<Preview, ApiError> {
    plan(
        fx,
        env,
        &Edit::AddMemory {
            agent: Gemini.id(),
            store: "gemini.private".to_owned(),
            title: title.to_owned(),
            description: description.to_owned(),
            body: "The body.\n".to_owned(),
        },
    )
}

#[test]
fn a_memory_note_comes_with_its_index_line_and_a_scan_shows_it_indexed() {
    let fx = Fixture::new();
    register_slug(&fx, "proj");
    let env = fx.env();
    let dir = fx.home_path(".gemini/tmp/proj/memory");
    fx.write(
        &dir.join("MEMORY.md"),
        "# Index\n- [Old](old.md) \u{2014} older\n",
    );
    fx.write(&dir.join("old.md"), "old");

    let preview = add_memory(&fx, &env, "Build Notes", "how the build runs").unwrap();
    let paths: Vec<_> = preview
        .change_set
        .ops
        .iter()
        .map(|o| o.path.clone())
        .collect();
    assert_eq!(paths, [dir.join("build-notes.md"), dir.join("MEMORY.md")]);
    apply(&preview);
    assert_eq!(
        read(&dir.join("MEMORY.md")),
        "# Index\n- [Old](old.md) \u{2014} older\n- [Build Notes](build-notes.md) \u{2014} how the build runs\n"
    );
    let scanned = scan(&env);
    let entry = scanned.view.memory[0]
        .entries
        .iter()
        .find(|e| e.file == dir.join("build-notes.md"))
        .unwrap();
    assert_eq!(entry.indexed, Some(true));
    assert_eq!(entry.description.as_deref(), Some("how the build runs"));

    // The same title again picks a free name.
    let again = add_memory(&fx, &env, "Build Notes", "").unwrap();
    assert_eq!(again.change_set.ops[0].path, dir.join("build-notes-2.md"));
}

#[test]
fn a_memory_store_is_created_with_its_index_when_the_folder_is_empty() {
    let fx = Fixture::new();
    register_slug(&fx, "proj");
    let env = fx.env();
    let preview = add_memory(&fx, &env, "\u{d55c}글 제목", "").unwrap();
    let dir = fx.home_path(".gemini/tmp/proj/memory");
    assert_eq!(preview.change_set.ops[1].path, dir.join("MEMORY.md"));
    assert_eq!(preview.change_set.ops[1].expect, Base::Absent);
    apply(&preview);
    assert_eq!(scan(&env).view.memory[0].entries.len(), 2);
}

#[test]
fn without_a_slug_the_memory_edit_says_gemini_creates_the_folder_on_first_run() {
    let fx = Fixture::new();
    fx.mkdir(&fx.home_path(".gemini"));
    let err = add_memory(&fx, &fx.env(), "T", "").unwrap_err();
    assert_eq!(
        err,
        ApiError::InvalidEdit {
            field: "store".into(),
            why: RuleRef::new("gemini.memory.no_slug")
        }
    );
    assert!(!fx.home_path(".gemini/projects.json").as_path().exists());
}

#[test]
fn trashing_a_note_removes_its_index_line_in_the_same_plan() {
    let fx = Fixture::new();
    register_slug(&fx, "proj");
    let dir = fx.home_path(".gemini/tmp/proj/memory");
    fx.write(
        &dir.join("MEMORY.md"),
        "- [A](a.md) \u{2014} one\n- [B](b.md) \u{2014} two\n",
    );
    fx.write(&dir.join("a.md"), "a");
    fx.write(&dir.join("b.md"), "b");
    let env = fx.env();
    let preview = plan(
        &fx,
        &env,
        &Edit::Trash {
            path: dir.join("a.md"),
            base: Revision::of(b"a"),
        },
    )
    .unwrap();
    assert_eq!(preview.change_set.ops.len(), 2);
    assert_eq!(preview.change_set.ops[0].action, Action::Trash);
    apply(&preview);
    assert_eq!(read(&dir.join("MEMORY.md")), "- [B](b.md) \u{2014} two\n");

    // The index itself and an unrelated file still go through the generic trash.
    let generic = plan(
        &fx,
        &env,
        &Edit::Trash {
            path: dir.join("MEMORY.md"),
            base: Revision::of(read(&dir.join("MEMORY.md")).as_bytes()),
        },
    )
    .unwrap();
    assert_eq!(generic.change_set.ops.len(), 1);
}

fn register_slug(fx: &Fixture, slug: &str) {
    fx.write(
        &fx.home_path(".gemini/projects.json"),
        &format!("{{\"projects\":{{\"{}\":\"{slug}\"}}}}", fx.project()),
    );
}

// -------------------------------------------------------------------------------------------
// Warnings on a plain save
// -------------------------------------------------------------------------------------------

fn save(fx: &Fixture, env: &ScanEnv, path: &AbsPath, text: &str) -> Preview {
    let base = match std::fs::read(path.as_path()) {
        Ok(bytes) => Base::Rev {
            rev: Revision::of(&bytes),
        },
        Err(_) => Base::Absent,
    };
    plan(
        fx,
        env,
        &Edit::Save {
            path: path.clone(),
            base,
            text: text.to_owned(),
        },
    )
    .unwrap()
}

#[test]
fn saving_a_settings_file_warns_of_an_overwrite_by_a_running_session() {
    let fx = Fixture::new();
    fx.write(&settings(&fx), "{}");
    let preview = save(&fx, &fx.env(), &settings(&fx), "{\"a\": 1}");
    assert!(
        preview
            .warnings
            .contains(&Warning::SessionMayOverwrite { agent: Gemini.id() })
    );
}

#[test]
fn saving_a_project_subagent_warns_that_it_must_be_acknowledged_again() {
    let fx = Fixture::new();
    let agent = fx.project_path(".gemini/agents/a.md");
    fx.write(&agent, "---\nname: a\ndescription: d\n---\nbody");
    let env = trusted(&fx);
    let preview = save(
        &fx,
        &env,
        &agent,
        "---\nname: a\ndescription: d\n---\nnew body",
    );
    assert!(preview.warnings.contains(&Warning::AgentAckReset));
    // A user subagent is not acknowledged, so no warning.
    let user = fx.home_path(".gemini/agents/u.md");
    fx.write(&user, "---\nname: u\ndescription: d\n---\nbody");
    let preview = save(&fx, &env, &user, "---\nname: u\ndescription: d\n---\nnew");
    assert!(!preview.warnings.contains(&Warning::AgentAckReset));
}

#[test]
fn saving_a_context_file_warns_only_for_a_newly_added_import_token() {
    let fx = Fixture::new();
    let file = fx.project_path("GEMINI.md");
    fx.write(&file, "see @./old.md and `@code` here\n");
    let env = trusted(&fx);
    let preview = save(
        &fx,
        &env,
        &file,
        "see @./old.md and `@code` here\nping @someone about a@b.md and `@tick` and ~/x @~/y @1.md\n",
    );
    let tokens: Vec<_> = preview
        .warnings
        .iter()
        .filter_map(|w| match w {
            Warning::NewImportToken { raw } => Some(raw.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(tokens, ["@someone"]);
    let none = save(&fx, &env, &file, "see @./old.md and `@code` here\nplain\n");
    assert!(
        !none
            .warnings
            .iter()
            .any(|w| matches!(w, Warning::NewImportToken { .. }))
    );
}

// -------------------------------------------------------------------------------------------
// Round trips on files that are formatted in unusual ways
// -------------------------------------------------------------------------------------------

const ONE_HOOK: &str = r#"{"type":"command","command":"keep.sh"}"#;

fn odd_files() -> Vec<(&'static str, String)> {
    vec![
        (
            "compact",
            format!(
                r#"{{"ui":{{"theme":"dark"}},"hooks":{{"BeforeTool":[{{"hooks":[{ONE_HOOK}]}}]}}}}"#
            ),
        ),
        (
            "two spaces with comments",
            format!(
                "// top\n{{\n  // theme\n  \"ui\": {{ \"theme\": \"dark\" }},\n  \"hooks\": {{\n    \"BeforeTool\": [\n      // the hook\n      {{ \"hooks\": [ {ONE_HOOK} ] }}\n    ]\n  }} /* end */\n}}\n"
            ),
        ),
        (
            "crlf",
            format!(
                "{{\r\n  \"ui\": {{\"theme\": \"dark\"}},\r\n  \"hooks\": {{\r\n    \"BeforeTool\": [\r\n      {{\"hooks\": [{ONE_HOOK}]}}\r\n    ]\r\n  }}\r\n}}\r\n"
            ),
        ),
        (
            "an existing disabled list",
            format!(
                "{{\n  \"hooksConfig\": {{ \"disabled\": [\"x\", \"y\"] }},\n  \"hooks\": {{\"BeforeTool\": [{{\"hooks\": [{ONE_HOOK}]}}]}}\n}}\n"
            ),
        ),
        (
            "four spaces and a hooks key last",
            format!(
                "{{\n    \"a\": [\n        1,\n        2\n    ],\n    \"hooks\": {{\n        \"BeforeTool\": [\n            {{\n                \"hooks\": [\n                    {ONE_HOOK}\n                ]\n            }}\n        ]\n    }}\n}}"
            ),
        ),
    ]
}

#[test]
fn every_unusual_layout_survives_a_hook_add_and_remove_byte_for_byte() {
    for (label, original) in odd_files() {
        for (event, matcher) in [("AfterTool", None), ("BeforeTool", Some("^x"))] {
            let fx = Fixture::new();
            fx.write(&settings(&fx), &original);
            let env = trusted(&fx);
            apply(&add(
                &fx,
                &env,
                draft(
                    event,
                    matcher,
                    json!({"type": "command", "command": "new.sh"}),
                ),
            ));
            let added = read(&settings(&fx));
            is_valid_gemini_json(&added);
            assert!(added.contains("keep.sh"), "{label}");
            let id = hook_id(&env, "new.sh");
            apply(&plan(&fx, &env, &Edit::RemoveHook { hook: id }).unwrap());
            assert_eq!(read(&settings(&fx)), original, "{label}, {event}");
        }
    }
}

#[test]
fn every_unusual_layout_survives_a_switch_off_and_on_byte_for_byte() {
    for (label, original) in odd_files() {
        let fx = Fixture::new();
        fx.write(&settings(&fx), &original);
        let env = trusted(&fx);
        apply(&switch(&fx, &env, "keep.sh", false));
        is_valid_gemini_json(&read(&settings(&fx)));
        apply(&switch(&fx, &env, "keep.sh", true));
        assert_eq!(read(&settings(&fx)), original, "{label}");
    }
}

#[test]
fn updating_a_hook_in_every_unusual_layout_changes_only_its_entry() {
    for (label, original) in odd_files() {
        let fx = Fixture::new();
        fx.write(&settings(&fx), &original);
        let env = trusted(&fx);
        let id = hook_id(&env, "keep.sh");
        apply(
            &plan(
                &fx,
                &env,
                &Edit::UpdateHook {
                    hook: id,
                    draft: draft(
                        "BeforeTool",
                        None,
                        json!({"type": "command", "command": "k2.sh"}),
                    ),
                },
            )
            .unwrap(),
        );
        let now = read(&settings(&fx));
        // The new entry is laid out by the syntax tree, so compare what the file holds, and
        // that the comments around it are still there.
        assert_eq!(
            jsonc::get(&now, "").unwrap(),
            jsonc::get(&original.replace("keep.sh", "k2.sh"), "").unwrap(),
            "{label}"
        );
        for comment in ["// top", "// theme", "// the hook", "/* end */"] {
            assert_eq!(now.contains(comment), original.contains(comment), "{label}");
        }
    }
}
