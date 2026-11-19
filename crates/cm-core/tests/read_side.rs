//! The passes that run after the adapters, and the simulator and the search, on fixture trees.

use cm_core::agents::{ScanCx, registry};
use cm_core::probe_fs::ProbeFs;
use cm_core::scan::{Scanned, scan_full};
use cm_core::simulate::simulate;
use cm_core::testing::Fixture;
use cm_core::*;

fn view<'a>(scanned: &'a Scanned, agent: &str) -> &'a AgentView {
    scanned
        .snapshot
        .agents
        .iter()
        .find(|v| v.agent.as_str() == agent)
        .unwrap()
}

fn references(view: &AgentView) -> Vec<(String, u8)> {
    view.loads
        .iter()
        .filter_map(|load| match &load.via {
            Via::Reference { depth, .. } => {
                Some((load.file.file_name().unwrap().to_owned(), *depth))
            }
            _ => None,
        })
        .collect()
}

fn run_simulation(fx: &Fixture, env: &env::ScanEnv, target: &AbsPath) -> Vec<Simulation> {
    let scanned = {
        let mut agents = Vec::new();
        agents.extend(registry().iter().copied());
        cm_core::scan::scan_with(&agents, env)
    };
    let fs = scanned.fs();
    let cx = ScanCx { env, fs };
    let _ = fx;
    simulate(registry(), &cx, &scanned.snapshot, target)
}

fn sim<'a>(all: &'a [Simulation], agent: &str) -> &'a Simulation {
    all.iter().find(|s| s.agent.as_str() == agent).unwrap()
}

#[test]
fn references_follow_two_levels_under_the_agent_that_loads_the_document() {
    let fx = Fixture::new();
    fx.write(
        &fx.project_path("CLAUDE.md"),
        "# Claude\nSee [a](docs/a.md) and the rule [r](.claude/rules/r.md).\n",
    );
    fx.write(&fx.project_path("docs/a.md"), "Next: [b](b.md)\n");
    fx.write(&fx.project_path("docs/b.md"), "leaf\n");
    fx.write(&fx.project_path(".claude/rules/r.md"), "a rule\n");
    fx.write(
        &fx.project_path("AGENTS.md"),
        "Codex reads [x](docs/x.md)\n",
    );
    fx.write(&fx.project_path("docs/x.md"), "x\n");
    let scanned = scan_full(&fx.env());

    let claude = view(&scanned, "claude");
    assert_eq!(
        references(claude),
        [("a.md".to_owned(), 1), ("b.md".to_owned(), 2)],
        "the rule file is already a load, so it is not a reference"
    );
    let a = claude
        .loads
        .iter()
        .find(|l| l.file.file_name() == Some("a.md"))
        .unwrap();
    assert_eq!(a.when, Activation::ModelMayRead);
    assert_eq!(a.kind, DocKind::Reference);
    let b = claude
        .loads
        .iter()
        .find(|l| l.file.file_name() == Some("b.md"))
        .unwrap();
    assert!(matches!(&b.via, Via::Reference { host, line: 1, depth: 2 } if *host == a.id));
    assert!(
        scanned
            .snapshot
            .files
            .contains_key(&fx.project_path("docs/b.md"))
    );

    assert_eq!(
        references(view(&scanned, "codex")),
        [("x.md".to_owned(), 1)]
    );
    assert!(references(view(&scanned, "gemini")).is_empty());
}

#[test]
fn a_graph_over_500_nodes_stops_and_reports_it() {
    let fx = Fixture::new();
    let mut index = String::new();
    for i in 0..520 {
        fx.write(&fx.project_path(&format!("n/{i}.md")), "x\n");
        index.push_str(&format!("[n](n/{i}.md)\n"));
    }
    fx.write(&fx.project_path("CLAUDE.md"), &index);
    let scanned = scan_full(&fx.env());
    assert_eq!(references(view(&scanned, "claude")).len(), 500);
    assert!(
        scanned
            .snapshot
            .diagnostics
            .iter()
            .any(|d| matches!(d.problem, Problem::WalkCapped { entries: 500 }))
    );
}

#[test]
fn the_budget_is_the_sum_of_the_blocks() {
    let fx = Fixture::new();
    fx.write(
        &fx.project_path("CLAUDE.md"),
        "project rules for the budget\n",
    );
    fx.write(&fx.home_path(".claude/CLAUDE.md"), "user rules\n");
    fx.write(
        &fx.project_path("sub/CLAUDE.md"),
        "nested rules for later\n",
    );
    fx.write(&fx.project_path("AGENTS.md"), "codex rules\n");
    fx.write(&fx.project_path("GEMINI.md"), "gemini rules\n");
    let env = fx.env().with_var("GEMINI_CLI_TRUST_WORKSPACE", "true");
    let scanned = scan_full(&env);
    for agent in &scanned.snapshot.agents {
        let sum: u32 = agent.session.iter().map(|b| b.tokens).sum();
        assert_eq!(agent.budget.session_start, sum, "{}", agent.agent);
        let by_label: u32 = agent.budget.by_label.iter().map(|(_, t)| t).sum();
        assert_eq!(by_label, sum, "{}", agent.agent);
        let on_access: u32 = agent
            .loads
            .iter()
            .filter(|l| matches!(l.when, Activation::OnAccess { .. }))
            .filter_map(|l| l.delivered.as_ref())
            .map(|d| d.tokens)
            .sum();
        assert_eq!(agent.budget.on_access_max, on_access, "{}", agent.agent);
    }
    let claude = view(&scanned, "claude");
    assert!(claude.budget.session_start > 0 && claude.budget.on_access_max > 0);
    assert_eq!(view(&scanned, "codex").budget.on_access_max, 0);
}

#[test]
fn a_scan_builds_its_on_access_loads_from_the_same_rule_as_the_simulator() {
    let fx = Fixture::new();
    fx.mkdir(&fx.project_path(".git"));
    fx.write(&fx.project_path("CLAUDE.md"), "root\n");
    fx.write(&fx.project_path("GEMINI.md"), "root\n");
    fx.write(&fx.project_path("a/CLAUDE.md"), "a\n");
    fx.write(&fx.project_path("a/b/CLAUDE.md"), "b\n");
    fx.write(&fx.project_path("a/GEMINI.md"), "a\n");
    fx.write(&fx.project_path("a/b/GEMINI.md"), "b\n");
    fx.write(&fx.project_path("a/b/AGENTS.md"), "codex\n");
    let env = fx.env().with_var("GEMINI_CLI_TRUST_WORKSPACE", "true");
    let scanned = scan_full(&env);
    let fs = ProbeFs::new();
    let cx = ScanCx { env: &env, fs: &fs };
    let dirs: Vec<AbsPath> = cm_core::walk::walk(&env.cwd, &fs).dirs().cloned().collect();
    for agent in registry() {
        let view = view(&scanned, agent.id().as_str());
        let mut from_scan: Vec<AbsPath> = view
            .loads
            .iter()
            .filter(|l| matches!(l.when, Activation::OnAccess { .. }))
            .map(|l| l.file.clone())
            .collect();
        let mut from_rule: Vec<AbsPath> = dirs
            .iter()
            .flat_map(|dir| agent.on_access(&cx, dir))
            .filter(|c| c.file.starts_with(&env.cwd))
            .map(|c| c.file)
            .collect();
        from_scan.sort();
        from_scan.dedup();
        from_rule.sort();
        from_rule.dedup();
        assert_eq!(from_scan, from_rule, "{}", agent.id());
    }
}

#[test]
fn simulate_fires_nested_files_and_matching_rules_only() {
    let fx = Fixture::new();
    fx.mkdir(&fx.project_path(".git"));
    fx.write(&fx.project_path("CLAUDE.md"), "root\n");
    fx.write(&fx.project_path("GEMINI.md"), "root\n");
    fx.write(&fx.project_path("sub/CLAUDE.md"), "nested claude\n");
    fx.write(&fx.project_path("sub/GEMINI.md"), "nested gemini\n");
    fx.write(&fx.project_path("sub/AGENTS.md"), "nested codex\n");
    fx.write(
        &fx.project_path(".claude/rules/ts.md"),
        "---\npaths:\n  - \"**/*.ts\"\n---\nTS rule\n",
    );
    fx.write(
        &fx.project_path(".claude/rules/py.md"),
        "---\npaths:\n  - \"**/*.py\"\n---\nPY rule\n",
    );
    fx.write(&fx.project_path("sub/x.ts"), "");
    let env = fx.env().with_var("GEMINI_CLI_TRUST_WORKSPACE", "true");
    let all = run_simulation(&fx, &env, &fx.project_path("sub/x.ts"));

    let scanned = scan_full(&env);
    let names = |agent: &str, sim: &Simulation| -> Vec<String> {
        sim.fired
            .iter()
            .map(|id| {
                view(&scanned, agent).loads[id.0 as usize]
                    .file
                    .as_str()
                    .strip_prefix(fx.project().as_str())
                    .unwrap()
                    .to_owned()
            })
            .collect()
    };
    let claude = names("claude", sim(&all, "claude"));
    assert!(claude.contains(&"/sub/CLAUDE.md".to_owned()), "{claude:?}");
    assert!(
        claude.contains(&"/.claude/rules/ts.md".to_owned()),
        "{claude:?}"
    );
    assert!(
        !claude.contains(&"/.claude/rules/py.md".to_owned()),
        "{claude:?}"
    );
    assert!(sim(&all, "claude").added_tokens > 0);

    assert_eq!(names("gemini", sim(&all, "gemini")), ["/sub/GEMINI.md"]);
    assert!(sim(&all, "gemini").added_tokens > 0);

    let codex = sim(&all, "codex");
    assert!(codex.fired.is_empty() && codex.extra.is_empty());
    assert_eq!(codex.added_tokens, 0);
    assert_eq!(codex.notes, [RuleRef::new("codex.agents_md.below_cwd")]);
}

#[test]
fn simulate_finds_a_nested_file_inside_a_gitignored_directory() {
    let fx = Fixture::new();
    fx.mkdir(&fx.project_path(".git"));
    fx.write(&fx.project_path(".gitignore"), "build/\n");
    fx.write(&fx.project_path("CLAUDE.md"), "root\n");
    fx.write(&fx.project_path("GEMINI.md"), "root\n");
    fx.write(
        &fx.project_path("build/out/CLAUDE.md"),
        "generated claude\n",
    );
    fx.write(
        &fx.project_path("build/out/GEMINI.md"),
        "generated gemini\n",
    );
    fx.write(&fx.project_path("build/out/a.ts"), "");
    let env = fx.env().with_var("GEMINI_CLI_TRUST_WORKSPACE", "true");
    let scanned = scan_full(&env);
    assert!(
        !scanned
            .snapshot
            .files
            .contains_key(&fx.project_path("build/out/CLAUDE.md")),
        "the walk does not enter an ignored directory"
    );
    let all = run_simulation(&fx, &env, &fx.project_path("build/out/a.ts"));
    for (agent, name) in [("claude", "CLAUDE.md"), ("gemini", "GEMINI.md")] {
        let sim = sim(&all, agent);
        let files: Vec<_> = sim.extra.iter().map(|l| l.file.clone()).collect();
        assert_eq!(files, [fx.project_path("build/out").join(name)], "{agent}");
        assert!(sim.added_tokens > 0, "{agent}");
        assert!(matches!(sim.extra[0].via, Via::Discovery { .. }));
    }
}

fn problems(scanned: &Scanned) -> Vec<(Problem, Subject)> {
    scanned
        .snapshot
        .diagnostics
        .iter()
        .map(|d| (d.problem.clone(), d.at.clone()))
        .collect()
}

#[test]
fn a_link_to_a_missing_file_is_reported_with_its_line() {
    let fx = Fixture::new();
    fx.write(
        &fx.project_path("CLAUDE.md"),
        "# T\nsee [gone](docs/gone.md#x), [ok](docs/ok.md), [web](https://x.dev/a.md), `[code](nope.md)`\n",
    );
    fx.write(&fx.project_path("docs/ok.md"), "fine\n");
    let scanned = scan_full(&fx.env());
    let missing: Vec<_> = problems(&scanned)
        .into_iter()
        .filter(|(p, _)| matches!(p, Problem::ReferenceMissing { .. }))
        .collect();
    assert_eq!(
        missing,
        [(
            Problem::ReferenceMissing {
                raw: "docs/gone.md#x".to_owned()
            },
            Subject::File {
                path: fx.project_path("CLAUDE.md"),
                line: Some(2)
            }
        )]
    );
}

#[test]
fn a_hook_with_no_program_or_no_execute_bit_is_diagnosed() {
    use std::os::unix::fs::PermissionsExt;
    let fx = Fixture::new();
    let bin = fx.root().join("bin");
    fx.write(&bin.join("tool"), "#!/bin/sh\n");
    std::fs::set_permissions(bin.join("tool").as_path(), PermissionsExt::from_mode(0o755)).unwrap();
    fx.write(&fx.project_path("hooks/plain.sh"), "#!/bin/sh\n");
    fx.write(&fx.project_path("hooks/run.sh"), "#!/bin/sh\n");
    std::fs::set_permissions(
        fx.project_path("hooks/run.sh").as_path(),
        PermissionsExt::from_mode(0o755),
    )
    .unwrap();
    let hook = |command: &str| serde_json::json!({"matcher": "*", "hooks": [{"type": "command", "command": command}]});
    let settings = serde_json::json!({"hooks": {"PreToolUse": [
        hook("missing-tool --flag"),
        hook("tool run"),
        hook("./hooks/plain.sh"),
        hook("\"$CLAUDE_PROJECT_DIR\"/hooks/run.sh"),
        hook("cd /tmp && tool"),
    ]}});
    fx.write(
        &fx.project_path(".claude/settings.json"),
        &settings.to_string(),
    );
    let env = fx.env().with_var("PATH", &format!("/nonexistent:{bin}"));
    let scanned = scan_full(&env);
    let found: Vec<_> = problems(&scanned)
        .into_iter()
        .filter_map(|(p, _)| match p {
            Problem::HookProgramMissing { program } => Some(format!("missing {program}")),
            Problem::HookNotExecutable { path } => Some(format!(
                "not executable {}",
                path.as_str().strip_prefix(fx.project().as_str()).unwrap()
            )),
            _ => None,
        })
        .collect();
    assert_eq!(
        found,
        ["missing missing-tool", "not executable /hooks/plain.sh"]
    );
}

#[test]
fn a_config_file_an_agent_uses_that_does_not_parse_is_diagnosed_once() {
    use cm_core::scan::scan_with;
    use cm_core::testing::TestAgent;
    let fx = Fixture::new();
    let bad = fx.project_path("bad.json");
    let toml = fx.project_path("bad.toml");
    let good = fx.project_path("good.json");
    fx.write(&bad, "{\"a\": ");
    fx.write(&toml, "a = ");
    fx.write(&good, "{\"a\": 1} // comment is fine");
    let agent = TestAgent {
        files: vec![bad.clone(), toml.clone(), good],
        ..TestAgent::default()
    };
    let scanned = scan_with(&[&agent], &fx.env());
    let errors: Vec<_> = problems(&scanned)
        .into_iter()
        .filter_map(|(p, at)| match (p, at) {
            (Problem::ParseError { format, .. }, Subject::File { path, .. }) => {
                Some((format, path))
            }
            _ => None,
        })
        .collect();
    assert_eq!(errors, [(Format::Json, bad), (Format::Toml, toml)]);
}

#[test]
fn search_finds_plain_and_regex_matches_with_positions_and_caps_at_500() {
    use cm_core::search::search;
    let fx = Fixture::new();
    fx.write(
        &fx.project_path("CLAUDE.md"),
        "alpha Beta\nsecond line beta\n",
    );
    let scanned = scan_full(&fx.env());
    let req = |query: &str, regex: bool, case: bool| SearchReq {
        cwd: fx.project(),
        query: query.to_owned(),
        regex,
        case_sensitive: case,
    };
    let hits = search(&scanned, &req("beta", false, false)).unwrap().hits;
    let spots: Vec<_> = hits.iter().map(|h| (h.line, h.column)).collect();
    assert_eq!(spots, [(1, 7), (2, 13)]);
    assert_eq!(hits[0].text, "alpha Beta");
    assert_eq!(hits[0].file, fx.project_path("CLAUDE.md"));
    let exact = search(&scanned, &req("beta", false, true)).unwrap().hits;
    assert_eq!(exact.len(), 1);
    let re = search(&scanned, &req("^s\\w+ line", true, true))
        .unwrap()
        .hits;
    assert_eq!((re.len(), re[0].line), (1, 2));
    assert!(matches!(
        search(&scanned, &req("(", true, true)),
        Err(ApiError::InvalidEdit { .. })
    ));

    fx.write(&fx.project_path("CLAUDE.md"), &"x\n".repeat(600));
    let scanned = scan_full(&fx.env());
    let big = search(&scanned, &req("x", false, true)).unwrap();
    assert_eq!((big.hits.len(), big.capped), (500, true));
}
