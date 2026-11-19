//! What the Codex planners return for fixture homes: the bytes a plan would write, derived from
//! the file formats and the spec, not from the planner. A test applies a plan by writing its
//! operations to the temp home, then scans again.

use super::*;
use crate::agents::Agent;
use crate::agents::codex::Codex;
use crate::edit::{ChangeSource, Planner};
use crate::scan::scan_with;
use crate::testing::Fixture;

struct NoHistory;
impl ChangeSource for NoHistory {
    fn detail(&self, _: &ChangeId) -> Result<ChangeDetail, ApiError> {
        Err(ApiError::NotFound)
    }
}

struct Home {
    fx: Fixture,
}

impl Home {
    fn new() -> Home {
        let fx = Fixture::new();
        fx.write(&fx.project_path(".git/HEAD"), "ref: refs/heads/main\n");
        Home { fx }
    }

    fn codex(&self, rel: &str) -> AbsPath {
        self.fx.home_path(".codex").join(rel)
    }

    fn write(&self, rel: &str, text: &str) {
        self.fx.write(&self.codex(rel), text);
    }

    fn read(&self, rel: &str) -> String {
        std::fs::read_to_string(self.codex(rel).as_path()).unwrap()
    }

    fn env(&self) -> crate::env::ScanEnv {
        self.fx
            .env()
            .with_var("CODEX_HOME", self.fx.home_path(".codex").as_str())
    }

    fn plan(&self, edit: &Edit) -> Result<Preview, ApiError> {
        let env = self.env();
        let scanned = scan_with(&[&Codex as &dyn Agent], &env);
        let parked = self.fx.root().join("data/parked");
        Planner {
            agents: &[&Codex as &dyn Agent],
            scanned: &scanned,
            env: &env,
            parked_dir: &parked,
            changes: &NoHistory,
        }
        .plan(edit)
    }

    /// Executes a plan's operations, as the host would.
    fn apply(&self, preview: &Preview) {
        for op in &preview.change_set.ops {
            match &op.action {
                Action::Write { text } => self.fx.write(&op.path, text),
                Action::Trash => std::fs::remove_file(op.path.as_path()).unwrap(),
                Action::TrashDir => std::fs::remove_dir_all(op.path.as_path()).unwrap(),
            }
        }
    }

    fn run(&self, edit: &Edit) -> Preview {
        let preview = self.plan(edit).unwrap_or_else(|e| panic!("{e}"));
        self.apply(&preview);
        preview
    }

    fn view(&self) -> AgentView {
        let env = self.env();
        scan_with(&[&Codex as &dyn Agent], &env)
            .snapshot
            .agents
            .remove(0)
    }

    fn hook(&self, summary: &str) -> Hook {
        self.view()
            .hooks
            .into_iter()
            .find(|h| h.handler.summary == summary)
            .unwrap_or_else(|| panic!("no hook {summary}"))
    }

    fn state(&self) -> String {
        std::fs::read_to_string(self.codex("config.toml").as_path()).unwrap_or_default()
    }
}

fn draft(event: &str, matcher: Option<&str>, command: &str) -> HookDraft {
    HookDraft {
        event: event.to_owned(),
        matcher: matcher.map(str::to_owned),
        handler: json!({"type": "command", "command": command, "x-unknown": [1, 2]}),
    }
}

/// The text between the longest common prefix and suffix: what a change touched.
fn changed(before: &str, after: &str) -> (String, String) {
    let prefix = before
        .bytes()
        .zip(after.bytes())
        .take_while(|(a, b)| a == b)
        .count();
    let max_suffix = before.len().min(after.len()) - prefix;
    let suffix = before
        .bytes()
        .rev()
        .zip(after.bytes().rev())
        .take(max_suffix)
        .take_while(|(a, b)| a == b)
        .count();
    (
        before[prefix..before.len() - suffix].to_owned(),
        after[prefix..after.len() - suffix].to_owned(),
    )
}

const JSON_FILE: &str = "{\n\t\"description\":   \"mine\",\n\t\"hooks\" : {\n\t\t\"Stop\": [ {\"hooks\": [{\"type\":\"command\",\"command\":\"echo stop\"}]} ],\n\t\t\"PreToolUse\": [\n\t\t\t{\n\t\t\t\t\"matcher\": \"Bash\",\n\t\t\t\t\"hooks\": [\n\t\t\t\t\t{\"type\": \"command\", \"command\": \"echo one\", \"timeout\": 5},\n\t\t\t\t\t{\"type\": \"command\", \"command\": \"echo two\"}\n\t\t\t\t]\n\t\t\t},\n\t\t\t{\"matcher\": \"Edit\", \"hooks\": [{\"type\": \"command\", \"command\": \"echo three\"}]}\n\t\t]\n\t}\n}\n";

const TOML_FILE: &str = "# my codex config\nmodel = \"gpt-5\"   # keep\n\n[features]\nhooks = true\n\n[[hooks.PreToolUse]]   # first group\nmatcher = \"Bash\"\n\n[[hooks.PreToolUse.hooks]]\ntype = \"command\"\ncommand = \"echo one\"\ntimeout = 5   # five\n\n[[hooks.PreToolUse.hooks]]\ntype = \"command\"\ncommand = \"echo two\"\n\n[[hooks.Stop]]\n[[hooks.Stop.hooks]]\ntype=\"command\"\ncommand=\"echo stop\"\n\n[hooks.state.\"some:other:key\"]\ntrusted_hash = \"sha256:abc\"   # approved\n\n[tail]\nx = 1\n";

fn home_with(file: &str, text: &str) -> Home {
    let home = Home::new();
    home.write(file, text);
    home
}

fn parsed_hooks(home: &Home) -> Vec<String> {
    home.view()
        .hooks
        .iter()
        .map(|h| h.handler.summary.clone())
        .collect()
}

// --- hooks.json ----------------------------------------------------------------------------

#[test]
fn json_add_appends_a_group_and_removing_it_restores_every_byte() {
    let home = home_with("hooks.json", JSON_FILE);
    let file = home.codex("hooks.json");
    let preview = home.run(&Edit::AddHook {
        agent: me(),
        file: file.clone(),
        draft: draft("PreToolUse", Some("Write"), "echo new"),
    });
    assert!(preview.warnings.contains(&Warning::HookTrustReset));
    let after = home.read("hooks.json");
    let (gone, added) = changed(JSON_FILE, &after);
    assert!(gone.is_empty(), "an add removes no byte: {gone:?}");
    assert!(added.contains("echo new") && added.contains("x-unknown"));
    assert!(
        parsed_hooks(&home).contains(&"echo new".to_owned()),
        "{after}"
    );
    let added = home.hook("echo new");
    assert_eq!(added.handler.raw["x-unknown"], json!([1, 2]));
    assert_eq!(added.matcher.raw.as_deref(), Some("Write"));

    home.run(&Edit::RemoveHook { hook: added.id });
    assert_eq!(home.read("hooks.json"), JSON_FILE);
}

#[test]
fn json_update_changes_only_the_entry() {
    let home = home_with("hooks.json", JSON_FILE);
    let before = home.hook("echo two");
    home.run(&Edit::UpdateHook {
        hook: before.id,
        draft: draft("PreToolUse", Some("Bash"), "echo changed"),
    });
    let after = home.read("hooks.json");
    let (was, now) = changed(JSON_FILE, &after);
    assert!(was.contains("echo two") && !was.contains("echo one") && !was.contains("echo three"));
    assert!(now.contains("echo changed"));
    let mut hooks = parsed_hooks(&home);
    hooks.sort();
    assert_eq!(hooks, ["echo changed", "echo one", "echo stop", "echo three"]);
    // An update to the same entry plans to nothing.
    let same = home.hook("echo changed");
    let again = home
        .plan(&Edit::UpdateHook {
            hook: same.id,
            draft: HookDraft {
                event: "PreToolUse".into(),
                matcher: Some("Bash".into()),
                handler: same.handler.raw.clone(),
            },
        })
        .unwrap();
    assert!(again.change_set.ops.is_empty());
}

#[test]
fn json_removing_the_last_handler_removes_the_group_and_then_the_event() {
    let home = home_with("hooks.json", JSON_FILE);
    home.run(&Edit::RemoveHook {
        hook: home.hook("echo three").id,
    });
    let after = home.read("hooks.json");
    assert!(!after.contains("Edit"), "{after}");
    assert!(after.contains("echo one"));
    let stop = home.hook("echo stop");
    home.run(&Edit::RemoveHook { hook: stop.id });
    let after = home.read("hooks.json");
    assert!(!after.contains("\"Stop\""), "no empty event: {after}");
    assert!(serde_json::from_str::<Json>(&after).is_ok(), "{after}");
    assert!(after.contains("\"description\""));
}

#[test]
fn json_update_of_the_matcher_of_a_shared_group_moves_the_entry_to_a_new_group() {
    let home = home_with("hooks.json", JSON_FILE);
    let two = home.hook("echo two");
    home.run(&Edit::UpdateHook {
        hook: two.id,
        draft: draft("PreToolUse", Some("Other"), "echo two"),
    });
    let moved = home.hook("echo two");
    assert_eq!(moved.matcher.raw.as_deref(), Some("Other"));
    assert!(home.hook("echo one").matcher.raw.as_deref() == Some("Bash"));
}

#[test]
fn json_update_of_the_matcher_of_a_single_handler_group_keeps_its_place() {
    let home = home_with("hooks.json", JSON_FILE);
    let three = home.hook("echo three");
    home.run(&Edit::UpdateHook {
        hook: three.id,
        draft: draft("PreToolUse", None, "echo three"),
    });
    let after = home.read("hooks.json");
    assert!(!after.contains("\"Edit\""), "{after}");
    assert_eq!(home.hook("echo three").matcher.raw, None);
}

#[test]
fn a_new_hook_file_is_plain_pretty_json() {
    let home = Home::new();
    let file = home.codex("hooks.json");
    home.run(&Edit::AddHook {
        agent: me(),
        file: file.clone(),
        draft: draft("Stop", None, "echo first"),
    });
    assert_eq!(
        home.read("hooks.json"),
        "{\n  \"hooks\": {\n    \"Stop\": [\n      {\n        \"hooks\": [\n          {\n            \"type\": \"command\",\n            \"command\": \"echo first\",\n            \"x-unknown\": [\n              1,\n              2\n            ]\n          }\n        ]\n      }\n    ]\n  }\n}\n"
    );
}

// --- config.toml [hooks] -------------------------------------------------------------------

#[test]
fn toml_add_and_remove_round_trip_and_keep_comments_and_state() {
    let home = home_with("config.toml", TOML_FILE);
    let file = home.codex("config.toml");
    let preview = home.run(&Edit::AddHook {
        agent: me(),
        file,
        draft: draft("PreToolUse", Some("Write"), "echo new"),
    });
    assert!(preview.warnings.contains(&Warning::HookTrustReset));
    assert!(preview.warnings.contains(&also_writes()));
    let after = home.state();
    let (gone, added) = changed(TOML_FILE, &after);
    assert!(gone.is_empty(), "an add removes no byte: {gone:?}");
    assert!(added.contains("echo new"));
    assert!(after.contains("# first group") && after.contains("# five") && after.contains("# approved"));
    let new = home.hook("echo new");
    assert_eq!(new.handler.raw["x-unknown"], json!([1, 2]));
    assert_eq!(new.matcher.raw.as_deref(), Some("Write"));
    home.run(&Edit::RemoveHook { hook: new.id });
    assert_eq!(home.state(), TOML_FILE);
}

#[test]
fn toml_update_changes_only_the_entry_and_keeps_the_neighbours_comments() {
    let home = home_with("config.toml", TOML_FILE);
    let one = home.hook("echo one");
    home.run(&Edit::UpdateHook {
        hook: one.id,
        draft: HookDraft {
            event: "PreToolUse".into(),
            matcher: Some("Bash".into()),
            handler: json!({"type": "command", "command": "echo uno", "timeout": 5}),
        },
    });
    let after = home.state();
    let (was, now) = changed(TOML_FILE, &after);
    assert_eq!((was.as_str(), now.as_str()), ("one", "uno"), "only that word changed");
    assert!(after.contains("# five"), "an unchanged field keeps its comment");
    assert!(parsed_hooks(&home).contains(&"echo uno".to_owned()));
    assert!(!parsed_hooks(&home).contains(&"echo one".to_owned()));
}

#[test]
fn toml_removing_the_last_handler_of_a_group_and_event_leaves_no_shell() {
    let home = home_with("config.toml", TOML_FILE);
    home.run(&Edit::RemoveHook {
        hook: home.hook("echo stop").id,
    });
    let after = home.state();
    assert!(!after.contains("Stop"), "{after}");
    assert!(after.contains("[hooks.state.\"some:other:key\"]"));
    assert!(toml::get(&after, &["hooks", "PreToolUse"]).unwrap().is_some());
    home.run(&Edit::RemoveHook {
        hook: home.hook("echo one").id,
    });
    home.run(&Edit::RemoveHook {
        hook: home.hook("echo two").id,
    });
    let after = home.state();
    assert!(!after.contains("PreToolUse"), "{after}");
    assert!(after.contains("# approved") && after.contains("[tail]"));
}

#[test]
fn toml_hook_with_a_nested_input_table_reads_back_as_written() {
    let home = Home::new();
    let handler = json!({"type": "mcp_tool", "server": "sec", "tool": "scan", "input": {"path": "x", "n": 3}});
    home.run(&Edit::AddHook {
        agent: me(),
        file: home.codex("config.toml"),
        draft: HookDraft {
            event: "PreToolUse".into(),
            matcher: None,
            handler: handler.clone(),
        },
    });
    let hooks = home.view().hooks;
    assert_eq!(hooks.len(), 1, "{}", home.state());
    assert_eq!(hooks[0].handler.raw, handler);
}

// --- the switch ---------------------------------------------------------------------------

#[test]
fn switching_off_and_on_restores_the_file_and_a_second_switch_plans_nothing() {
    for (start, name) in [(TOML_FILE, "with state"), ("", "empty"), ("model = \"x\"\n", "short")] {
        let home = Home::new();
        home.write("hooks.json", JSON_FILE);
        if !start.is_empty() {
            home.write("config.toml", start);
        }
        let hook = home.hook("echo two");
        let off = home.run(&Edit::SetHookEnabled {
            hook: hook.id.clone(),
            enabled: false,
        });
        assert!(!off.warnings.contains(&Warning::HookTrustReset));
        let state = home.state();
        assert!(state.contains("enabled = false"), "{name}: {state}");
        assert!(state.contains("hooks.json:pre_tool_use:0:1"), "{name}: {state}");
        assert!(
            !state.contains("trusted_hash") || state.contains("sha256:abc"),
            "no hash is written"
        );
        assert!(
            home.view()
                .hooks
                .iter()
                .any(|h| h.id == hook.id && matches!(h.state, HookState::Disabled { .. }))
        );

        let again = home
            .plan(&Edit::SetHookEnabled {
                hook: hook.id.clone(),
                enabled: false,
            })
            .unwrap();
        assert!(again.change_set.ops.is_empty(), "{name}: a second off is a no-op");

        home.run(&Edit::SetHookEnabled {
            hook: hook.id.clone(),
            enabled: true,
        });
        let back = std::fs::read_to_string(home.codex("config.toml").as_path()).ok();
        let expected = (!start.is_empty()).then(|| start.to_owned());
        // A file this plan created is left empty, not deleted. Compare by content.
        assert_eq!(back.unwrap_or_default(), expected.unwrap_or_default(), "{name}");
        let on_again = home
            .plan(&Edit::SetHookEnabled {
                hook: hook.id,
                enabled: true,
            })
            .unwrap();
        assert!(on_again.change_set.ops.is_empty(), "{name}: a second on is a no-op");
    }
}

#[test]
fn a_switch_never_touches_an_existing_trusted_hash() {
    let home = Home::new();
    home.write("hooks.json", JSON_FILE);
    let hook = home.hook("echo one");
    let env = home.env();
    let fs = crate::probe_fs::ProbeFs::new();
    let key = crate::agents::codex::inspect_hooks(&ScanCx { env: &env, fs: &fs })
        .into_iter()
        .find(|i| i.key.ends_with("pre_tool_use:0:0"))
        .unwrap()
        .key;
    let start = format!("[hooks.state.\"{key}\"]\ntrusted_hash = \"sha256:abc\"\n");
    home.write("config.toml", &start);
    home.run(&Edit::SetHookEnabled {
        hook: hook.id.clone(),
        enabled: false,
    });
    let off = home.state();
    assert_eq!(off, format!("{start}enabled = false\n"));
    home.run(&Edit::SetHookEnabled {
        hook: hook.id,
        enabled: true,
    });
    assert_eq!(home.state(), start);
}

#[test]
fn a_managed_or_unsupported_hook_cannot_be_switched() {
    let home = home_with(
        "hooks.json",
        r#"{"hooks":{"Stop":[{"hooks":[{"type":"prompt"}]}]}}"#,
    );
    let hook = home.view().hooks.remove(0);
    let err = home
        .plan(&Edit::SetHookEnabled {
            hook: hook.id,
            enabled: false,
        })
        .unwrap_err();
    assert!(matches!(err, ApiError::InvalidEdit { .. }), "{err:?}");
}

// --- stale ids and bad drafts -------------------------------------------------------------

#[test]
fn a_stale_hook_id_is_not_found_and_plans_no_write() {
    let home = home_with("hooks.json", JSON_FILE);
    let hook = home.hook("echo two");
    // The entry changes after the scan that produced the id.
    home.write("hooks.json", &JSON_FILE.replace("echo two", "echo swapped"));
    let swapped = home.read("hooks.json");
    for edit in [
        Edit::RemoveHook {
            hook: hook.id.clone(),
        },
        Edit::UpdateHook {
            hook: hook.id.clone(),
            draft: draft("Stop", None, "x"),
        },
        Edit::SetHookEnabled {
            hook: hook.id.clone(),
            enabled: false,
        },
    ] {
        assert_eq!(home.plan(&edit).unwrap_err(), ApiError::NotFound);
    }
    assert_eq!(home.read("hooks.json"), swapped);
    assert!(!home.codex("config.toml").as_path().exists());
}

#[test]
fn an_unknown_event_or_a_broken_handler_is_invalid() {
    let home = home_with("hooks.json", JSON_FILE);
    let file = home.codex("hooks.json");
    let bad_event = home.plan(&Edit::AddHook {
        agent: me(),
        file: file.clone(),
        draft: draft("BeforeTool", None, "x"),
    });
    assert!(matches!(bad_event, Err(ApiError::InvalidEdit { field, .. }) if field == "draft.event"));
    let bad_handler = home.plan(&Edit::AddHook {
        agent: me(),
        file,
        draft: HookDraft {
            event: "Stop".into(),
            matcher: None,
            handler: json!({"type": "command"}),
        },
    });
    assert!(matches!(bad_handler, Err(ApiError::InvalidEdit { field, .. }) if field == "draft.handler"));
    let outside = home.plan(&Edit::AddHook {
        agent: me(),
        file: home.fx.project_path("elsewhere.json"),
        draft: draft("Stop", None, "x"),
    });
    assert!(matches!(outside, Err(ApiError::InvalidEdit { field, .. }) if field == "file"));
}

// --- new items and memory notes -----------------------------------------------------------

fn slot_id(home: &Home, id: &str) -> SlotId {
    home.view()
        .slots
        .into_iter()
        .find(|s| s.id.0 == id)
        .unwrap_or_else(|| panic!("no slot {id}"))
        .id
}

#[test]
fn new_items_start_from_a_valid_skeleton_in_the_slot() {
    let home = Home::new();
    let new = |slot: &str, name: Option<&str>| Edit::NewItem {
        agent: me(),
        slot: slot_id(&home, slot),
        name: name.map(str::to_owned),
    };
    let skill = home.plan(&new("codex.user.skill", Some("deploy"))).unwrap();
    assert_eq!(skill.change_set.ops.len(), 1);
    assert_eq!(skill.change_set.ops[0].path, home.codex("skills/deploy/SKILL.md"));
    assert_eq!(skill.change_set.ops[0].expect, Base::Absent);
    let Action::Write { text } = &skill.change_set.ops[0].action else {
        panic!("write")
    };
    assert!(text.starts_with("---\nname: deploy\ndescription: "), "{text}");
    home.apply(&skill);
    assert_eq!(
        home.view()
            .extensions
            .iter()
            .filter(|e| e.name == "deploy")
            .count(),
        1,
        "the new skill is listed"
    );

    let agent = home.plan(&new("codex.user.subagent", Some("reviewer"))).unwrap();
    let Action::Write { text } = &agent.change_set.ops[0].action else {
        panic!("write")
    };
    let table: ::toml::Table = ::toml::from_str(text).unwrap();
    assert_eq!(table["name"].as_str(), Some("reviewer"));
    assert!(table.contains_key("description"));
    assert_eq!(agent.change_set.ops[0].path, home.codex("agents/reviewer.toml"));
    home.apply(&agent);
    assert!(home.view().extensions.iter().any(|e| e.name == "reviewer"));

    let rule = home.plan(&new("codex.user.rule", Some("policy"))).unwrap();
    assert_eq!(rule.change_set.ops[0].path, home.codex("rules/policy.rules"));

    let instructions = home.plan(&new("codex.user.instructions", None)).unwrap();
    assert_eq!(instructions.change_set.ops[0].path, home.codex("AGENTS.md"));
    assert_eq!(
        instructions.change_set.ops[0].action,
        Action::Write {
            text: String::new()
        }
    );
    assert!(skill.warnings.iter().any(|w| matches!(w, Warning::TakesEffect { .. })));
}

#[test]
fn a_colliding_or_malformed_name_is_invalid() {
    let home = Home::new();
    home.write("agents/reviewer.toml", "name = \"reviewer\"\n");
    home.write("skills/deploy/SKILL.md", "---\ndescription: d\n---\n");
    home.write("AGENTS.md", "existing\n");
    let new = |slot: &str, name: Option<&str>| Edit::NewItem {
        agent: me(),
        slot: slot_id(&home, slot),
        name: name.map(str::to_owned),
    };
    for edit in [
        new("codex.user.subagent", Some("reviewer")),
        new("codex.user.skill", Some("deploy")),
        new("codex.user.instructions", None),
    ] {
        let err = home.plan(&edit).unwrap_err();
        assert!(
            matches!(&err, ApiError::InvalidEdit { why, .. } if why.0 == RULE_COLLISION),
            "{err:?}"
        );
    }
    for name in [None, Some(""), Some("../x"), Some(".hidden"), Some("a/b"), Some(&"n".repeat(65)[..])] {
        let err = home.plan(&new("codex.user.skill", name)).unwrap_err();
        assert!(
            matches!(&err, ApiError::InvalidEdit { field, .. } if field == "name"),
            "{name:?}: {err:?}"
        );
    }
}

#[test]
fn a_memory_note_is_one_file_in_the_notes_folder_and_a_scan_lists_it() {
    let home = Home::new();
    home.write("config.toml", "[features]\nmemories = true\n");
    home.write("memories/memory_summary.md", "s\n");
    let add = |title: &str| Edit::AddMemory {
        agent: me(),
        store: "codex.memories".into(),
        title: title.into(),
        description: "Prefers tabs".into(),
        body: "Use tabs in Go files.".into(),
    };
    let preview = home.plan(&add("Tabs vs spaces")).unwrap();
    let notes = home.codex("memories/extensions/ad_hoc/notes");
    assert_eq!(
        preview
            .change_set
            .ops
            .iter()
            .map(|o| o.path.clone())
            .collect::<Vec<_>>(),
        vec![notes.join("tabs-vs-spaces.md")]
    );
    let Action::Write { text } = &preview.change_set.ops[0].action else {
        panic!("write")
    };
    assert_eq!(text, "# Tabs vs spaces\n\nPrefers tabs\n\nUse tabs in Go files.\n");
    assert!(preview.warnings.iter().any(
        |w| matches!(w, Warning::TakesEffect { when, .. } if when.0 == RULE_NOTE)
    ));
    home.apply(&preview);
    let store = &home.view().memory[0];
    let entry = store
        .entries
        .iter()
        .find(|e| e.file == notes.join("tabs-vs-spaces.md"))
        .expect("the scan lists the note");
    assert_eq!(entry.role, MemoryRole::Note);
    // The same title again collides, and the generated files are untouched.
    assert!(matches!(
        home.plan(&add("Tabs vs spaces")),
        Err(ApiError::InvalidEdit { .. })
    ));
    assert_eq!(home.read("memories/memory_summary.md"), "s\n");
}

#[test]
fn a_note_with_the_memory_feature_off_is_planned_with_a_warning() {
    let home = Home::new();
    home.write("memories/MEMORY.md", "# generated\n");
    let preview = home
        .plan(&Edit::AddMemory {
            agent: me(),
            store: "codex.memories".into(),
            title: "한국어 제목".into(),
            description: String::new(),
            body: "body".into(),
        })
        .unwrap();
    assert_eq!(preview.change_set.ops.len(), 1);
    assert!(preview.change_set.ops[0].path.as_str().ends_with("/한국어-제목.md"));
    assert!(preview.warnings.iter().any(
        |w| matches!(w, Warning::TakesEffect { when, .. } if when.0 == RULE_MEMORY_OFF)
    ));
    let missing = home.plan(&Edit::AddMemory {
        agent: me(),
        store: "nope".into(),
        title: "t".into(),
        description: String::new(),
        body: String::new(),
    });
    assert!(matches!(missing, Err(ApiError::InvalidEdit { field, .. }) if field == "store"));
}

#[test]
fn every_rule_the_planners_cite_is_in_rules_toml() {
    let known: Vec<_> = Codex.rules().iter().map(|r| r.id.0.as_str()).collect();
    for rule in [
        RULE_NAME,
        RULE_COLLISION,
        RULE_SLOT,
        RULE_STORE,
        RULE_EVENT,
        RULE_HANDLER,
        RULE_FILE,
        RULE_UNREADABLE,
        RULE_NEXT_SESSION,
        RULE_HOOKS_REREAD,
        RULE_NOTE,
        RULE_MEMORY_OFF,
    ] {
        assert!(known.contains(&rule), "{rule} is not in rules.toml");
    }
}
