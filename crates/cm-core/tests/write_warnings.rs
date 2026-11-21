//! A warning about a file reaches every write of that file, a plain editor save included.
//! Each case saves one file through the core planner and reads the warnings of the preview.

use cm_core::agents::registry;
use cm_core::edit::{ChangeSource, Planner};
use cm_core::env::ScanEnv;
use cm_core::scan::scan_full;
use cm_core::testing::Fixture;
use cm_core::*;

struct NoHistory;
impl ChangeSource for NoHistory {
    fn detail(&self, _: &ChangeId) -> Result<ChangeDetail, ApiError> {
        Err(ApiError::NotFound)
    }
}

fn save_warnings(fx: &Fixture, env: &ScanEnv, path: &AbsPath) -> Vec<Warning> {
    let scanned = scan_full(env);
    let rev = match &scanned.snapshot.files[path].state {
        FileState::Present { rev, .. } => rev.clone(),
        other => panic!("{path} is not present: {other:?}"),
    };
    let parked = fx.root().join("data/parked");
    let text = format!("{}\n", std::fs::read_to_string(path.as_path()).unwrap());
    Planner {
        agents: registry(),
        scanned: &scanned,
        env,
        parked_dir: &parked,
        changes: &NoHistory,
    }
    .plan(&Edit::Save {
        path: path.clone(),
        base: Base::Rev { rev },
        text,
    })
    .unwrap()
    .warnings
}

fn also_writes(agent: &str) -> Warning {
    Warning::AgentAlsoWrites {
        agent: AgentId::parse(agent).unwrap(),
    }
}

#[test]
fn a_plain_save_of_the_user_codex_config_says_codex_also_writes_it() {
    let fx = Fixture::new();
    let config = fx.home_path(".codex/config.toml");
    fx.write(&config, "model = \"gpt-5\"\n");
    let warnings = save_warnings(&fx, &fx.env(), &config);
    assert!(warnings.contains(&also_writes("codex")), "{warnings:?}");
}

#[test]
fn a_plain_save_of_claude_settings_local_json_says_claude_also_writes_it() {
    let fx = Fixture::new();
    fx.mkdir(&fx.project_path(".git"));
    let local = fx.project_path(".claude/settings.local.json");
    fx.write(&local, "{\"permissions\": {}}\n");
    let warnings = save_warnings(&fx, &fx.env(), &local);
    assert!(warnings.contains(&also_writes("claude")), "{warnings:?}");
}

#[test]
fn a_plain_save_of_gemini_settings_and_of_the_private_memory_index_says_gemini_also_writes_them() {
    let fx = Fixture::new();
    let settings = fx.home_path(".gemini/settings.json");
    fx.write(&settings, "{}\n");
    let index = fx.home_path(".gemini/tmp/proj/memory/MEMORY.md");
    fx.write(&index, "- [Note](note.md) - hook\n");
    fx.write(
        &fx.home_path(".gemini/projects.json"),
        &serde_json::json!({"projects": {fx.project().as_str(): "proj"}}).to_string(),
    );
    let env = fx.env();
    for path in [&settings, &index] {
        let warnings = save_warnings(&fx, &env, path);
        assert!(
            warnings.contains(&also_writes("gemini")),
            "{path}: {warnings:?}"
        );
    }
}

#[test]
fn a_file_the_agent_does_not_rewrite_gets_no_such_warning() {
    let fx = Fixture::new();
    fx.mkdir(&fx.project_path(".git"));
    let settings = fx.project_path(".claude/settings.json");
    fx.write(&settings, "{}\n");
    let project_config = fx.project_path(".codex/config.toml");
    fx.write(&project_config, "model = \"gpt-5\"\n");
    for path in [&settings, &project_config] {
        let warnings = save_warnings(&fx, &fx.env(), path);
        assert!(
            !warnings
                .iter()
                .any(|w| matches!(w, Warning::AgentAlsoWrites { .. })),
            "{path}: {warnings:?}"
        );
    }
}
