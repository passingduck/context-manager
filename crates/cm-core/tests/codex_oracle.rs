//! The Codex adapter against the installed `codex`. `codex debug prompt-input` prints the
//! model-visible prompt and `codex app-server` answers `hooks/list` (docs/agents/codex.md
//! section 7.3). Every case builds a fixture home and project in a temp directory and points
//! `HOME` and `CODEX_HOME` at it, so the real `~/.codex` is never read or written.
//!
//! The tests skip, and say so, when `codex` is not installed.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use cm_core::agents::codex::{Codex, TrustStatus, inspect_hooks};
use cm_core::agents::{Agent, ScanCx};
use cm_core::env::ScanEnv;
use cm_core::model::*;
use cm_core::probe_fs::ProbeFs;
use cm_core::testing::Fixture;
use serde_json::Value;

fn codex_installed() -> bool {
    let ok = Command::new("codex")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !ok {
        eprintln!("SKIPPED: `codex` is not installed, so there is no oracle to compare with");
    }
    ok
}

/// A fixture home and project laid out like a Codex user's, with a `.git` that has a `HEAD`.
struct Case {
    fx: Fixture,
}

impl Case {
    fn new() -> Case {
        let fx = Fixture::new();
        fx.mkdir(&fx.codex_home());
        fx.write(&fx.project_path(".git/HEAD"), "ref: refs/heads/main\n");
        Case { fx }
    }

    fn write(&self, path: &AbsPath, text: &str) {
        self.fx.write(path, text);
    }

    fn user(&self, rel: &str, text: &str) {
        self.write(&self.fx.codex_home().join(rel), text);
    }

    fn proj(&self, rel: &str, text: &str) {
        self.write(&self.fx.project_path(rel), text);
    }

    fn trust(&self, level: &str) {
        let key = self.fx.project();
        self.append_user_config(&format!(
            "[projects.\"{key}\"]\ntrust_level = \"{level}\"\n"
        ));
    }

    fn append_user_config(&self, text: &str) {
        let path = self.fx.codex_home().join("config.toml");
        let old = std::fs::read_to_string(path.as_path()).unwrap_or_default();
        self.write(&path, &format!("{old}{text}"));
    }

    fn env(&self, cwd: &AbsPath) -> ScanEnv {
        self.fx
            .env()
            .with_cwd(cwd.clone())
            .with_var("CODEX_HOME", self.fx.codex_home().as_str())
    }

    /// `codex debug prompt-input` from `cwd`: the list of items.
    fn prompt_input(&self, cwd: &AbsPath) -> Vec<Value> {
        let output = Command::new("codex")
            .args(["debug", "prompt-input"])
            .current_dir(cwd.as_path())
            .env("HOME", self.fx.home().as_str())
            .env("CODEX_HOME", self.fx.codex_home().as_str())
            .output()
            .expect("run codex");
        assert!(
            output.status.success(),
            "codex failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice::<Value>(&output.stdout)
            .expect("prompt-input prints JSON")
            .as_array()
            .expect("an array of items")
            .clone()
    }

    fn oracle_texts(&self, cwd: &AbsPath) -> Vec<(String, String)> {
        let mut texts = Vec::new();
        for item in self.prompt_input(cwd) {
            let role = item["role"].as_str().unwrap_or("").to_owned();
            for part in item["content"].as_array().into_iter().flatten() {
                if let Some(text) = part["text"].as_str() {
                    texts.push((role.clone(), text.to_owned()));
                }
            }
        }
        texts
    }

    fn oracle_agents_block(&self, cwd: &AbsPath) -> Option<String> {
        self.oracle_texts(cwd)
            .into_iter()
            .map(|(_, t)| t)
            .find(|t| t.starts_with("# AGENTS.md instructions"))
    }

    fn oracle_skills_block(&self, cwd: &AbsPath) -> Option<String> {
        self.oracle_texts(cwd)
            .into_iter()
            .map(|(_, t)| t)
            .find(|t| t.starts_with("<skills_instructions>"))
    }

    fn rendered(&self, cwd: &AbsPath) -> Vec<RenderedBlock> {
        let env = self.env(cwd);
        let fs = ProbeFs::new();
        let cx = ScanCx {
            env: &env,
            fs: &fs,
            parked_dir: None,
        };
        let view = Codex.scan(&cx).view;
        Codex.render(&cx, &view)
    }

    fn ours_agents_block(&self, cwd: &AbsPath) -> Option<String> {
        self.rendered(cwd)
            .into_iter()
            .find(|b| b.delivery == Delivery::UserMessage && b.label == BlockLabel::Instructions)
            .map(|b| b.text)
    }

    fn ours_skills_block(&self, cwd: &AbsPath) -> Option<String> {
        self.rendered(cwd)
            .into_iter()
            .find(|b| b.label == BlockLabel::SkillCatalog)
            .map(|b| b.text)
    }
}

trait FixtureExt {
    fn codex_home(&self) -> AbsPath;
}

impl FixtureExt for Fixture {
    fn codex_home(&self) -> AbsPath {
        self.home_path(".codex")
    }
}

#[track_caller]
fn same_instructions(case: &Case, cwd: &AbsPath) {
    let oracle = case.oracle_agents_block(cwd);
    let ours = case.ours_agents_block(cwd);
    assert_eq!(ours, oracle, "the AGENTS.md message differs from codex's");
}

// ---------------------------------------------------------------------------------------------
// Instructions (probe_codex.py cases A to H and Q)
// ---------------------------------------------------------------------------------------------

#[test]
fn oracle_instructions_chain_order_separator_and_below_cwd() {
    if !codex_installed() {
        return;
    }
    let case = Case::new();
    case.user("AGENTS.md", "\n  GLOBAL\n\n");
    case.proj("AGENTS.md", "ROOT\n");
    case.proj("sub/AGENTS.md", "SUB\n");
    case.proj("sub/deep/AGENTS.md", "DEEP\n");
    let cwd = case.fx.project_path("sub");
    same_instructions(&case, &cwd);
    let text = case.ours_agents_block(&cwd).unwrap();
    assert!(
        text.contains("GLOBAL\n\n--- project-doc ---\n\nROOT\n\n\nSUB"),
        "{text:?}"
    );
    assert!(
        !text.contains("DEEP"),
        "AGENTS.md below cwd is never loaded"
    );
}

#[test]
fn oracle_global_only_and_nothing_at_all() {
    if !codex_installed() {
        return;
    }
    let case = Case::new();
    let cwd = case.fx.project();
    same_instructions(&case, &cwd);
    assert_eq!(case.ours_agents_block(&cwd), None);
    case.user("AGENTS.md", "ONLY GLOBAL\n");
    same_instructions(&case, &cwd);
    assert!(case.ours_agents_block(&cwd).is_some());
}

#[test]
fn oracle_an_empty_project_override_shadows_agents_md() {
    if !codex_installed() {
        return;
    }
    let case = Case::new();
    case.proj("AGENTS.md", "ROOT\n");
    case.proj("sub/AGENTS.md", "SUB\n");
    case.proj("sub/AGENTS.override.md", "   \n");
    let cwd = case.fx.project_path("sub");
    same_instructions(&case, &cwd);
    let text = case.ours_agents_block(&cwd).unwrap();
    assert!(text.contains("ROOT") && !text.contains("SUB"));

    // The scan says why: the override takes the directory.
    let env = case.env(&cwd);
    let fs = ProbeFs::new();
    let scan = Codex.scan(&ScanCx {
        env: &env,
        fs: &fs,
        parked_dir: None,
    });
    let shadowed = scan
        .view
        .loads
        .iter()
        .find(|l| l.file.as_str().ends_with("sub/AGENTS.md"))
        .expect("the shadowed file is listed");
    assert!(matches!(
        &shadowed.status,
        LoadStatus::Skipped {
            skip: Skip::Shadowed {
                how: Shadow::EmptyOverride,
                ..
            }
        }
    ));
    assert!(
        scan.diagnostics
            .iter()
            .any(|d| d.problem == Problem::ShadowedByEmptyOverride)
    );
}

#[test]
fn oracle_global_override_wins_and_an_empty_one_falls_through() {
    if !codex_installed() {
        return;
    }
    let case = Case::new();
    let cwd = case.fx.project();
    case.user("AGENTS.override.md", "\n");
    case.user("AGENTS.md", "GLOBAL\n");
    same_instructions(&case, &cwd);
    assert!(case.ours_agents_block(&cwd).unwrap().contains("GLOBAL"));
    case.user("AGENTS.override.md", "GOVERRIDE\n");
    same_instructions(&case, &cwd);
    let text = case.ours_agents_block(&cwd).unwrap();
    assert!(text.contains("GOVERRIDE") && !text.contains("GLOBAL"));
}

#[test]
fn oracle_an_untrusted_project_skips_project_files_and_unknown_trust_loads_them() {
    if !codex_installed() {
        return;
    }
    let case = Case::new();
    case.user("AGENTS.md", "GLOBAL\n");
    case.proj("AGENTS.md", "ROOT\n");
    let cwd = case.fx.project();
    same_instructions(&case, &cwd);
    assert!(case.ours_agents_block(&cwd).unwrap().contains("ROOT"));

    case.trust("untrusted");
    same_instructions(&case, &cwd);
    let text = case.ours_agents_block(&cwd).unwrap();
    assert!(text.contains("GLOBAL") && !text.contains("ROOT"));
    let env = case.env(&cwd);
    let fs = ProbeFs::new();
    let scan = Codex.scan(&ScanCx {
        env: &env,
        fs: &fs,
        parked_dir: None,
    });
    assert_eq!(scan.view.trust.level, TrustLevel::Untrusted);
    assert!(scan.view.trust.recorded_in.is_some());
}

#[test]
fn oracle_the_byte_budget_cuts_the_crossing_file_and_drops_the_rest() {
    if !codex_installed() {
        return;
    }
    let case = Case::new();
    case.user("AGENTS.md", &format!("{}\n", "G".repeat(40)));
    case.proj("AGENTS.md", "0123456789ABCDEF\n");
    case.proj("sub/AGENTS.md", "SUBFILE\n");
    case.user("config.toml", "project_doc_max_bytes = 10\n");
    let cwd = case.fx.project_path("sub");
    same_instructions(&case, &cwd);
    let text = case.ours_agents_block(&cwd).unwrap();
    assert!(text.contains("0123456789\n") && !text.contains("ABCDEF"));

    let env = case.env(&cwd);
    let fs = ProbeFs::new();
    let scan = Codex.scan(&ScanCx {
        env: &env,
        fs: &fs,
        parked_dir: None,
    });
    let status = |suffix: &str| {
        scan.view
            .loads
            .iter()
            .find(|l| l.file.as_str().ends_with(suffix))
            .map(|l| l.status.clone())
    };
    assert!(matches!(
        status("project/AGENTS.md"),
        Some(LoadStatus::Truncated {
            kept: 10,
            total: 17,
            ..
        })
    ));
    assert!(matches!(
        status("sub/AGENTS.md"),
        Some(LoadStatus::Skipped {
            skip: Skip::BudgetExhausted { .. }
        })
    ));
}

#[test]
fn oracle_a_split_multibyte_character_becomes_the_replacement_character() {
    if !codex_installed() {
        return;
    }
    let case = Case::new();
    case.proj("AGENTS.md", "ab\u{e9}cd\n");
    case.user("config.toml", "project_doc_max_bytes = 3\n");
    same_instructions(&case, &case.fx.project());
}

#[test]
fn oracle_fallback_names_apply_to_the_project_and_not_to_the_global_file() {
    if !codex_installed() {
        return;
    }
    let case = Case::new();
    case.user(
        "config.toml",
        "project_doc_fallback_filenames = [\"TEAM.md\"]\n",
    );
    case.proj("TEAM.md", "TEAMROOT\n");
    case.proj("sub/TEAM.md", "TEAMSUB\n");
    case.proj("sub/AGENTS.md", "SUB\n");
    case.user("TEAM.md", "TEAMGLOBAL\n");
    let cwd = case.fx.project_path("sub");
    same_instructions(&case, &cwd);
    let text = case.ours_agents_block(&cwd).unwrap();
    assert!(text.contains("TEAMROOT") && text.contains("SUB") && !text.contains("TEAMSUB"));
    assert!(!text.contains("TEAMGLOBAL"));
}

#[test]
fn oracle_without_a_root_marker_only_cwd_counts() {
    if !codex_installed() {
        return;
    }
    let case = Case::new();
    std::fs::remove_dir_all(case.fx.project_path(".git").as_path()).unwrap();
    case.proj("AGENTS.md", "ROOT\n");
    case.proj("sub/AGENTS.md", "SUB\n");
    let cwd = case.fx.project_path("sub");
    same_instructions(&case, &cwd);
    let text = case.ours_agents_block(&cwd).unwrap();
    assert!(text.contains("SUB") && !text.contains("ROOT"));
}

#[test]
fn oracle_root_markers_come_from_the_user_layer_and_never_from_the_project() {
    if !codex_installed() {
        return;
    }
    let case = Case::new();
    case.proj("sub/.hg", "");
    case.proj("AGENTS.md", "ROOT\n");
    case.proj("sub/AGENTS.md", "SUB\n");
    case.proj(
        ".codex/config.toml",
        "project_root_markers = [\".hg\"]\ndeveloper_instructions = \"H-LAYER\"\n",
    );
    case.trust("trusted");
    let cwd = case.fx.project_path("sub/deep");
    std::fs::create_dir_all(cwd.as_path()).unwrap();
    same_instructions(&case, &cwd);
    let text = case.ours_agents_block(&cwd).unwrap();
    assert!(
        text.contains("ROOT") && text.contains("SUB"),
        "the project marker is ignored"
    );

    case.user(
        "config.toml",
        &format!(
            "project_root_markers = [\".hg\"]\n[projects.\"{}\"]\ntrust_level = \"trusted\"\n",
            case.fx.project()
        ),
    );
    same_instructions(&case, &cwd);
    let text = case.ours_agents_block(&cwd).unwrap();
    assert!(
        !text.contains("ROOT") && text.contains("SUB"),
        "the user marker is honored"
    );
}

#[test]
fn oracle_a_git_directory_without_head_is_a_root_for_agents_md() {
    if !codex_installed() {
        return;
    }
    let case = Case::new();
    std::fs::remove_file(case.fx.project_path(".git/HEAD").as_path()).unwrap();
    case.proj("AGENTS.md", "ROOT\n");
    case.proj("sub/AGENTS.md", "SUB\n");
    let cwd = case.fx.project_path("sub");
    same_instructions(&case, &cwd);
    assert!(case.ours_agents_block(&cwd).unwrap().contains("ROOT"));
}

#[test]
fn oracle_developer_and_base_instructions_come_first() {
    if !codex_installed() {
        return;
    }
    let case = Case::new();
    case.user("base.md", "CUSTOM-BASE\n");
    case.user(
        "config.toml",
        "developer_instructions = \"DEV-MARK\"\nmodel_instructions_file = \"base.md\"\n",
    );
    let cwd = case.fx.project();
    let oracle = case.oracle_texts(&cwd);
    assert_eq!(
        oracle[0],
        ("developer".to_owned(), "CUSTOM-BASE".to_owned())
    );
    assert_eq!(oracle[1], ("developer".to_owned(), "DEV-MARK".to_owned()));
    let ours = case.rendered(&cwd);
    assert_eq!(ours[0].delivery, Delivery::SystemPrompt);
    assert_eq!(ours[0].text, "CUSTOM-BASE");
    assert_eq!(ours[1].delivery, Delivery::DeveloperMessage);
    assert_eq!(ours[1].text, "DEV-MARK");
}

// ---------------------------------------------------------------------------------------------
// Skills (probe J)
// ---------------------------------------------------------------------------------------------

fn skill(name: &str) -> String {
    format!("---\nname: {name}\ndescription: {name} description\n---\nbody of {name}\n")
}

fn catalog_lines(block: &str) -> Vec<String> {
    let tail = block.split("### Available skills\n").nth(1).unwrap_or("");
    tail.lines()
        .filter(|l| l.starts_with("- "))
        .map(str::to_owned)
        .collect()
}

#[test]
fn oracle_skill_catalog_lines_across_three_roots() {
    if !codex_installed() {
        return;
    }
    let case = Case::new();
    case.proj(".agents/skills/rootskill/SKILL.md", &skill("rootskill"));
    case.proj("sub/.agents/skills/subskill/SKILL.md", &skill("subskill"));
    case.proj(
        "sub/deep/.agents/skills/deepskill/SKILL.md",
        &skill("deepskill"),
    );
    case.proj(
        ".codex/skills/dotcodexskill/SKILL.md",
        &skill("dotcodexskill"),
    );
    case.user("skills/codexhomeskill/SKILL.md", &skill("codexhomeskill"));
    case.write(
        &case.fx.home_path(".agents/skills/homeagentsskill/SKILL.md"),
        &skill("homeagentsskill"),
    );
    case.user("skills/nodesc/SKILL.md", "---\nname: nodesc\n---\nbody\n");
    case.user("skills/a/b/c/nested/SKILL.md", &skill("nested"));
    let cwd = case.fx.project_path("sub");

    // The oracle runs first: Codex installs its bundled `.system` skills into the home.
    let oracle = case.oracle_skills_block(&cwd).expect("codex lists skills");
    let ours = case.ours_skills_block(&cwd).expect("we list skills");
    assert_eq!(catalog_lines(&ours), catalog_lines(&oracle));
    assert_eq!(ours, oracle, "the whole developer message matches");
    let lines = catalog_lines(&ours);
    for name in [
        "rootskill",
        "subskill",
        "codexhomeskill",
        "homeagentsskill",
        "nested",
        "dotcodexskill",
    ] {
        assert!(
            lines.iter().any(|l| l.starts_with(&format!("- {name}:"))),
            "{name} is listed"
        );
    }
    assert!(
        !lines.iter().any(|l| l.starts_with("- deepskill:")),
        "below cwd is not scanned"
    );
    assert!(
        !lines.iter().any(|l| l.starts_with("- nodesc:")),
        "no description, rejected"
    );

    // Two loads per skill (design section 3).
    let env = case.env(&cwd);
    let fs = ProbeFs::new();
    let view = Codex
        .scan(&ScanCx {
            env: &env,
            fs: &fs,
            parked_dir: None,
        })
        .view;
    let root = view
        .extensions
        .iter()
        .find(|e| e.name == "rootskill")
        .expect("rootskill");
    let listing = &view.loads[root.listing.as_ref().unwrap().0 as usize];
    let body = &view.loads[root.body.0 as usize];
    assert_eq!(listing.kind, DocKind::Listing);
    assert_eq!(listing.when, Activation::SessionStart);
    assert_eq!(body.kind, DocKind::SkillBody);
    assert_eq!(body.when, Activation::OnInvoke);
    assert!(
        view.extensions
            .iter()
            .any(|e| e.name == "nodesc" && matches!(e.status, ExtStatus::Rejected { .. }))
    );
}

#[test]
fn oracle_skills_config_disables_a_skill_and_the_catalog_follows() {
    if !codex_installed() {
        return;
    }
    let case = Case::new();
    case.user("skills/keep/SKILL.md", &skill("keep"));
    case.user("skills/drop/SKILL.md", &skill("drop"));
    case.user(
        "config.toml",
        "[[skills.config]]\nname = \"drop\"\nenabled = false\n",
    );
    let cwd = case.fx.project();
    let oracle = case.oracle_skills_block(&cwd).unwrap();
    let ours = case.ours_skills_block(&cwd).unwrap();
    assert_eq!(ours, oracle);
    assert!(!ours.contains("- drop:"));
}

// ---------------------------------------------------------------------------------------------
// Hooks (probe_hooks.py)
// ---------------------------------------------------------------------------------------------

/// `hooks/list` over `codex app-server`.
fn hooks_list(case: &Case, cwd: &AbsPath) -> Vec<Value> {
    let mut child = Command::new("codex")
        .arg("app-server")
        .current_dir(cwd.as_path())
        .env("HOME", case.fx.home().as_str())
        .env("CODEX_HOME", case.fx.codex_home().as_str())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("start codex app-server");
    let mut stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let (tx, rx) = mpsc::channel::<Value>();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if let Ok(value) = serde_json::from_str::<Value>(&line) {
                let _ = tx.send(value);
            }
        }
    });
    let send = |stdin: &mut std::process::ChildStdin, value: Value| {
        writeln!(stdin, "{value}").unwrap();
        stdin.flush().unwrap();
    };
    send(
        &mut stdin,
        serde_json::json!({"jsonrpc":"2.0","id":1,"method":"initialize",
            "params":{"clientInfo":{"name":"cm-oracle","version":"0.0.0"}}}),
    );
    let wait = |id: i64| -> Value {
        loop {
            let msg = rx
                .recv_timeout(Duration::from_secs(60))
                .expect("app-server answers");
            if msg["id"] == id {
                return msg;
            }
        }
    };
    wait(1);
    send(
        &mut stdin,
        serde_json::json!({"jsonrpc":"2.0","method":"initialized"}),
    );
    send(
        &mut stdin,
        serde_json::json!({"jsonrpc":"2.0","id":2,"method":"hooks/list",
            "params":{"cwds":[cwd.as_str()]}}),
    );
    let response = wait(2);
    let _ = child.kill();
    let _ = child.wait();
    assert!(
        response.get("error").is_none(),
        "hooks/list failed: {response}"
    );
    response["result"]["data"][0]["hooks"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

#[derive(Debug, PartialEq)]
struct Row {
    enabled: bool,
    trust: String,
    hash: String,
    source: String,
}

fn trust_word(trust: TrustStatus) -> &'static str {
    match trust {
        TrustStatus::Managed => "managed",
        TrustStatus::Trusted => "trusted",
        TrustStatus::Modified => "modified",
        TrustStatus::Untrusted => "untrusted",
    }
}

fn oracle_rows(list: &[Value]) -> BTreeMap<String, Row> {
    list.iter()
        .map(|h| {
            (
                h["key"].as_str().unwrap().to_owned(),
                Row {
                    enabled: h["enabled"].as_bool().unwrap(),
                    trust: h["trustStatus"].as_str().unwrap().to_owned(),
                    hash: h["currentHash"].as_str().unwrap().to_owned(),
                    source: h["sourcePath"].as_str().unwrap().to_owned(),
                },
            )
        })
        .collect()
}

fn our_rows(case: &Case, cwd: &AbsPath) -> BTreeMap<String, Row> {
    let env = case.env(cwd);
    let fs = ProbeFs::new();
    inspect_hooks(&ScanCx {
        env: &env,
        fs: &fs,
        parked_dir: None,
    })
    .into_iter()
    .map(|h| {
        (
            h.key,
            Row {
                enabled: h.enabled,
                trust: trust_word(h.trust).to_owned(),
                hash: h.current_hash,
                source: h.source_path.to_string(),
            },
        )
    })
    .collect()
}

fn hooks_case() -> (Case, AbsPath, AbsPath) {
    let case = Case::new();
    case.user(
        "hooks.json",
        r#"{
  "hooks": {
    "PreToolUse": [
      {"matcher": "Bash", "hooks": [{"type": "command", "command": "echo user-json-pre", "timeout": 30}]}
    ],
    "Stop": [
      {"matcher": "ignored-for-stop", "hooks": [{"type": "command", "command": "echo user-json-stop"}]}
    ],
    "SessionEnd": [
      {"hooks": [{"type": "command", "command": "echo end", "timeout": 99}]}
    ],
    "PostToolUse": [
      {"matcher": "Edit|Write", "hooks": [
        {"type": "command", "command": "echo post", "statusMessage": "posting", "async": true,
         "additionalContextLimit": 100},
        {"type": "mcp_tool", "server": "sec", "tool": "scan", "input": {"path": "${tool_input.path}", "n": 3}}
      ]}
    ],
    "UserPromptSubmit": [
      {"hooks": [{"type": "prompt"}, {"type": "command", "command": "echo submit", "additionalContextLimit": 2500}]}
    ],
    "NotAnEvent": [{"hooks": []}]
  }
}"#,
    );
    case.user(
        "config.toml",
        r#"[[hooks.SessionStart]]
matcher = "startup|resume"
[[hooks.SessionStart.hooks]]
type = "command"
command = "echo user-toml-start"
statusMessage = "starting"

[[hooks.PreCompact]]
matcher = "^man"
[[hooks.PreCompact.hooks]]
type = "command"
command = "echo regex"
"#,
    );
    let trusted = case.fx.project();
    case.proj(
        ".codex/hooks.json",
        r#"{"hooks":{"PostToolUse":[{"matcher":"Edit|Write","hooks":[{"type":"command","command":"echo trusted-project-json"}]}]}}"#,
    );
    case.proj(
        ".codex/config.toml",
        "[[hooks.PreCompact]]\nmatcher = \"manual\"\n[[hooks.PreCompact.hooks]]\ntype = \"command\"\ncommand = \"echo trusted-project-toml\"\n",
    );
    case.trust("trusted");
    let cwd = trusted.clone();
    (case, trusted, cwd)
}

#[test]
fn oracle_hooks_match_hooks_list_for_json_toml_and_trusted_project() {
    if !codex_installed() {
        return;
    }
    let (case, _project, cwd) = hooks_case();
    let oracle = oracle_rows(&hooks_list(&case, &cwd));
    assert!(
        oracle.len() >= 8,
        "the fixture should list many hooks: {oracle:#?}"
    );
    assert!(
        oracle
            .keys()
            .any(|k| k.contains("/project/.codex/hooks.json")),
        "the trusted project's hooks are listed"
    );
    assert_eq!(
        our_rows(&case, &cwd),
        oracle,
        "keys, enabled flags, trust and hashes"
    );
}

#[test]
fn oracle_hooks_with_state_trusted_modified_and_disabled() {
    if !codex_installed() {
        return;
    }
    let (case, _project, cwd) = hooks_case();
    let first = oracle_rows(&hooks_list(&case, &cwd));
    let mut keys = first.keys();
    let trusted_key = keys.next().unwrap().clone();
    let modified_key = keys.next().unwrap().clone();
    let disabled_key = keys.next().unwrap().clone();
    let trusted_hash = first[&trusted_key].hash.clone();
    case.append_user_config(&format!(
        "\n[hooks.state.\"{trusted_key}\"]\ntrusted_hash = \"{trusted_hash}\"\n\
         [hooks.state.\"{modified_key}\"]\ntrusted_hash = \"sha256:0000\"\n\
         [hooks.state.\"{disabled_key}\"]\nenabled = false\ntrusted_hash = \"{}\"\n",
        first[&disabled_key].hash
    ));
    let oracle = oracle_rows(&hooks_list(&case, &cwd));
    assert_eq!(oracle[&trusted_key].trust, "trusted");
    assert_eq!(oracle[&modified_key].trust, "modified");
    assert!(!oracle[&disabled_key].enabled);
    assert_eq!(our_rows(&case, &cwd), oracle);

    // The scan's hook list agrees with the inspection.
    let env = case.env(&cwd);
    let fs = ProbeFs::new();
    let view = Codex
        .scan(&ScanCx {
            env: &env,
            fs: &fs,
            parked_dir: None,
        })
        .view;
    let active = view
        .hooks
        .iter()
        .filter(|h| h.state == HookState::Active)
        .count();
    assert_eq!(active, 1, "only the trusted hook runs");
    assert!(view.hooks.iter().any(|h| matches!(
        h.state,
        HookState::WillNotRun {
            why: HookBlock::Modified
        }
    )));
    assert!(view.hooks.iter().any(|h| matches!(
        h.state,
        HookState::Disabled {
            by: DisabledBy::Native { .. }
        }
    )));
    assert!(
        view.hooks.iter().any(|h| h.handler.handler_kind == "prompt"
            && matches!(
                h.state,
                HookState::WillNotRun {
                    why: HookBlock::UnsupportedType
                }
            )),
        "a prompt hook is shown and will not run"
    );
}

#[test]
fn oracle_project_hooks_of_an_unknown_project_are_not_listed() {
    if !codex_installed() {
        return;
    }
    let case = Case::new();
    case.proj(
        ".codex/hooks.json",
        r#"{"hooks":{"Stop":[{"hooks":[{"type":"command","command":"echo p"}]}]}}"#,
    );
    let cwd = case.fx.project();
    let oracle = oracle_rows(&hooks_list(&case, &cwd));
    assert!(oracle.is_empty());
    assert!(our_rows(&case, &cwd).is_empty());
    let env = case.env(&cwd);
    let fs = ProbeFs::new();
    let view = Codex
        .scan(&ScanCx {
            env: &env,
            fs: &fs,
            parked_dir: None,
        })
        .view;
    assert_eq!(view.hooks.len(), 1);
    assert!(matches!(
        view.hooks[0].state,
        HookState::WillNotRun {
            why: HookBlock::FolderUntrusted
        }
    ));
}

#[test]
fn oracle_plugin_hooks_carry_the_content_hash_and_the_two_part_approval() {
    if !codex_installed() {
        return;
    }
    let case = Case::new();
    let base = "plugins/cache/mkt/demo/1.0.0";
    case.user(
        &format!("{base}/.codex-plugin/plugin.json"),
        "{\"name\":\"demo\"}",
    );
    case.user(
        &format!("{base}/hooks/hooks.json"),
        r#"{"hooks":{"PreToolUse":[{"matcher":"Bash","hooks":[{"type":"command","command":"${PLUGIN_ROOT}/run.sh"}]}]}}"#,
    );
    case.user(&format!("{base}/run.sh"), "#!/bin/sh\necho hi\n");
    case.user(&format!("{base}/scripts/helper.py"), "print('x')\n");
    case.user("config.toml", "[plugins.\"demo@mkt\"]\nenabled = true\n");
    let cwd = case.fx.project();

    let first = oracle_rows(&hooks_list(&case, &cwd));
    assert_eq!(first.len(), 1, "{first:#?}");
    let (key, row) = first.iter().next().unwrap();
    assert!(
        key.starts_with("demo@mkt:hooks/hooks.json:pre_tool_use:0:0"),
        "{key}"
    );
    assert!(
        row.hash.contains('/'),
        "declaration hash, a slash, content hash: {}",
        row.hash
    );
    assert_eq!(our_rows(&case, &cwd), first);

    // Approve both halves, then change a script: the content hash no longer matches.
    let (declaration, content) = row.hash.split_once('/').unwrap();
    case.append_user_config(&format!(
        "[hooks.state.\"{key}\"]\ntrusted_hash = \"{declaration}\"\ntrusted_content_hash = \"{content}\"\n"
    ));
    let trusted = oracle_rows(&hooks_list(&case, &cwd));
    assert_eq!(trusted[key].trust, "trusted");
    assert_eq!(our_rows(&case, &cwd), trusted);

    case.user(&format!("{base}/scripts/helper.py"), "print('changed')\n");
    let modified = oracle_rows(&hooks_list(&case, &cwd));
    assert_eq!(modified[key].trust, "modified");
    assert_eq!(our_rows(&case, &cwd), modified);
}

// --- planners against hooks/list ----------------------------------------------------------

struct NoHistory;

impl cm_core::edit::ChangeSource for NoHistory {
    fn detail(&self, _: &ChangeId) -> Result<ChangeDetail, ApiError> {
        Err(ApiError::NotFound)
    }
}

/// Plans `edit` against the case as the host would, then writes the operations.
fn plan_and_apply(case: &Case, cwd: &AbsPath, edit: &Edit) -> Preview {
    let env = case.env(cwd);
    let scanned = cm_core::scan::scan_with(&[&Codex as &dyn Agent], &env);
    let parked = case.fx.root().join("data/parked");
    let preview = cm_core::edit::Planner {
        agents: &[&Codex as &dyn Agent],
        scanned: &scanned,
        env: &env,
        parked_dir: &parked,
        changes: &NoHistory,
    }
    .plan(edit)
    .unwrap_or_else(|err| panic!("plan failed: {err}"));
    for op in &preview.change_set.ops {
        match &op.action {
            Action::Write { text } => case.write(&op.path, text),
            other => panic!("unexpected {other:?}"),
        }
    }
    preview
}

fn hook_by_command(case: &Case, cwd: &AbsPath, command: &str) -> Hook {
    let env = case.env(cwd);
    let fs = ProbeFs::new();
    Codex
        .scan(&ScanCx {
            env: &env,
            fs: &fs,
            parked_dir: None,
        })
        .view
        .hooks
        .into_iter()
        .find(|h| h.handler.summary == command)
        .unwrap_or_else(|| panic!("no hook {command}"))
}

/// Approves every hook the oracle lists, the way `/hooks` would, so trust is `trusted`.
fn trust_all(case: &Case, cwd: &AbsPath) -> BTreeMap<String, Row> {
    let rows = oracle_rows(&hooks_list(case, cwd));
    let mut state = String::new();
    for (key, row) in &rows {
        state.push_str(&format!(
            "\n[hooks.state.\"{key}\"]\ntrusted_hash = \"{}\"\n",
            row.hash
        ));
    }
    case.append_user_config(&state);
    oracle_rows(&hooks_list(case, cwd))
}

#[test]
fn oracle_a_switch_changes_enabled_and_leaves_trust_and_hash_alone() {
    if !codex_installed() {
        return;
    }
    let (case, _project, cwd) = hooks_case();
    let trusted = trust_all(&case, &cwd);
    let hook = hook_by_command(&case, &cwd, "echo user-json-pre");
    let key = trusted
        .iter()
        .find(|(k, _)| {
            k.ends_with("hooks.json:pre_tool_use:0:0")
                && k.contains(".codex/hooks.json")
                && !k.contains("/project/")
        })
        .map(|(k, _)| k.clone())
        .expect("the user hook is listed");
    assert!(trusted[&key].enabled && trusted[&key].trust == "trusted");

    let before_config =
        std::fs::read_to_string(case.fx.codex_home().join("config.toml").as_path()).unwrap();
    plan_and_apply(
        &case,
        &cwd,
        &Edit::SetHookEnabled {
            hook: hook.id.clone(),
            enabled: false,
        },
    );
    let off = oracle_rows(&hooks_list(&case, &cwd));
    assert!(!off[&key].enabled, "hooks/list reports the hook disabled");
    assert_eq!(
        off[&key].trust, trusted[&key].trust,
        "trust status unchanged"
    );
    assert_eq!(off[&key].hash, trusted[&key].hash, "current hash unchanged");
    for (other, row) in &off {
        if *other != key {
            assert_eq!(row, &trusted[other], "no other hook moves");
        }
    }
    assert_eq!(our_rows(&case, &cwd), off);

    plan_and_apply(
        &case,
        &cwd,
        &Edit::SetHookEnabled {
            hook: hook.id,
            enabled: true,
        },
    );
    assert_eq!(oracle_rows(&hooks_list(&case, &cwd)), trusted);
    let after_config =
        std::fs::read_to_string(case.fx.codex_home().join("config.toml").as_path()).unwrap();
    assert_eq!(
        after_config, before_config,
        "off then on restores config.toml"
    );
}

#[test]
fn oracle_an_update_makes_the_hook_modified_and_an_add_makes_it_untrusted() {
    if !codex_installed() {
        return;
    }
    let (case, _project, cwd) = hooks_case();
    let trusted = trust_all(&case, &cwd);
    let hook = hook_by_command(&case, &cwd, "echo user-json-pre");
    let preview = plan_and_apply(
        &case,
        &cwd,
        &Edit::UpdateHook {
            hook: hook.id,
            draft: HookDraft {
                event: "PreToolUse".into(),
                matcher: Some("Bash".into()),
                handler: serde_json::json!({"type": "command", "command": "echo updated", "timeout": 30}),
            },
        },
    );
    assert!(preview.warnings.contains(&Warning::HookTrustReset));
    let updated = oracle_rows(&hooks_list(&case, &cwd));
    assert_eq!(updated.len(), trusted.len());
    let changed: Vec<_> = updated.iter().filter(|(k, r)| trusted[*k] != **r).collect();
    assert_eq!(
        changed.len(),
        1,
        "only the updated hook differs: {changed:#?}"
    );
    assert_eq!(changed[0].1.trust, "modified");
    assert_eq!(our_rows(&case, &cwd), updated);

    // A new hook has no approval at all.
    plan_and_apply(
        &case,
        &cwd,
        &Edit::AddHook {
            agent: AgentId::parse("codex").unwrap(),
            file: case.fx.codex_home().join("config.toml"),
            draft: HookDraft {
                event: "Stop".into(),
                matcher: None,
                handler: serde_json::json!({"type": "command", "command": "echo added"}),
            },
        },
    );
    let added = oracle_rows(&hooks_list(&case, &cwd));
    assert_eq!(added.len(), updated.len() + 1);
    let new = added
        .iter()
        .find(|(k, _)| !updated.contains_key(*k))
        .unwrap();
    assert_eq!(new.1.trust, "untrusted");
    assert_eq!(our_rows(&case, &cwd), added);
}
