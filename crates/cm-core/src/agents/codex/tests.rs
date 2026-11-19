//! Behaviour of the Codex adapter on fixture homes. The cases that compare with the installed
//! `codex` live in `tests/codex_oracle.rs`. These cover what the oracle cannot show: memories,
//! subagents, plugins, the guard and the rule texts.

use super::*;
use crate::env::ScanEnv;
use crate::probe_fs::ProbeFs;
use crate::testing::Fixture;

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

    fn env(&self) -> ScanEnv {
        self.fx
            .env()
            .with_var("CODEX_HOME", self.fx.home_path(".codex").as_str())
    }

    fn scan(&self) -> AgentScan {
        let env = self.env();
        let fs = ProbeFs::new();
        Codex.scan(&ScanCx {
            env: &env,
            fs: &fs,
            parked_dir: None,
        })
    }

    fn trust_project(&self) {
        let key = self.fx.project();
        let path = self.codex("config.toml");
        let old = std::fs::read_to_string(path.as_path()).unwrap_or_default();
        self.fx.write(
            &path,
            &format!("{old}\n[projects.\"{key}\"]\ntrust_level = \"trusted\"\n"),
        );
    }
}

fn load_of<'a>(scan: &'a AgentScan, suffix: &str) -> &'a Load {
    scan.view
        .loads
        .iter()
        .find(|l| l.file.as_str().ends_with(suffix))
        .unwrap_or_else(|| panic!("no load for {suffix}"))
}

// --- memories -------------------------------------------------------------------------------

#[test]
fn memories_are_off_by_default_and_the_summary_is_not_sent() {
    let home = Home::new();
    home.write("memories/memory_summary.md", "I like tabs.\n");
    home.write("memories/MEMORY.md", "# registry\n");
    let scan = home.scan();
    let store = &scan.view.memory[0];
    assert_eq!(store.root, home.codex("memories"));
    assert!(matches!(store.enabled, Enabled::Off { .. }));
    assert_eq!(store.index, Some(home.codex("memories/MEMORY.md")));
    assert!(matches!(
        load_of(&scan, "memory_summary.md").status,
        LoadStatus::Skipped {
            skip: Skip::FeatureOff { .. }
        }
    ));
    assert!(
        scan.view
            .session
            .iter()
            .all(|b| b.label != BlockLabel::Memory)
    );
}

#[test]
fn enabled_memories_send_only_the_summary_and_cut_it_at_2500_tokens() {
    let home = Home::new();
    home.write("config.toml", "[features]\nmemories = true\n");
    home.write("memories/memory_summary.md", &"word ".repeat(4000));
    home.write("memories/MEMORY.md", "never sent\n");
    home.write("memories/extensions/ad_hoc/notes/one.md", "remember this\n");
    let scan = home.scan();
    let summary = load_of(&scan, "memory_summary.md");
    assert!(
        matches!(
            &summary.status,
            LoadStatus::Truncated { limit, kept: 2500, total } if limit.kind == LimitKind::SummaryTokens && *total > 2500
        ),
        "{:?}",
        summary.status
    );
    assert!(
        scan.view
            .loads
            .iter()
            .all(|l| !l.file.as_str().ends_with("/MEMORY.md")),
        "MEMORY.md is never injected"
    );
    let store = &scan.view.memory[0];
    assert!(matches!(store.write, MemoryWrite::NotesDir { .. }));
    assert!(
        store
            .entries
            .iter()
            .any(|e| e.role == MemoryRole::Note && e.file.as_str().ends_with("one.md"))
    );
    let block = scan
        .view
        .session
        .iter()
        .find(|b| b.label == BlockLabel::Memory)
        .expect("a memory block");
    assert_eq!(block.fidelity, Fidelity::Approximate);
    let generated = scan
        .files
        .iter()
        .find(|f| f.path.as_str().ends_with("memory_summary.md"))
        .unwrap();
    assert_eq!(
        generated.policy,
        EditPolicy::ReadOnly {
            why: ReadOnlyWhy::Generated
        }
    );
}

#[test]
fn enabling_memories_selects_v2_only_when_no_v1_directory_exists() {
    let home = Home::new();
    home.write("config.toml", "[features]\nmemories = true\n");
    assert_eq!(home.scan().view.memory[0].root, home.codex("memories_v2"));
    home.write("memories/memory_summary.md", "x\n");
    assert_eq!(home.scan().view.memory[0].root, home.codex("memories"));
}

// --- subagents ------------------------------------------------------------------------------

#[test]
fn subagent_roles_come_from_files_and_config_entries_and_the_higher_layer_wins() {
    let home = Home::new();
    home.write(
        "agents/reviewer.toml",
        "name = \"reviewer\"\ndescription = \"user reviewer\"\ndeveloper_instructions = \"be kind\"\n",
    );
    home.write(
        "agents/nested/dir/second.toml",
        "description = \"second\"\n",
    );
    home.write("agents/broken.toml", "name = \"broken\"\n");
    home.write("agents/ignored.json", "{}");
    home.write(
        "config.toml",
        "[agents.listed]\nconfig_file = \"agents/listed.toml\"\ndescription = \"from the table\"\n",
    );
    home.write("agents/listed.toml", "developer_instructions = \"x\"\n");
    home.fx.write(
        &home.fx.project_path(".codex/agents/reviewer.toml"),
        "name = \"reviewer\"\ndescription = \"project reviewer\"\n",
    );
    home.trust_project();

    let scan = home.scan();
    let named = |name: &str| -> Vec<&Extension> {
        scan.view
            .extensions
            .iter()
            .filter(|e| e.ext_kind == ExtKind::Subagent && e.name == name)
            .collect()
    };
    assert_eq!(
        named("second").len(),
        1,
        "the filename gives the name, recursion finds it"
    );
    assert_eq!(
        named("listed").len(),
        1,
        "a file named by config_file is found once"
    );
    let reviewers = named("reviewer");
    assert_eq!(reviewers.len(), 2);
    assert!(
        reviewers
            .iter()
            .any(|e| matches!(e.status, ExtStatus::Shadowed { .. }) && e.scope == Scope::User)
    );
    assert!(
        reviewers
            .iter()
            .any(|e| e.status == ExtStatus::Active && matches!(e.scope, Scope::Project { .. }))
    );
    assert!(matches!(
        named("broken")[0].status,
        ExtStatus::Rejected { .. }
    ));
    assert!(named("ignored").is_empty(), "only .toml files count");
}

#[test]
fn project_subagents_need_trust() {
    let home = Home::new();
    home.fx.write(
        &home.fx.project_path(".codex/agents/p.toml"),
        "description = \"p\"\n",
    );
    let scan = home.scan();
    let role = scan
        .view
        .extensions
        .iter()
        .find(|e| e.name == "p")
        .expect("listed");
    assert!(matches!(role.status, ExtStatus::Disabled { .. }));
    home.trust_project();
    let role = home
        .scan()
        .view
        .extensions
        .into_iter()
        .find(|e| e.name == "p")
        .unwrap();
    assert_eq!(role.status, ExtStatus::Active);
}

// --- plugins, legacy prompts, homes ----------------------------------------------------------

#[test]
fn only_configured_plugins_load_and_their_skills_are_namespaced_and_read_only() {
    let home = Home::new();
    let skill = "---\nname: lint\ndescription: lints\n---\nbody\n";
    for (version, tag) in [("1.0.0", "old"), ("1.2.0", "new")] {
        home.write(
            &format!("plugins/cache/mkt/demo/{version}/.codex-plugin/plugin.json"),
            "{\"name\":\"demo\"}",
        );
        home.write(
            &format!("plugins/cache/mkt/demo/{version}/skills/lint/SKILL.md"),
            &skill.replace("lints", tag),
        );
    }
    home.write(
        "plugins/cache/mkt/other/1.0.0/skills/x/SKILL.md",
        "---\ndescription: x\n---\n",
    );
    home.write("config.toml", "[plugins.\"demo@mkt\"]\nenabled = true\n");
    let scan = home.scan();
    let lint = scan
        .view
        .extensions
        .iter()
        .find(|e| e.name == "demo:lint")
        .expect("namespaced skill");
    assert_eq!(
        lint.description.as_deref(),
        Some("new"),
        "the highest version is active"
    );
    assert!(matches!(lint.scope, Scope::Plugin { .. }));
    assert!(scan.view.extensions.iter().all(|e| !e.name.ends_with(":x")));
    let claim = scan.files.iter().find(|f| f.path == lint.file).unwrap();
    assert!(matches!(
        claim.policy,
        EditPolicy::ReadOnly {
            why: ReadOnlyWhy::PluginCache { .. }
        }
    ));
    home.write("config.toml", "[plugins.\"demo@mkt\"]\nenabled = false\n");
    assert!(
        home.scan()
            .view
            .extensions
            .iter()
            .all(|e| e.name != "demo:lint")
    );
}

#[test]
fn legacy_prompts_are_listed_as_not_read() {
    let home = Home::new();
    home.write("prompts/review.md", "Review this.\n");
    let scan = home.scan();
    let load = load_of(&scan, "prompts/review.md");
    assert!(matches!(
        &load.status,
        LoadStatus::Skipped {
            skip: Skip::NotRead { .. }
        }
    ));
    assert!(
        scan.diagnostics
            .iter()
            .any(|d| matches!(d.problem, Problem::LegacyLocation { .. }))
    );
}

#[test]
fn a_broken_user_config_is_a_fatal_parse_error() {
    let home = Home::new();
    home.write("config.toml", "this is = = not toml");
    let scan = home.scan();
    assert!(scan.diagnostics.iter().any(|d| matches!(
        d.problem,
        Problem::ParseError {
            format: Format::Toml,
            fatal_for_agent: true,
            ..
        }
    )));
}

#[test]
fn known_projects_come_from_the_projects_table() {
    let home = Home::new();
    home.write(
        "config.toml",
        "[projects.\"/work/a\"]\ntrust_level = \"trusted\"\n[projects.\"/work/b\"]\ntrust_level = \"untrusted\"\n",
    );
    let env = home.env();
    let fs = ProbeFs::new();
    let projects = Codex.known_projects(&ScanCx {
        env: &env,
        fs: &fs,
        parked_dir: None,
    });
    let names: Vec<_> = projects.iter().map(|p| p.as_str().to_owned()).collect();
    assert_eq!(names, ["/work/a", "/work/b"]);
}

// --- guard ----------------------------------------------------------------------------------

const BASE: &str = "# my notes\n[hooks.state.\"/h/hooks.json:stop:0:0\"]\nenabled = true\ntrusted_hash = \"sha256:aa\"\n";

fn config_path() -> AbsPath {
    AbsPath::parse("/home/u/.codex/config.toml").unwrap()
}

#[test]
fn the_guard_refuses_any_change_to_a_trusted_hash() {
    let path = config_path();
    let forbidden = |before: Option<&str>, after: Option<&str>| {
        matches!(
            Codex.guard(&path, before, after),
            Err(ApiError::Forbidden { .. })
        )
    };
    // Changed.
    assert!(forbidden(
        Some(BASE),
        Some(&BASE.replace("sha256:aa", "sha256:bb"))
    ));
    // Added to a hook that had none.
    assert!(forbidden(
        Some("[hooks.state.\"k\"]\nenabled = false\n"),
        Some("[hooks.state.\"k\"]\nenabled = false\ntrusted_hash = \"sha256:cc\"\n")
    ));
    // Added in a new file.
    assert!(forbidden(None, Some(BASE)));
    // Removed.
    assert!(forbidden(
        Some(BASE),
        Some("[hooks.state.\"/h/hooks.json:stop:0:0\"]\nenabled = true\n")
    ));
    // The plugin content hash counts too.
    assert!(forbidden(
        Some("[hooks.state.\"k\"]\ntrusted_hash = \"a\"\n"),
        Some("[hooks.state.\"k\"]\ntrusted_hash = \"a\"\ntrusted_content_hash = \"b\"\n")
    ));
    // The file is deleted while it holds a hash.
    assert!(forbidden(Some(BASE), None));
}

#[test]
fn the_guard_allows_the_enabled_flag_and_every_other_file() {
    let path = config_path();
    let flipped = BASE.replace("enabled = true", "enabled = false");
    assert!(Codex.guard(&path, Some(BASE), Some(&flipped)).is_ok());
    assert!(Codex.guard(&path, Some(BASE), Some(BASE)).is_ok());
    let other = AbsPath::parse("/home/u/.codex/AGENTS.md").unwrap();
    assert!(
        Codex
            .guard(&other, None, Some("trusted_hash = \"x\""))
            .is_ok()
    );
    let profile = AbsPath::parse("/home/u/.codex/work.config.toml").unwrap();
    assert!(
        Codex
            .guard(&profile, Some(BASE), Some(&BASE.replace("aa", "bb")))
            .is_err(),
        "a profile config is guarded too"
    );
}

// --- hook schema and runs --------------------------------------------------------------------

#[test]
fn the_hook_schema_lists_14_events_and_the_matcher_kind_of_each() {
    let schema = Codex.hook_schema();
    assert_eq!(schema.events.len(), 14);
    let kind = |name: &str| {
        schema
            .events
            .iter()
            .find(|e| e.name == name)
            .unwrap()
            .matcher
    };
    assert_eq!(kind("PreToolUse"), MatcherKind::ExactOrRegex);
    assert_eq!(kind("SessionStart"), MatcherKind::ExactOrRegex);
    assert_eq!(kind("Stop"), MatcherKind::None);
    assert_eq!(kind("UserPromptSubmit"), MatcherKind::None);
    let kinds: Vec<_> = schema
        .handler_kinds
        .iter()
        .map(|k| k.kind.as_str())
        .collect();
    assert_eq!(kinds, ["command", "mcp_tool", "prompt", "agent"]);
}

#[test]
fn sample_input_carries_the_fields_each_event_requires() {
    let home = Home::new();
    let env = home.env();
    let fs = ProbeFs::new();
    let cx = ScanCx {
        env: &env,
        fs: &fs,
        parked_dir: None,
    };
    for event in hooks::EVENTS {
        let input = Codex.sample_input(event, &cx);
        assert_eq!(input["hook_event_name"], event);
        assert_eq!(input["cwd"], home.fx.project().as_str());
        assert!(input["session_id"].is_string());
    }
    assert_eq!(Codex.sample_input("SessionStart", &cx)["source"], "startup");
    assert_eq!(Codex.sample_input("PreToolUse", &cx)["tool_name"], "Bash");
}

#[test]
fn a_command_hook_runs_through_the_login_shell_in_the_session_directory() {
    let home = Home::new();
    home.write(
        "hooks.json",
        r#"{"hooks":{"Stop":[{"hooks":[{"type":"command","command":"echo hi"},{"type":"mcp_tool","server":"s","tool":"t"}]}]}}"#,
    );
    let scan = home.scan();
    let env = home.env();
    let fs = ProbeFs::new();
    let cx = ScanCx {
        env: &env,
        fs: &fs,
        parked_dir: None,
    };
    let command = scan
        .view
        .hooks
        .iter()
        .find(|h| h.handler.handler_kind == "command")
        .unwrap();
    let spec = Codex
        .exec_spec(command, &serde_json::json!({"a": 1}), &cx)
        .unwrap();
    assert_eq!(spec.argv, ["/bin/sh", "-lc", "echo hi"]);
    assert_eq!(spec.cwd, home.fx.project());
    assert_eq!(spec.stdin, "{\"a\":1}");
    let mcp = scan
        .view
        .hooks
        .iter()
        .find(|h| h.handler.handler_kind == "mcp_tool")
        .unwrap();
    assert!(Codex.exec_spec(mcp, &serde_json::Value::Null, &cx).is_err());
}

#[test]
fn hook_timeouts_follow_the_event_defaults_and_the_session_end_clamp() {
    let home = Home::new();
    home.write(
        "hooks.json",
        r#"{"hooks":{
  "Stop":[{"hooks":[{"type":"command","command":"a"},{"type":"command","command":"b","timeout":0}]}],
  "SessionEnd":[{"hooks":[{"type":"command","command":"c"},{"type":"command","command":"d","timeout":99}]}]
}}"#,
    );
    let ms: Vec<_> = home
        .scan()
        .view
        .hooks
        .iter()
        .map(|h| (h.handler.summary.clone(), h.handler.timeout_ms))
        .collect();
    assert_eq!(
        ms,
        [
            ("c".to_owned(), Some(1000)),
            ("d".to_owned(), Some(3000)),
            ("a".to_owned(), Some(600_000)),
            ("b".to_owned(), Some(1000)),
        ]
    );
}

#[test]
fn an_unknown_event_and_a_bad_regex_are_reported() {
    let home = Home::new();
    home.write(
        "hooks.json",
        r#"{"hooks":{"Bogus":[],"PreToolUse":[{"matcher":"(","hooks":[{"type":"command","command":"x"}]}],
  "Stop":[{"matcher":"x","hooks":[{"type":"command","command":"y"}]}]}}"#,
    );
    let scan = home.scan();
    let problems: Vec<_> = scan.diagnostics.iter().map(|d| &d.problem).collect();
    assert!(
        problems
            .iter()
            .any(|p| matches!(p, Problem::HookUnknownEvent { event } if event == "Bogus"))
    );
    assert!(
        problems
            .iter()
            .any(|p| matches!(p, Problem::HookMatcherInvalid { .. }))
    );
    assert!(
        problems
            .iter()
            .any(|p| matches!(p, Problem::HookMatcherIgnored))
    );
    let stop = scan.view.hooks.iter().find(|h| h.event == "Stop").unwrap();
    assert_eq!(stop.matcher.eval, MatcherEval::Ignored);
}

#[test]
fn hook_toggles_are_native_for_user_hooks_and_unavailable_for_prompt_handlers() {
    let home = Home::new();
    home.write(
        "hooks.json",
        r#"{"hooks":{"Stop":[{"hooks":[{"type":"command","command":"x"},{"type":"prompt"}]}]}}"#,
    );
    let scan = home.scan();
    assert_eq!(scan.view.hooks[0].toggle, Toggle::Native);
    assert!(matches!(
        scan.view.hooks[1].toggle,
        Toggle::Unavailable { .. }
    ));
    assert!(
        scan.view
            .hook_files
            .iter()
            .any(|f| f.format == HookFormat::CodexHooksJson && f.exists)
    );
}

// --- rule texts -------------------------------------------------------------------------------

/// Every `RuleRef` a scan of a rich fixture produces has an entry in `rules.toml`.
#[test]
fn every_rule_ref_a_scan_produces_is_in_rules_toml() {
    let home = Home::new();
    home.write("AGENTS.md", "global\n");
    home.write("prompts/old.md", "x\n");
    home.write(
        "skills/off/SKILL.md",
        "---\ndescription: d\nmetadata: {}\n---\n",
    );
    home.write("skills/bad/SKILL.md", "no frontmatter\n");
    home.write("skills/hidden/SKILL.md", "---\ndescription: d\n---\n");
    home.write(
        "skills/hidden/agents/openai.yaml",
        "policy:\n  allow_implicit_invocation: false\n",
    );
    home.write(
        "config.toml",
        "developer_instructions = \"hi\"\nmodel_instructions_file = \"base.md\"\n[features]\nmemories = true\n[[skills.config]]\nname = \"off\"\nenabled = false\n",
    );
    home.write("base.md", "base\n");
    home.write("memories/memory_summary.md", "s\n");
    home.write("agents/a.toml", "description = \"a\"\n");
    home.write("agents/b.toml", "x = 1\n");
    home.write(
        "hooks.json",
        r#"{"hooks":{"Stop":[{"hooks":[{"type":"command","command":"x"},{"type":"agent"}]}]}}"#,
    );
    home.fx.write(&home.fx.project_path("AGENTS.md"), "p\n");
    home.fx.write(&home.fx.project_path("sub/AGENTS.md"), "p\n");
    home.fx.write(
        &home.fx.project_path(".codex/agents/p.toml"),
        "description = \"p\"\n",
    );
    let scan = home.scan();

    let mut refs: Vec<String> = Vec::new();
    refs.extend(scan.view.homes.iter().map(|h| h.role.0.clone()));
    for load in &scan.view.loads {
        if let Via::Discovery { rule } = &load.via {
            refs.push(rule.0.clone());
        }
        if let LoadStatus::Skipped {
            skip: Skip::NotRead { why },
        } = &load.status
        {
            refs.push(why.0.clone());
        }
    }
    for ext in &scan.view.extensions {
        match &ext.status {
            ExtStatus::Hidden { why } | ExtStatus::Rejected { why } => refs.push(why.0.clone()),
            _ => {}
        }
    }
    for hook in &scan.view.hooks {
        if let Toggle::Unavailable { why } = &hook.toggle {
            refs.push(why.0.clone());
        }
    }
    refs.extend(scan.view.slots.iter().map(|s| s.rule.0.clone()));
    refs.extend(scan.view.memory.iter().map(|m| m.label.0.clone()));
    for assumption in &scan.view.assumptions {
        if let Assumption::Unknown { what } = assumption {
            refs.push(what.0.clone());
        }
    }
    for d in &scan.diagnostics {
        match &d.problem {
            Problem::LegacyLocation { why } | Problem::ExtensionRejected { why } => {
                refs.push(why.0.clone())
            }
            Problem::ProjectUntrusted { skipped } => {
                refs.extend(skipped.iter().map(|r| r.0.clone()))
            }
            _ => {}
        }
    }
    refs.push(extras::RULE_GUARD.to_owned());
    refs.push(extras::RULE_RUN_MCP.to_owned());
    let known: Vec<_> = Codex.rules().iter().map(|r| r.id.0.as_str()).collect();
    for rule in refs {
        assert!(
            known.contains(&rule.as_str()),
            "{rule} is not in rules.toml"
        );
    }
    for spec in Codex.rules() {
        assert!(
            spec.en.ends_with('.') && spec.ko.ends_with('.'),
            "{} is a sentence",
            spec.id.0
        );
    }
}
