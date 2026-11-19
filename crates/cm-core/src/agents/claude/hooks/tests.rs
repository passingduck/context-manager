use super::*;
use crate::agents::claude::extensions;
use crate::agents::claude::setup::TEST_MANAGED_DIR_VAR;
use crate::env::ScanEnv;
use crate::probe_fs::ProbeFs;
use crate::testing::Fixture;
use serde_json::json;

fn scan_with(env: &ScanEnv) -> HooksOut {
    let fs = ProbeFs::new();
    let cx = ScanCx { env, fs: &fs };
    let setup = Setup::load(&cx);
    let ext = extensions::scan(&cx, &setup, 0);
    scan(&cx, &setup, &ext)
}

fn managed_env(fx: &Fixture) -> (ScanEnv, AbsPath) {
    let managed = fx.root().join("managed");
    fx.mkdir(&fx.project_path(".git"));
    (
        fx.env().with_var(TEST_MANAGED_DIR_VAR, managed.as_str()),
        managed,
    )
}

fn cmd(command: &str) -> Value {
    json!({"type": "command", "command": command})
}

fn settings(hooks: Value) -> String {
    json!({ "hooks": hooks }).to_string()
}

fn by_event<'a>(out: &'a HooksOut, event: &str) -> Vec<&'a Hook> {
    out.hooks.iter().filter(|h| h.event == event).collect()
}

#[test]
fn settings_hooks_get_a_pointer_a_matcher_and_a_park_toggle() {
    // Spec 3, Definition locations, Schema, Matcher semantics.
    let fx = Fixture::new();
    fx.write(
        &fx.home_path(".claude/settings.json"),
        &settings(json!({
            "PreToolUse": [
                {"matcher": "Bash|Edit", "hooks": [cmd("a.sh")]},
                {"matcher": "Edit.*", "hooks": [cmd("b.sh"), cmd("c.sh")]},
            ],
            "Stop": [{"hooks": [cmd("d.sh")]}],
        })),
    );
    let (env, _) = managed_env(&fx);
    let out = scan_with(&env);
    assert_eq!(out.hooks.len(), 4);

    let pre = by_event(&out, "PreToolUse");
    assert_eq!(
        pre[0].source.at,
        Locator::Json {
            pointer: "/hooks/PreToolUse/0/hooks/0".to_owned()
        }
    );
    assert_eq!(
        pre[2].source.at,
        Locator::Json {
            pointer: "/hooks/PreToolUse/1/hooks/1".to_owned()
        }
    );
    assert_eq!(
        pre[0].matcher.eval,
        MatcherEval::Exact {
            values: vec!["Bash".to_owned(), "Edit".to_owned()]
        }
    );
    assert_eq!(pre[1].matcher.eval, MatcherEval::Regex);
    assert_eq!(pre[0].handler.summary, "a.sh");
    assert_eq!(pre[0].handler.handler_kind, "command");
    assert_eq!(pre[0].state, HookState::Active);
    assert_eq!(pre[0].toggle, Toggle::Park);
    assert_eq!(pre[0].source.scope, Scope::User);
    assert_eq!(by_event(&out, "Stop")[0].matcher.eval, MatcherEval::All);
    assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
}

#[test]
fn the_raw_handler_keeps_unknown_keys() {
    let fx = Fixture::new();
    fx.write(
        &fx.home_path(".claude/settings.json"),
        &settings(json!({"Stop": [{"hooks": [{"type": "command", "command": "x", "bogus": 1, "timeout": 2.5}]}]})),
    );
    let (env, _) = managed_env(&fx);
    let out = scan_with(&env);
    let hook = &out.hooks[0];
    assert_eq!(hook.handler.raw["bogus"], 1);
    assert_eq!(hook.handler.timeout_ms, Some(2500));
}

#[test]
fn managed_hooks_come_first_and_cannot_be_switched() {
    let fx = Fixture::new();
    let (env, managed) = managed_env(&fx);
    fx.write(
        &managed.join("managed-settings.json"),
        &settings(json!({"Stop": [{"hooks": [cmd("org.sh")]}]})),
    );
    fx.write(
        &managed.join("managed-settings.d/10-extra.json"),
        &settings(json!({"Stop": [{"hooks": [cmd("extra.sh")]}]})),
    );
    fx.write(
        &fx.home_path(".claude/remote-settings.json"),
        &settings(json!({"Stop": [{"hooks": [cmd("remote.sh")]}]})),
    );
    fx.write(
        &fx.home_path(".claude/settings.json"),
        &settings(json!({"Stop": [{"hooks": [cmd("mine.sh")]}]})),
    );
    let out = scan_with(&env);
    let order: Vec<_> = out
        .hooks
        .iter()
        .map(|h| h.handler.summary.as_str())
        .collect();
    assert_eq!(order, ["org.sh", "extra.sh", "remote.sh", "mine.sh"]);
    for hook in &out.hooks[..3] {
        assert_eq!(hook.source.scope, Scope::Managed);
        assert_eq!(
            hook.toggle,
            Toggle::Unavailable {
                why: RuleRef::new("claude.hook.toggle_managed")
            }
        );
    }
    // Read-only hook files are listed; the user's file is editable.
    let files: Vec<_> = out
        .hook_files
        .iter()
        .map(|f| (f.file.file_name().unwrap().to_owned(), f.policy.clone()))
        .collect();
    assert!(files.contains(&(
        "managed-settings.json".to_owned(),
        EditPolicy::ReadOnly {
            why: ReadOnlyWhy::Managed
        }
    )));
    assert!(files.contains(&("settings.json".to_owned(), EditPolicy::Editable)));
}

#[test]
fn disable_all_hooks_turns_off_everything_but_managed_hooks() {
    // Spec 3, Switches: the value after precedence wins; a project false beats a user true.
    let fx = Fixture::new();
    let (env, managed) = managed_env(&fx);
    fx.write(
        &managed.join("managed-settings.json"),
        &settings(json!({"Stop": [{"hooks": [cmd("org.sh")]}]})),
    );
    fx.write(
        &fx.home_path(".claude/settings.json"),
        &json!({"disableAllHooks": true, "hooks": {"Stop": [{"hooks": [cmd("mine.sh")]}]}})
            .to_string(),
    );
    let out = scan_with(&env);
    let user_file = fx.home_path(".claude/settings.json");
    assert_eq!(out.hooks[0].state, HookState::Active);
    assert_eq!(
        out.hooks[1].state,
        HookState::Disabled {
            by: DisabledBy::AllHooksOff { set_in: user_file }
        }
    );

    fx.write(
        &fx.project_path(".claude/settings.json"),
        &json!({"disableAllHooks": false}).to_string(),
    );
    let out = scan_with(&env);
    assert_eq!(out.hooks[1].state, HookState::Active);
}

#[test]
fn managed_disable_all_hooks_also_stops_managed_hooks() {
    let fx = Fixture::new();
    let (env, managed) = managed_env(&fx);
    fx.write(
        &managed.join("managed-settings.json"),
        &json!({"disableAllHooks": true, "hooks": {"Stop": [{"hooks": [cmd("org.sh")]}]}})
            .to_string(),
    );
    let out = scan_with(&env);
    assert!(matches!(out.hooks[0].state, HookState::Disabled { .. }));
}

#[test]
fn allow_managed_hooks_only_blocks_everything_else() {
    let fx = Fixture::new();
    let (env, managed) = managed_env(&fx);
    fx.write(
        &managed.join("managed-settings.json"),
        &json!({"allowManagedHooksOnly": true, "hooks": {"Stop": [{"hooks": [cmd("org.sh")]}]}})
            .to_string(),
    );
    fx.write(
        &fx.home_path(".claude/settings.json"),
        &settings(json!({"Stop": [{"hooks": [cmd("mine.sh")]}]})),
    );
    // Only a managed source counts: the same key in a user file does nothing.
    let out = scan_with(&env);
    assert_eq!(out.hooks[0].state, HookState::Active);
    assert_eq!(
        out.hooks[1].state,
        HookState::WillNotRun {
            why: HookBlock::ManagedOnly
        }
    );
}

#[test]
fn an_untrusted_folder_holds_back_settings_hooks_only() {
    let fx = Fixture::new();
    let (env, _) = managed_env(&fx);
    fx.write(
        &fx.home_path(".claude/settings.json"),
        &settings(json!({"Stop": [{"hooks": [cmd("mine.sh")]}]})),
    );
    fx.write(
        &fx.home_path(".claude.json"),
        &json!({"projects": {fx.project().as_str(): {"hasTrustDialogAccepted": false}}})
            .to_string(),
    );
    let out = scan_with(&env);
    assert_eq!(
        out.hooks[0].state,
        HookState::WillNotRun {
            why: HookBlock::FolderUntrusted
        }
    );

    fx.write(
        &fx.home_path(".claude.json"),
        &json!({"projects": {fx.project().as_str(): {"hasTrustDialogAccepted": true}}}).to_string(),
    );
    assert_eq!(scan_with(&env).hooks[0].state, HookState::Active);
}

#[test]
fn bad_entries_follow_spec_3() {
    let fx = Fixture::new();
    fx.write(
        &fx.home_path(".claude/settings.json"),
        &settings(json!({
            "NotAnEvent": [{"hooks": [cmd("x")]}],
            "Stop": [{"hooks": [{"type": "comand", "command": "x"}]}],
            "PreToolUse": [{"hooks": [{"type": "command"}]}],
            "Setup": [{"hooks": [{"type": "http", "url": "https://example.com"}]}],
            "SessionStart": [{"hooks": [{"type": "command", "command": "x", "if": "Bash(ls)"}]}],
        })),
    );
    let (env, _) = managed_env(&fx);
    let out = scan_with(&env);
    let kinds: Vec<_> = out
        .diagnostics
        .iter()
        .map(|d| (d.severity, d.problem.clone()))
        .collect();
    assert!(kinds.contains(&(
        Severity::Warning,
        Problem::HookUnknownEvent {
            event: "NotAnEvent".to_owned()
        }
    )));
    // An unknown handler type under Stop: that handler alone is ignored.
    let stop = &by_event(&out, "Stop")[0];
    assert_eq!(
        stop.state,
        HookState::WillNotRun {
            why: HookBlock::UnsupportedType
        }
    );
    // Any broken PreToolUse entry is an error on the file.
    assert!(out.diagnostics.iter().any(|d| d.severity == Severity::Error
        && matches!(&d.problem, Problem::ParseError { detail, .. } if detail.starts_with("/hooks/PreToolUse/0/hooks/0"))));
    assert_eq!(
        by_event(&out, "PreToolUse")[0].state,
        HookState::WillNotRun {
            why: HookBlock::UnsupportedType
        }
    );
    // http on Setup is not supported; `if` on SessionStart never runs.
    assert_eq!(
        by_event(&out, "Setup")[0].state,
        HookState::WillNotRun {
            why: HookBlock::UnsupportedType
        }
    );
    assert_eq!(
        by_event(&out, "SessionStart")[0].state,
        HookState::WillNotRun {
            why: HookBlock::UnsupportedType
        }
    );
    // The unknown event is still listed, and cannot run.
    assert_eq!(
        by_event(&out, "NotAnEvent")[0].state,
        HookState::WillNotRun {
            why: HookBlock::UnsupportedType
        }
    );
}

#[test]
fn matcher_values_are_checked_per_event() {
    let fx = Fixture::new();
    fx.write(
        &fx.home_path(".claude/settings.json"),
        &settings(json!({
            "SessionStart": [{"matcher": "init|startup", "hooks": [cmd("x")]}],
            "UserPromptSubmit": [{"matcher": "Bash", "hooks": [cmd("y")]}],
            "PostToolUse": [{"matcher": "(", "hooks": [cmd("z")]}],
            "FileChanged": [{"matcher": ".envrc|a-b", "hooks": [cmd("w")]}],
            "Stop": [{"matcher": "*", "hooks": [cmd("v")]}],
        })),
    );
    let (env, _) = managed_env(&fx);
    let out = scan_with(&env);
    let problems: Vec<_> = out.diagnostics.iter().map(|d| d.problem.clone()).collect();
    // The oh-my-claudecode case: `init` belongs to Setup, not SessionStart.
    assert!(problems.contains(&Problem::HookMatcherInvalid {
        error: "unknown_value:init".to_owned()
    }));
    assert!(problems.contains(&Problem::HookMatcherIgnored));
    assert_eq!(
        by_event(&out, "PostToolUse")[0].matcher.eval,
        MatcherEval::Invalid {
            error: "unterminated_group".to_owned()
        }
    );
    assert_eq!(
        by_event(&out, "UserPromptSubmit")[0].matcher.eval,
        MatcherEval::Ignored
    );
    assert_eq!(
        by_event(&out, "FileChanged")[0].matcher.eval,
        MatcherEval::Regex
    );
    assert_eq!(by_event(&out, "Stop")[0].matcher.eval, MatcherEval::All);
}

#[test]
fn js_regex_validity_approximation() {
    for ok in [
        "Edit.*",
        "^(Edit|Write)$",
        "mcp__srv__.*",
        "[a-z]+",
        "a(?:b)?c",
        "a*?",
    ] {
        assert_eq!(js_regex_error(ok), None, "{ok}");
    }
    assert_eq!(js_regex_error("("), Some("unterminated_group"));
    assert_eq!(js_regex_error(")"), Some("unmatched_paren"));
    assert_eq!(js_regex_error("[abc"), Some("unterminated_class"));
    assert_eq!(js_regex_error("*a"), Some("nothing_to_repeat"));
    assert_eq!(js_regex_error("a\\"), Some("trailing_backslash"));
}

fn plugin_with_hooks(fx: &Fixture, name: &str, enabled: bool) -> AbsPath {
    let root = fx.home_path(&format!(".claude/plugins/cache/mkt/{name}/1.0.0"));
    fx.write(
        &root.join(".claude-plugin/plugin.json"),
        &json!({"name": name, "hooks": {"Stop": [{"hooks": [cmd("inline.sh")]}]}}).to_string(),
    );
    fx.write(
        &root.join("hooks/hooks.json"),
        &settings(json!({"SessionStart": [{"matcher": "startup|resume|clear|compact", "hooks": [
            {"type": "command", "command": "\"${CLAUDE_PLUGIN_ROOT}/hooks/session-start.sh\" claude", "statusMessage": "Loading"}
        ]}]})),
    );
    let _ = enabled;
    root
}

#[test]
fn plugin_hooks_are_active_when_enabled_and_never_switchable() {
    // Spec 3, "Ground truth": the pstack hooks file.
    let fx = Fixture::new();
    let on = plugin_with_hooks(&fx, "on", true);
    let off = plugin_with_hooks(&fx, "off", false);
    fx.write(
        &fx.home_path(".claude/plugins/installed_plugins.json"),
        &json!({"version": 2, "plugins": {
            "on@mkt": [{"scope": "user", "installPath": on.as_str()}],
            "off@mkt": [{"scope": "user", "installPath": off.as_str()}],
        }})
        .to_string(),
    );
    fx.write(
        &fx.home_path(".claude/settings.json"),
        &json!({"enabledPlugins": {"on@mkt": true, "off@mkt": false}}).to_string(),
    );
    let (env, _) = managed_env(&fx);
    let out = scan_with(&env);

    let on_hooks: Vec<_> = out
        .hooks
        .iter()
        .filter(|h| {
            h.source.scope
                == Scope::Plugin {
                    id: "on@mkt".to_owned(),
                }
        })
        .collect();
    assert_eq!(on_hooks.len(), 2);
    for hook in &on_hooks {
        assert_eq!(hook.state, HookState::Active);
        assert_eq!(
            hook.toggle,
            Toggle::Unavailable {
                why: RuleRef::new("claude.hook.toggle_plugin")
            }
        );
    }
    let start = on_hooks.iter().find(|h| h.event == "SessionStart").unwrap();
    assert_eq!(start.source.file, on.join("hooks/hooks.json"));
    assert_eq!(
        start.matcher.eval,
        MatcherEval::Exact {
            values: ["startup", "resume", "clear", "compact"]
                .map(str::to_owned)
                .to_vec()
        }
    );
    // The manifest `hooks` key is a separate source from `hooks/hooks.json`.
    let inline = on_hooks.iter().find(|h| h.event == "Stop").unwrap();
    assert_eq!(inline.source.file, on.join(".claude-plugin/plugin.json"));
    assert_eq!(
        inline.source.at,
        Locator::Json {
            pointer: "/hooks/Stop/0/hooks/0".to_owned()
        }
    );

    for hook in out.hooks.iter().filter(|h| {
        h.source.scope
            == Scope::Plugin {
                id: "off@mkt".to_owned(),
            }
    }) {
        assert_eq!(
            hook.state,
            HookState::WillNotRun {
                why: HookBlock::PluginDisabled
            }
        );
    }
}

#[test]
fn a_manifest_path_naming_the_standard_hooks_file_loads_it_once() {
    let fx = Fixture::new();
    let root = fx.home_path(".claude/plugins/cache/mkt/p/1.0.0");
    fx.write(
        &root.join(".claude-plugin/plugin.json"),
        &json!({"name": "p", "hooks": "./hooks/hooks.json"}).to_string(),
    );
    fx.write(
        &root.join("hooks/hooks.json"),
        &settings(json!({"Stop": [{"hooks": [cmd("a.sh")]}]})),
    );
    fx.write(
        &fx.home_path(".claude/plugins/installed_plugins.json"),
        &json!({"version": 2, "plugins": {"p@mkt": [{"scope": "user", "installPath": root.as_str()}]}}).to_string(),
    );
    let (env, _) = managed_env(&fx);
    assert_eq!(scan_with(&env).hooks.len(), 1);
}

#[test]
fn frontmatter_hooks_register_on_invoke_and_plugin_agents_ignore_them() {
    let fx = Fixture::new();
    let front = "hooks:\n  PreToolUse:\n    - matcher: Bash\n      hooks:\n        - type: command\n          command: ./check.sh\n";
    fx.write(
        &fx.project_path(".claude/skills/guarded/SKILL.md"),
        &format!("---\ndescription: g\n{front}---\nbody\n"),
    );
    fx.write(
        &fx.project_path(".claude/agents/watcher.md"),
        &format!("---\nname: watcher\ndescription: w\n{front}---\nprompt\n"),
    );
    let root = fx.home_path(".claude/plugins/cache/mkt/p/1.0.0");
    fx.write(&root.join(".claude-plugin/plugin.json"), "{\"name\":\"p\"}");
    fx.write(
        &root.join("agents/ignored.md"),
        &format!("---\nname: ignored\ndescription: w\n{front}---\np\n"),
    );
    fx.write(
        &fx.home_path(".claude/plugins/installed_plugins.json"),
        &json!({"version": 2, "plugins": {"p@mkt": [{"scope": "user", "installPath": root.as_str()}]}}).to_string(),
    );
    let (env, _) = managed_env(&fx);
    let out = scan_with(&env);
    assert_eq!(out.hooks.len(), 2);
    let skill = out
        .hooks
        .iter()
        .find(|h| matches!(h.source.registered, Registration::OnSkillInvoke { .. }))
        .unwrap();
    assert!(matches!(
        &skill.source.at,
        Locator::Frontmatter { path } if path == &["hooks", "PreToolUse", "0", "hooks", "0"]
    ));
    assert_eq!(
        skill.matcher.eval,
        MatcherEval::Exact {
            values: vec!["Bash".to_owned()]
        }
    );
    assert_eq!(
        skill.toggle,
        Toggle::Unavailable {
            why: RuleRef::new("claude.hook.toggle_frontmatter")
        }
    );
    let agent = out
        .hooks
        .iter()
        .find(|h| matches!(h.source.registered, Registration::WhileSubagent { .. }))
        .unwrap();
    assert!(agent.source.file.as_str().ends_with("agents/watcher.md"));
}

#[test]
fn exec_spec_builds_the_shell_form_and_the_exec_form() {
    let fx = Fixture::new();
    fx.write(
        &fx.home_path(".claude/settings.json"),
        &settings(json!({"Stop": [{"hooks": [
            cmd("echo \"${CLAUDE_PROJECT_DIR}\""),
            {"type": "command", "command": "node", "args": ["${CLAUDE_PROJECT_DIR}/x.js", "--v"]},
            cmd("\"${CLAUDE_PLUGIN_ROOT}/x.sh\""),
            {"type": "http", "url": "https://example.com"},
        ]}]})),
    );
    let (env, _) = managed_env(&fx);
    let fs = ProbeFs::new();
    let cx = ScanCx { env: &env, fs: &fs };
    let out = scan_with(&env);
    let input = json!({"session_id": "abc", "hook_event_name": "Stop"});
    let project = fx.project();

    let shell = exec_spec(&out.hooks[0], &input, &cx).unwrap();
    assert_eq!(shell.argv, ["sh", "-c", &format!("echo \"{project}\"")]);
    assert_eq!(shell.cwd, project);
    assert!(
        shell
            .env
            .contains(&("CLAUDE_PROJECT_DIR".to_owned(), project.to_string()))
    );
    assert!(
        shell
            .env
            .contains(&("CLAUDE_CODE_SESSION_ID".to_owned(), "abc".to_owned()))
    );
    assert_eq!(serde_json::from_str::<Value>(&shell.stdin).unwrap(), input);

    let exec = exec_spec(&out.hooks[1], &input, &cx).unwrap();
    assert_eq!(exec.argv, ["node", &format!("{project}/x.js"), "--v"]);

    assert_eq!(
        exec_spec(&out.hooks[2], &input, &cx).unwrap_err(),
        RuleRef::new("claude.hook.plugin_variable_outside_plugin")
    );
    assert_eq!(
        exec_spec(&out.hooks[3], &input, &cx).unwrap_err(),
        RuleRef::new("claude.hook.run_unsupported")
    );
}

#[test]
fn exec_spec_resolves_the_plugin_root_for_plugin_hooks() {
    let fx = Fixture::new();
    let root = plugin_with_hooks(&fx, "on", true);
    fx.write(
        &fx.home_path(".claude/plugins/installed_plugins.json"),
        &json!({"version": 2, "plugins": {"on@mkt": [{"scope": "user", "installPath": root.as_str()}]}}).to_string(),
    );
    let (env, _) = managed_env(&fx);
    let fs = ProbeFs::new();
    let cx = ScanCx { env: &env, fs: &fs };
    let out = scan_with(&env);
    let hook = by_event(&out, "SessionStart")[0];
    let spec = exec_spec(hook, &sample_input("SessionStart", &cx), &cx).unwrap();
    assert_eq!(
        spec.argv[2],
        format!("\"{root}/hooks/session-start.sh\" claude")
    );
    assert!(
        spec.env
            .contains(&("CLAUDE_PLUGIN_ROOT".to_owned(), root.to_string()))
    );
    assert!(
        spec.env
            .iter()
            .any(|(k, v)| k == "CLAUDE_PLUGIN_DATA" && v.ends_with("/plugins/data/on-mkt"))
    );
}

#[test]
fn every_event_has_a_sample_input_naming_itself() {
    let fx = Fixture::new();
    let env = fx.env();
    let fs = ProbeFs::new();
    let cx = ScanCx { env: &env, fs: &fs };
    for event in &schema().events {
        let input = sample_input(&event.name, &cx);
        assert_eq!(input["hook_event_name"], event.name.as_str());
        assert_eq!(input["cwd"], fx.project().as_str());
    }
    assert_eq!(sample_input("PreToolUse", &cx)["tool_name"], "Bash");
}

#[test]
fn every_rule_ref_of_this_half_has_an_entry_in_both_languages() {
    // Rule ids in code are string literals that start with `claude.` and end in a letter.
    let sources = [
        include_str!("../hooks.rs"),
        include_str!("exec.rs"),
        include_str!("../extensions.rs"),
        include_str!("../extensions/plugins.rs"),
    ];
    let rules: toml::Table = include_str!("../rules_ext.toml").parse().unwrap();
    let entries = rules["rule"].as_array().unwrap();
    let mut seen = 0;
    for source in sources {
        for piece in source.split('"').skip(1).step_by(2) {
            let is_id = piece.starts_with("claude.")
                && piece.len() > 7
                && piece
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c == '.' || c == '_');
            if !is_id {
                continue;
            }
            seen += 1;
            let entry = entries
                .iter()
                .find(|e| e["id"].as_str() == Some(piece))
                .unwrap_or_else(|| panic!("{piece} is not in rules_ext.toml"));
            for key in ["en", "ko", "evidence"] {
                assert!(
                    !entry[key].as_str().unwrap().trim().is_empty(),
                    "{piece}.{key}"
                );
            }
        }
    }
    assert!(seen >= 10, "found only {seen} rule ids");
}
