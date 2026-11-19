//! The Claude planners, through the real `Planner`: the plan is checked against the boundary,
//! the bases and the guard as it would be in the host. `apply` here writes the change set to the
//! fixture the way the host does, without a history.

use serde_json::{Value, json};

use super::splice::Seg;
use crate::agents::claude::Claude;
use crate::agents::claude::setup::TEST_MANAGED_DIR_VAR;
use crate::agents::{Agent, registry};
use crate::edit::{ChangeSource, Planner};
use crate::env::ScanEnv;
use crate::model::*;
use crate::scan::{Scanned, scan_with_parked};
use crate::testing::Fixture;

struct NoHistory;
impl ChangeSource for NoHistory {
    fn detail(&self, _: &ChangeId) -> Result<ChangeDetail, ApiError> {
        Err(ApiError::NotFound)
    }
}

struct Rig {
    fx: Fixture,
    env: ScanEnv,
    parked: AbsPath,
}

impl Rig {
    fn new() -> Rig {
        let fx = Fixture::new();
        fx.mkdir(&fx.project_path(".git"));
        let managed = fx.root().join("managed");
        let env = fx.env().with_var(TEST_MANAGED_DIR_VAR, managed.as_str());
        let parked = fx.root().join("data/parked");
        Rig { fx, env, parked }
    }

    fn user_settings(&self) -> AbsPath {
        self.fx.home_path(".claude/settings.json")
    }

    fn scan(&self) -> Scanned {
        scan_with_parked(registry(), &self.env, Some(&self.parked))
    }

    fn view(&self) -> AgentView {
        self.scan()
            .snapshot
            .agents
            .into_iter()
            .find(|v| v.agent == AgentId::builtin("claude"))
            .unwrap()
    }

    fn plan(&self, edit: &Edit) -> Result<Preview, ApiError> {
        let scanned = self.scan();
        Planner {
            agents: registry(),
            scanned: &scanned,
            env: &self.env,
            parked_dir: &self.parked,
            changes: &NoHistory,
        }
        .plan(edit)
    }

    /// Plans, then writes the change set. Returns the preview.
    fn apply(&self, edit: &Edit) -> Preview {
        let preview = self.plan(edit).unwrap_or_else(|e| panic!("{edit:?}: {e}"));
        for op in &preview.change_set.ops {
            match &op.action {
                Action::Write { text } => self.fx.write(&op.path, text),
                Action::Trash => std::fs::remove_file(op.path.as_path()).unwrap(),
                Action::TrashDir => unreachable!(),
            }
        }
        preview
    }

    fn read(&self, path: &AbsPath) -> String {
        std::fs::read_to_string(path.as_path()).unwrap()
    }

    fn hooks(&self) -> Vec<Hook> {
        self.view().hooks
    }

    fn hook(&self, command: &str) -> Hook {
        self.hooks()
            .into_iter()
            .find(|h| h.handler.summary == command)
            .unwrap_or_else(|| panic!("no hook {command}"))
    }
}

fn cmd(command: &str) -> Value {
    json!({"type": "command", "command": command})
}

fn draft(event: &str, matcher: Option<&str>, handler: Value) -> HookDraft {
    HookDraft {
        event: event.to_owned(),
        matcher: matcher.map(str::to_owned),
        handler,
    }
}

fn add(file: &AbsPath, d: HookDraft) -> Edit {
    Edit::AddHook {
        agent: AgentId::builtin("claude"),
        file: file.clone(),
        draft: d,
    }
}

/// The part of `after` that differs from `before`, found by the longest common prefix and suffix.
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
        .take_while(|(a, b)| a == b)
        .count()
        .min(max_suffix);
    (
        before[prefix..before.len() - suffix].to_owned(),
        after[prefix..after.len() - suffix].to_owned(),
    )
}

/// Files with the layouts people actually have: the default, tabs, four spaces, one line, a
/// final line without a newline, long arrays on one line.
fn layouts() -> Vec<(&'static str, String)> {
    let two = r#"{
  "model": "opus",
  "permissions": {
    "allow": ["Bash(ls:*)", "Read"]
  },
  "hooks": {
    "PreToolUse": [
      {
        "matcher": "Bash",
        "hooks": [
          { "type": "command", "command": "keep-a.sh" }
        ]
      }
    ],
    "Stop": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "keep-b.sh",
            "timeout": 5
          }
        ]
      }
    ]
  },
  "env": {"A": "1"}
}
"#
    .to_owned();
    let tabs = two.replace("  ", "\t");
    let four = two.replace("  ", "    ");
    let one = r#"{"model":"opus","hooks":{"PreToolUse":[{"matcher":"Bash","hooks":[{"type":"command","command":"keep-a.sh"}]}],"Stop":[{"hooks":[{"type":"command","command":"keep-b.sh","timeout":5}]}]},"env":{"A":"1"}}"#.to_owned();
    let no_newline = two.trim_end().to_owned();
    let crlf = two.replace('\n', "\r\n");
    vec![
        ("two spaces", two),
        ("windows line endings", crlf),
        ("tabs", tabs),
        ("four spaces", four),
        ("one line", one),
        ("no final newline", no_newline),
    ]
}

fn parses(text: &str) -> Value {
    serde_json::from_str(text).unwrap_or_else(|e| panic!("not JSON ({e}):\n{text}"))
}

/// Everything of the file's value except `hooks`.
fn rest(value: &Value) -> Value {
    let mut value = value.clone();
    value.as_object_mut().unwrap().remove("hooks");
    value
}

// ---------------------------------------------------------------------------------------------
// Add, update, remove
// ---------------------------------------------------------------------------------------------

#[test]
fn adding_updating_and_removing_a_hook_changes_only_its_own_entry_in_every_layout() {
    for (name, start) in layouts() {
        let rig = Rig::new();
        let file = rig.user_settings();
        rig.fx.write(&file, &start);
        let original = parses(&start);

        // Add: a new event, a handler in an existing group, and a new group for a new matcher.
        for (event, matcher, command) in [
            ("SessionStart", None, "new-session.sh"),
            ("PreToolUse", Some("Bash"), "joins-group.sh"),
            ("PreToolUse", Some("Edit"), "new-group.sh"),
        ] {
            let before = rig.read(&file);
            let preview = rig.apply(&add(&file, draft(event, matcher, cmd(command))));
            let after = rig.read(&file);
            assert_eq!(preview.change_set.ops.len(), 1, "{name}");
            let (gone, came) = changed(&before, &after);
            assert!(
                gone.trim().is_empty() || gone.trim() == ",",
                "{name}: an add replaced {gone:?}"
            );
            assert!(came.contains(command), "{name}: {came:?}");
            let value = parses(&after);
            assert_eq!(rest(&value), rest(&original), "{name}: another key moved");
            let live = rig.hook(command);
            assert_eq!(live.event, event);
            assert_eq!(live.matcher.raw.as_deref(), matcher);
            assert_eq!(live.state, HookState::Active, "{name}");
        }
        let joined = rig.hook("joins-group.sh");
        let keep = rig.hook("keep-a.sh");
        assert_eq!(
            joined.source.at,
            Locator::Json {
                pointer: "/hooks/PreToolUse/0/hooks/1".into()
            },
            "{name}: it joins the group of the same matcher"
        );
        assert_eq!(
            keep.source.at,
            Locator::Json {
                pointer: "/hooks/PreToolUse/0/hooks/0".into()
            }
        );

        // Remove what was added, last first: the file is as it started, byte for byte.
        for command in ["new-group.sh", "joins-group.sh", "new-session.sh"] {
            let id = rig.hook(command).id;
            let before = rig.read(&file);
            rig.apply(&Edit::RemoveHook { hook: id });
            let after = rig.read(&file);
            let (_, came) = changed(&before, &after);
            assert!(!came.contains(command), "{name}");
            assert!(!after.contains(command), "{name}");
            parses(&after);
        }
        assert_eq!(
            rig.read(&file),
            start,
            "{name}: add and remove restore the file"
        );

        // Update in place keeps the unknown keys of the handler as written. The handler is the
        // hook's own entry, so only it may change.
        let keep = rig.hook("keep-a.sh");
        let handler =
            json!({"type": "command", "command": "keep-a2.sh", "timeout": 9, "x-extra": [1, 2]});
        rig.apply(&Edit::UpdateHook {
            hook: keep.id.clone(),
            draft: draft("PreToolUse", Some("Bash"), handler.clone()),
        });
        let after = rig.read(&file);
        // Every byte outside the handler's own span is as it was.
        let span = super::splice::parse(&start)
            .unwrap()
            .find(&[
                Seg::Key("hooks".into()),
                Seg::Key("PreToolUse".into()),
                Seg::Index(0),
                Seg::Key("hooks".into()),
                Seg::Index(0),
            ])
            .map(|n| (n.start, n.end))
            .unwrap();
        assert!(after.starts_with(&start[..span.0]), "{name}");
        assert!(after.ends_with(&start[span.1..]), "{name}");
        assert!(
            after.contains("keep-a2.sh") && after.contains("x-extra"),
            "{name}"
        );
        assert_eq!(rig.hook("keep-a2.sh").handler.raw, handler);
        assert_eq!(rest(&parses(&after)), rest(&original), "{name}");
    }
}

#[test]
fn removing_the_last_handler_removes_its_group_and_the_last_group_removes_the_event() {
    let rig = Rig::new();
    let file = rig.user_settings();
    rig.fx.write(
        &file,
        "{\n  \"model\": \"opus\",\n  \"hooks\": {\n    \"Stop\": [\n      {\n        \"hooks\": [\n          { \"type\": \"command\", \"command\": \"only.sh\" }\n        ]\n      }\n    ]\n  }\n}\n",
    );
    rig.apply(&Edit::RemoveHook {
        hook: rig.hook("only.sh").id,
    });
    assert_eq!(rig.read(&file), "{\n  \"model\": \"opus\"\n}\n");
}

#[test]
fn a_group_with_two_handlers_loses_only_the_one_removed() {
    let rig = Rig::new();
    let file = rig.user_settings();
    let start = "{\n  \"hooks\": {\n    \"Stop\": [\n      {\n        \"hooks\": [\n          { \"type\": \"command\", \"command\": \"a.sh\" },\n          { \"type\": \"command\", \"command\": \"b.sh\" }\n        ]\n      },\n      {\n        \"matcher\": \"x\",\n        \"hooks\": [{ \"type\": \"command\", \"command\": \"c.sh\" }]\n      }\n    ]\n  }\n}\n";
    rig.fx.write(&file, start);
    rig.apply(&Edit::RemoveHook {
        hook: rig.hook("a.sh").id,
    });
    let value = parses(&rig.read(&file));
    assert_eq!(value["hooks"]["Stop"].as_array().unwrap().len(), 2);
    assert_eq!(
        value["hooks"]["Stop"][0]["hooks"].as_array().unwrap().len(),
        1
    );
    rig.apply(&Edit::RemoveHook {
        hook: rig.hook("c.sh").id,
    });
    let value = parses(&rig.read(&file));
    assert_eq!(value["hooks"]["Stop"].as_array().unwrap().len(), 1);
}

#[test]
fn an_update_that_changes_the_event_or_matcher_moves_the_hook() {
    let rig = Rig::new();
    let file = rig.user_settings();
    rig.fx.write(
        &file,
        &json!({"hooks": {"Stop": [{"hooks": [cmd("a.sh"), cmd("b.sh")]}]}}).to_string(),
    );
    rig.apply(&Edit::UpdateHook {
        hook: rig.hook("b.sh").id,
        draft: draft("PreToolUse", Some("Bash"), cmd("b.sh")),
    });
    let b = rig.hook("b.sh");
    assert_eq!(
        (b.event.as_str(), b.matcher.raw.as_deref()),
        ("PreToolUse", Some("Bash"))
    );
    assert_eq!(rig.hook("a.sh").event, "Stop");
    // A sole handler changes its matcher in place instead of leaving an empty group.
    rig.apply(&Edit::UpdateHook {
        hook: rig.hook("b.sh").id,
        draft: draft("PreToolUse", Some("Edit"), cmd("b.sh")),
    });
    let value = parses(&rig.read(&file));
    assert_eq!(value["hooks"]["PreToolUse"].as_array().unwrap().len(), 1);
    assert_eq!(value["hooks"]["PreToolUse"][0]["matcher"], "Edit");
}

#[test]
fn an_update_to_what_the_file_has_plans_to_nothing() {
    let rig = Rig::new();
    let file = rig.user_settings();
    rig.fx.write(
        &file,
        &json!({"hooks": {"Stop": [{"hooks": [cmd("a.sh")]}]}}).to_string(),
    );
    let preview = rig
        .plan(&Edit::UpdateHook {
            hook: rig.hook("a.sh").id,
            draft: draft("Stop", None, cmd("a.sh")),
        })
        .unwrap();
    assert!(preview.change_set.ops.is_empty());
}

#[test]
fn a_file_that_does_not_exist_yet_gets_a_valid_one() {
    let rig = Rig::new();
    let file = rig.fx.project_path(".claude/settings.local.json");
    let preview = rig.apply(&add(&file, draft("Stop", None, cmd("a.sh"))));
    assert_eq!(preview.change_set.ops[0].expect, Base::Absent);
    let value = parses(&rig.read(&file));
    assert_eq!(value["hooks"]["Stop"][0]["hooks"][0]["command"], "a.sh");
    assert!(preview.warnings.contains(&Warning::AgentAlsoWrites {
        agent: AgentId::builtin("claude")
    }));
}

#[test]
fn a_draft_claude_code_would_not_run_is_refused() {
    let rig = Rig::new();
    let file = rig.user_settings();
    rig.fx.write(&file, "{}\n");
    let why = |d: HookDraft| match rig.plan(&add(&file, d)).unwrap_err() {
        ApiError::InvalidEdit { field, why } => (field, why.0),
        other => panic!("{other:?}"),
    };
    assert_eq!(
        why(draft("NoSuchEvent", None, cmd("a.sh"))),
        ("event".into(), "claude.plan.unknown_event".into())
    );
    assert_eq!(
        why(draft("Stop", None, json!({"type": "command"}))),
        ("handler".into(), "claude.plan.invalid_handler".into())
    );
    assert_eq!(
        why(draft("PreToolUse", Some("(unclosed"), cmd("a.sh"))),
        ("matcher".into(), "claude.plan.invalid_matcher".into())
    );
    assert_eq!(
        rig.plan(&add(
            &rig.fx.project_path("elsewhere.json"),
            draft("Stop", None, cmd("a.sh"))
        ))
        .unwrap_err(),
        ApiError::InvalidEdit {
            field: "file".into(),
            why: RuleRef::new("claude.plan.hook_file")
        }
    );
}

#[test]
fn a_settings_file_that_is_not_json_is_not_touched() {
    let rig = Rig::new();
    let file = rig.user_settings();
    rig.fx.write(&file, "{ \"hooks\": ");
    assert!(matches!(
        rig.plan(&add(&file, draft("Stop", None, cmd("a.sh"))))
            .unwrap_err(),
        ApiError::InvalidEdit { .. }
    ));
}

#[test]
fn a_hook_edit_warns_when_it_takes_effect() {
    let rig = Rig::new();
    let file = rig.user_settings();
    rig.fx.write(&file, "{}\n");
    let preview = rig
        .plan(&add(&file, draft("Stop", None, cmd("a.sh"))))
        .unwrap();
    assert!(preview.warnings.contains(&Warning::TakesEffect {
        agent: AgentId::builtin("claude"),
        when: RuleRef::new("claude.takes_effect.settings_live"),
    }));
}

// ---------------------------------------------------------------------------------------------
// Stale ids, managed and plugin hooks
// ---------------------------------------------------------------------------------------------

#[test]
fn a_hook_id_from_before_the_entry_changed_is_not_found_and_nothing_is_planned() {
    let rig = Rig::new();
    let file = rig.user_settings();
    rig.fx.write(
        &file,
        &json!({"hooks": {"Stop": [{"hooks": [cmd("a.sh")]}]}}).to_string(),
    );
    let stale = rig.hook("a.sh").id;
    // Someone edits the entry, and another hook now sits at the same position.
    rig.fx.write(
        &file,
        &json!({"hooks": {"Stop": [{"hooks": [cmd("other.sh")]}]}}).to_string(),
    );
    let after_outside_edit = rig.read(&file);
    for edit in [
        Edit::RemoveHook {
            hook: stale.clone(),
        },
        Edit::UpdateHook {
            hook: stale.clone(),
            draft: draft("Stop", None, cmd("z.sh")),
        },
        Edit::SetHookEnabled {
            hook: stale.clone(),
            enabled: false,
        },
    ] {
        assert_eq!(rig.plan(&edit).unwrap_err(), ApiError::NotFound, "{edit:?}");
    }
    assert_eq!(rig.read(&file), after_outside_edit);
    assert!(!rig.parked.as_path().exists());
}

#[test]
fn a_managed_hook_cannot_be_switched_edited_or_removed() {
    let rig = Rig::new();
    let managed = rig.fx.root().join("managed");
    rig.fx.write(
        &managed.join("managed-settings.json"),
        &json!({"hooks": {"Stop": [{"hooks": [cmd("m.sh")]}]}}).to_string(),
    );
    let hook = rig.hook("m.sh");
    assert!(matches!(hook.toggle, Toggle::Unavailable { .. }));
    assert!(matches!(
        rig.plan(&Edit::SetHookEnabled {
            hook: hook.id.clone(),
            enabled: false
        })
        .unwrap_err(),
        ApiError::InvalidEdit { .. }
    ));
    assert!(matches!(
        rig.plan(&Edit::RemoveHook { hook: hook.id }).unwrap_err(),
        ApiError::ReadOnly { .. }
    ));
}

// ---------------------------------------------------------------------------------------------
// Parking
// ---------------------------------------------------------------------------------------------

#[test]
fn switching_a_hook_off_and_on_restores_the_file_byte_for_byte() {
    for (name, start) in layouts() {
        let rig = Rig::new();
        let file = rig.user_settings();
        rig.fx.write(&file, &start);
        for command in ["keep-a.sh", "keep-b.sh"] {
            let id = rig.hook(command).id;
            let off = rig.apply(&Edit::SetHookEnabled {
                hook: id,
                enabled: false,
            });
            assert!(
                off.warnings.contains(&Warning::ParkedOnThisMachine),
                "{name}"
            );
            assert_eq!(
                off.change_set.ops.len(),
                2,
                "{name}: the file and the record"
            );
            assert!(!rig.read(&file).contains(command), "{name}");
            let parked = rig.hook(command);
            assert_eq!(
                parked.state,
                HookState::Disabled {
                    by: DisabledBy::Parked
                },
                "{name}"
            );
            assert!(matches!(parked.source.at, Locator::Parked { .. }));

            // A second off plans to nothing.
            let again = rig
                .plan(&Edit::SetHookEnabled {
                    hook: parked.id.clone(),
                    enabled: false,
                })
                .unwrap();
            assert!(again.change_set.ops.is_empty(), "{name}");

            let on = rig.apply(&Edit::SetHookEnabled {
                hook: parked.id,
                enabled: true,
            });
            assert!(!on.warnings.contains(&Warning::ParkedOnThisMachine));
            assert_eq!(rig.read(&file), start, "{name}: {command} is back in place");
            assert_eq!(rig.hook(command).state, HookState::Active, "{name}");
            assert!(
                !parked_record(&rig).exists(),
                "{name}: the record goes with the last parked hook"
            );
        }
    }
}

fn parked_record(rig: &Rig) -> std::path::PathBuf {
    crate::parked::record_path(&rig.parked, &rig.user_settings())
        .as_path()
        .to_path_buf()
}

#[test]
fn several_parked_hooks_come_back_in_any_order() {
    let value = json!({"hooks": {"PreToolUse": [
        {"matcher": "Bash", "hooks": [cmd("a.sh"), cmd("b.sh"), cmd("c.sh")]},
        {"matcher": "Edit", "hooks": [cmd("d.sh")]},
    ]}});
    let canonical = format!("{}\n", serde_json::to_string_pretty(&value).unwrap());
    let compact = "{\n  \"hooks\": {\n    \"PreToolUse\": [\n      {\n        \"matcher\": \"Bash\",\n        \"hooks\": [\n          { \"type\": \"command\", \"command\": \"a.sh\" },\n          { \"type\": \"command\", \"command\": \"b.sh\" },\n          { \"type\": \"command\", \"command\": \"c.sh\" }\n        ]\n      },\n      {\n        \"matcher\": \"Edit\",\n        \"hooks\": [\n          { \"type\": \"command\", \"command\": \"d.sh\" }\n        ]\n      }\n    ]\n  }\n}\n";
    let parked_order = ["b.sh", "d.sh", "a.sh"];
    for (layout, start) in [("canonical", canonical.as_str()), ("compact", compact)] {
        for order in [
            ["b.sh", "d.sh", "a.sh"],
            ["d.sh", "b.sh", "a.sh"],
            ["a.sh", "b.sh", "d.sh"],
            ["a.sh", "d.sh", "b.sh"],
        ] {
            let rig = Rig::new();
            let file = rig.user_settings();
            rig.fx.write(&file, start);
            for command in parked_order {
                rig.apply(&Edit::SetHookEnabled {
                    hook: rig.hook(command).id,
                    enabled: false,
                });
            }
            let listed: Vec<String> = rig
                .hooks()
                .iter()
                .map(|h| h.handler.summary.clone())
                .collect();
            assert_eq!(
                listed,
                ["a.sh", "b.sh", "c.sh", "d.sh"],
                "{layout}: the scan keeps the order"
            );
            for command in order {
                rig.apply(&Edit::SetHookEnabled {
                    hook: rig.hook(command).id,
                    enabled: true,
                });
            }
            let after = rig.read(&file);
            // The file as it was parked from is restored exactly while nothing else touched it,
            // which holds for the last-parked-first order. Any other order puts the handlers
            // back by value, in the layout the editor writes.
            if layout == "canonical" || order == ["a.sh", "d.sh", "b.sh"] {
                assert_eq!(after, start, "{layout} {order:?}");
            } else {
                assert_eq!(parses(&after), value, "{layout} {order:?}");
            }
            assert!(!parked_record(&rig).exists(), "{layout} {order:?}");
        }
    }
}

#[test]
fn a_parked_hook_goes_back_to_a_new_group_when_its_group_is_gone() {
    let rig = Rig::new();
    let file = rig.user_settings();
    rig.fx.write(
        &file,
        &json!({"hooks": {"PreToolUse": [{"matcher": "Bash", "hooks": [cmd("a.sh")]}]}})
            .to_string(),
    );
    rig.apply(&Edit::SetHookEnabled {
        hook: rig.hook("a.sh").id,
        enabled: false,
    });
    // While it is parked, the file gets a different group.
    rig.apply(&add(&file, draft("PreToolUse", Some("Edit"), cmd("e.sh"))));
    rig.apply(&Edit::SetHookEnabled {
        hook: rig.hook("a.sh").id,
        enabled: true,
    });
    let value = parses(&rig.read(&file));
    let groups = value["hooks"]["PreToolUse"].as_array().unwrap();
    assert_eq!(groups.len(), 2);
    // It was the first handler of the event, so its group comes first.
    assert_eq!(groups[0]["matcher"], "Bash");
    assert_eq!(groups[1]["matcher"], "Edit");
}

#[test]
fn a_parked_hook_can_be_removed_or_updated_in_the_record() {
    let rig = Rig::new();
    let file = rig.user_settings();
    rig.fx.write(
        &file,
        &json!({"hooks": {"Stop": [{"hooks": [cmd("a.sh"), cmd("b.sh")]}]}}).to_string(),
    );
    let live = rig.read(&file);
    rig.apply(&Edit::SetHookEnabled {
        hook: rig.hook("a.sh").id,
        enabled: false,
    });
    rig.apply(&Edit::UpdateHook {
        hook: rig.hook("a.sh").id,
        draft: draft("Stop", None, cmd("a2.sh")),
    });
    assert_eq!(
        rig.hook("a2.sh").state,
        HookState::Disabled {
            by: DisabledBy::Parked
        }
    );
    rig.apply(&Edit::RemoveHook {
        hook: rig.hook("a2.sh").id,
    });
    assert!(!parked_record(&rig).exists());
    assert_ne!(rig.read(&file), live);
    assert!(
        !rig.hooks()
            .iter()
            .any(|h| h.handler.summary.starts_with("a"))
    );
}

#[test]
fn a_plugin_hook_cannot_be_switched() {
    let rig = Rig::new();
    // A plugin directory loaded with `--plugin-dir` style env is not reachable here, so use the
    // frontmatter case: a skill that defines a hook.
    rig.fx.write(
        &rig.fx.home_path(".claude/skills/s/SKILL.md"),
        "---\nname: s\ndescription: d\nhooks:\n  Stop:\n    - hooks:\n        - type: command\n          command: fm.sh\n---\nbody\n",
    );
    let hook = rig.hook("fm.sh");
    assert!(matches!(hook.toggle, Toggle::Unavailable { .. }));
    assert!(matches!(
        rig.plan(&Edit::SetHookEnabled {
            hook: hook.id.clone(),
            enabled: false
        })
        .unwrap_err(),
        ApiError::InvalidEdit { .. }
    ));
    assert_eq!(
        rig.plan(&Edit::RemoveHook { hook: hook.id }).unwrap_err(),
        ApiError::InvalidEdit {
            field: "hook".into(),
            why: RuleRef::new("claude.plan.hook_not_in_json")
        }
    );
}

#[test]
fn another_agents_edits_are_left_to_it() {
    let rig = Rig::new();
    let scanned = rig.scan();
    let ctx = crate::agents::PlanCx {
        env: &rig.env,
        fs: scanned.fs(),
        snapshot: &scanned.snapshot,
        parked_dir: &rig.parked,
    };
    assert!(
        Claude
            .plan(
                &ctx,
                &Edit::NewItem {
                    agent: AgentId::builtin("codex"),
                    slot: SlotId("codex:x".into()),
                    name: None
                }
            )
            .is_none()
    );
}
