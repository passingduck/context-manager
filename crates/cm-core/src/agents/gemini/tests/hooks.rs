use serde_json::json;

use super::*;
use crate::model::HookEventSpec;

fn hook_json(name: Option<&str>, command: &str) -> String {
    let name = name
        .map(|n| format!(r#""name":"{n}","#))
        .unwrap_or_default();
    format!(
        r#"{{"hooks":{{"BeforeTool":[{{"matcher":"^write","hooks":[{{"type":"command",{name}"command":"{command}"}}]}}]}}}}"#
    )
}

#[test]
fn schema_lists_the_eleven_events_with_their_matcher_kinds() {
    let schema = Gemini.hook_schema();
    let names: Vec<&str> = schema.events.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(
        names,
        [
            "BeforeTool",
            "AfterTool",
            "BeforeAgent",
            "AfterAgent",
            "BeforeModel",
            "AfterModel",
            "BeforeToolSelection",
            "SessionStart",
            "SessionEnd",
            "Notification",
            "PreCompress"
        ]
    );
    let kind = |name: &str| {
        schema
            .events
            .iter()
            .find(|e: &&HookEventSpec| e.name == name)
            .unwrap()
            .matcher
    };
    assert_eq!(kind("BeforeTool"), MatcherKind::Regex);
    assert_eq!(kind("SessionStart"), MatcherKind::Exact);
    assert_eq!(kind("PreCompress"), MatcherKind::Exact);
    assert_eq!(kind("BeforeAgent"), MatcherKind::None);
    assert_eq!(schema.handler_kinds.len(), 1);
}

#[test]
fn hooks_config_disabled_matches_by_name() {
    let fx = Fixture::new();
    user_settings(
        &fx,
        &format!(
            r#"{{"hooksConfig":{{"disabled":["block-secrets"]}},{}"#,
            &hook_json(Some("block-secrets"), "check.sh")[1..]
        ),
    );
    let scan = scan(&trusted(&fx));
    assert_eq!(
        scan.view.hooks[0].state,
        HookState::Disabled {
            by: DisabledBy::Native {
                set_in: fx.home_path(".gemini/settings.json")
            }
        }
    );
    assert_eq!(scan.view.hooks[0].toggle, Toggle::Native);
}

#[test]
fn hooks_config_disabled_matches_by_command_when_there_is_no_name() {
    let fx = Fixture::new();
    user_settings(
        &fx,
        &format!(
            r#"{{"hooksConfig":{{"disabled":["./check.sh"]}},{}"#,
            &hook_json(None, "./check.sh")[1..]
        ),
    );
    assert!(matches!(
        scan(&trusted(&fx)).view.hooks[0].state,
        HookState::Disabled {
            by: DisabledBy::Native { .. }
        }
    ));
    // A name hides the command: the entry no longer matches.
    user_settings(
        &fx,
        &format!(
            r#"{{"hooksConfig":{{"disabled":["./check.sh"]}},{}"#,
            &hook_json(Some("named"), "./check.sh")[1..]
        ),
    );
    assert_eq!(scan(&trusted(&fx)).view.hooks[0].state, HookState::Active);
}

#[test]
fn hooks_config_enabled_false_turns_off_every_hook() {
    let fx = Fixture::new();
    user_settings(
        &fx,
        &format!(
            r#"{{"hooksConfig":{{"enabled":false}},{}"#,
            &hook_json(None, "x")[1..]
        ),
    );
    assert_eq!(
        scan(&trusted(&fx)).view.hooks[0].state,
        HookState::Disabled {
            by: DisabledBy::AllHooksOff {
                set_in: fx.home_path(".gemini/settings.json")
            }
        }
    );
}

#[test]
fn hooks_of_every_layer_are_concatenated_in_merge_order() {
    // Spec 5: system defaults, user, workspace, system.
    let fx = Fixture::new();
    let sys = fx.root().join("sys");
    let one = |c: &str| {
        format!(
            r#"{{"hooks":{{"SessionStart":[{{"hooks":[{{"type":"command","command":"{c}"}}]}}]}}}}"#
        )
    };
    fx.write(&sys.join("system-defaults.json"), &one("defaults"));
    fx.write(&sys.join("settings.json"), &one("system"));
    user_settings(&fx, &one("user"));
    fx.write(&fx.project_path(".gemini/settings.json"), &one("workspace"));
    let env = trusted(&fx).with_var(
        "GEMINI_CLI_SYSTEM_SETTINGS_PATH",
        sys.join("settings.json").as_str(),
    );
    let scan = scan(&env);
    let order: Vec<_> = scan
        .view
        .hooks
        .iter()
        .map(|h| h.handler.summary.clone())
        .collect();
    assert_eq!(order, ["defaults", "user", "workspace", "system"]);
    let scopes: Vec<_> = scan
        .view
        .hooks
        .iter()
        .map(|h| h.source.scope.clone())
        .collect();
    assert_eq!(
        scopes,
        [
            Scope::Managed,
            Scope::User,
            Scope::Project { dir: fx.project() },
            Scope::Managed
        ]
    );
}

#[test]
fn matcher_evaluation_depends_on_the_event() {
    let fx = Fixture::new();
    user_settings(
        &fx,
        r#"{"hooks":{
            "BeforeTool":[{"matcher":"read","hooks":[{"type":"command","command":"a"}]}],
            "AfterTool":[{"matcher":"(","hooks":[{"type":"command","command":"b"}]}],
            "SessionStart":[{"matcher":"startup","hooks":[{"type":"command","command":"c"}]}],
            "BeforeAgent":[{"matcher":"x","hooks":[{"type":"command","command":"d"}]}],
            "SessionEnd":[{"matcher":"*","hooks":[{"type":"command","command":"e"}]}],
            "PreCompress":[{"hooks":[{"type":"command","command":"f"}]}]
        }}"#,
    );
    let scan = scan(&trusted(&fx));
    let eval = |c: &str| {
        scan.view
            .hooks
            .iter()
            .find(|h| h.handler.summary == c)
            .unwrap()
            .matcher
            .eval
            .clone()
    };
    assert_eq!(eval("a"), MatcherEval::Regex);
    assert_eq!(
        eval("b"),
        MatcherEval::Exact {
            values: vec!["(".to_owned()]
        }
    );
    assert_eq!(
        eval("c"),
        MatcherEval::Exact {
            values: vec!["startup".to_owned()]
        }
    );
    assert_eq!(eval("d"), MatcherEval::Ignored);
    assert_eq!(eval("e"), MatcherEval::All);
    assert_eq!(eval("f"), MatcherEval::All);
    assert!(
        problems(&scan)
            .iter()
            .any(|p| matches!(p, Problem::HookMatcherInvalid { .. }))
    );
    assert!(
        problems(&scan)
            .iter()
            .any(|p| matches!(p, Problem::HookMatcherIgnored))
    );
}

#[test]
fn an_unknown_event_is_reported_and_skipped_and_legacy_keys_are_not_events() {
    let fx = Fixture::new();
    user_settings(
        &fx,
        r#"{"hooks":{"enabled":true,"disabled":[],"PreToolUse":[{"hooks":[{"type":"command","command":"x"}]}]}}"#,
    );
    let scan = scan(&trusted(&fx));
    assert!(scan.view.hooks.is_empty());
    let unknown: Vec<_> = problems(&scan)
        .into_iter()
        .filter_map(|p| match p {
            Problem::HookUnknownEvent { event } => Some(event.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(unknown, ["PreToolUse"]);
}

#[test]
fn a_definition_without_a_hooks_array_is_discarded() {
    let fx = Fixture::new();
    user_settings(
        &fx,
        r#"{"hooks":{"BeforeTool":[{"matcher":"x"},{"hooks":"no"}]}}"#,
    );
    assert!(scan(&trusted(&fx)).view.hooks.is_empty());
}

#[test]
fn timeouts_are_milliseconds_with_a_default_of_sixty_seconds() {
    let fx = Fixture::new();
    user_settings(
        &fx,
        r#"{"hooks":{"SessionStart":[{"hooks":[
            {"type":"command","command":"a","timeout":5000},
            {"type":"command","command":"b"}]}]}}"#,
    );
    let scan = scan(&trusted(&fx));
    assert_eq!(scan.view.hooks[0].handler.timeout_ms, Some(5000));
    assert_eq!(scan.view.hooks[1].handler.timeout_ms, Some(60_000));
}

#[test]
fn a_hook_of_another_type_will_not_run() {
    let fx = Fixture::new();
    user_settings(
        &fx,
        r#"{"hooks":{"SessionStart":[{"hooks":[{"type":"plugin","name":"p"},{"type":"http","url":"u"}]}]}}"#,
    );
    let scan = scan(&trusted(&fx));
    assert_eq!(scan.view.hooks.len(), 1);
    assert_eq!(
        scan.view.hooks[0].state,
        HookState::WillNotRun {
            why: HookBlock::UnsupportedType
        }
    );
}

#[test]
fn hook_files_list_the_settings_files_a_hook_may_go_in() {
    let fx = Fixture::new();
    let scan = scan(&trusted(&fx));
    let files: Vec<_> = scan
        .view
        .hook_files
        .iter()
        .map(|f| (f.file.clone(), f.exists, f.policy.clone()))
        .collect();
    assert_eq!(
        files,
        [
            (
                fx.home_path(".gemini/settings.json"),
                false,
                EditPolicy::Editable
            ),
            (
                fx.project_path(".gemini/settings.json"),
                false,
                EditPolicy::Editable
            ),
        ]
    );
    assert!(
        scan.view
            .hook_files
            .iter()
            .all(|f| f.format == HookFormat::GeminiSettingsJsonc)
    );
}

#[test]
fn sample_input_carries_the_base_fields_and_the_event_fields() {
    let fx = Fixture::new();
    let env = fx.env();
    let fs = ProbeFs::new();
    let cx = ScanCx {
        env: &env,
        fs: &fs,
        parked_dir: None,
    };
    let input = Gemini.sample_input("BeforeTool", &cx);
    assert_eq!(input["hook_event_name"], "BeforeTool");
    assert_eq!(input["cwd"], fx.project().as_str());
    assert!(input["tool_name"].is_string() && input["tool_input"].is_object());
    assert_eq!(
        Gemini.sample_input("SessionStart", &cx)["source"],
        "startup"
    );
    assert_eq!(Gemini.sample_input("PreCompress", &cx)["trigger"], "auto");
}

#[test]
fn exec_spec_runs_bash_with_the_five_names_replaced_and_exported() {
    let fx = Fixture::new();
    user_settings(
        &fx,
        r#"{"hooks":{"BeforeTool":[{"hooks":[{"type":"command","command":"$GEMINI_PROJECT_DIR/.gemini/h.sh $GEMINI_SESSION_ID","env":{"EXTRA":"1"}}]}]}}"#,
    );
    let env = trusted(&fx);
    let fs = ProbeFs::new();
    let cx = ScanCx {
        env: &env,
        fs: &fs,
        parked_dir: None,
    };
    let scanned = Gemini.scan(&cx);
    let input = Gemini.sample_input("BeforeTool", &cx);
    let spec = Gemini
        .exec_spec(&scanned.view.hooks[0], &input, &cx)
        .unwrap();
    assert_eq!(spec.argv[..2], ["bash", "-c"]);
    assert_eq!(
        spec.argv[2],
        format!("{}/.gemini/h.sh sample-session", fx.project())
    );
    assert_eq!(spec.cwd, fx.project());
    assert!(spec.env.contains(&(
        "GEMINI_PROJECT_DIR".to_owned(),
        fx.project().as_str().to_owned()
    )));
    assert!(spec.env.contains(&(
        "CLAUDE_PROJECT_DIR".to_owned(),
        fx.project().as_str().to_owned()
    )));
    assert!(spec.env.contains(&("EXTRA".to_owned(), "1".to_owned())));
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&spec.stdin).unwrap(),
        input
    );
    let _ = json!(null);
}

#[test]
fn an_extension_hook_has_its_extension_path_substituted_for_a_test_run() {
    let fx = Fixture::new();
    let ext = fx.home_path(".gemini/extensions/e");
    fx.write(
        &ext.join("gemini-extension.json"),
        r#"{"name":"e","version":"1"}"#,
    );
    fx.write(
        &ext.join("hooks/hooks.json"),
        r#"{"hooks":{"SessionStart":[{"hooks":[{"type":"command","command":"${extensionPath}${/}x.sh"}]}]}}"#,
    );
    let env = fx.env();
    let fs = ProbeFs::new();
    let cx = ScanCx {
        env: &env,
        fs: &fs,
        parked_dir: None,
    };
    let scanned = Gemini.scan(&cx);
    let spec = Gemini
        .exec_spec(
            &scanned.view.hooks[0],
            &Gemini.sample_input("SessionStart", &cx),
            &cx,
        )
        .unwrap();
    assert_eq!(spec.argv[2], format!("{ext}/x.sh"));
}
