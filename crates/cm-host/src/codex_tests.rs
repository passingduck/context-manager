//! The Codex planners through the host: plan, apply, history and revert over a temp home.

use std::path::Path;
use std::time::Duration;

use cm_core::agents::Agent;
use cm_core::agents::codex::Codex;
use cm_core::testing::Fixture;
use cm_core::*;

use crate::apply::Trasher;
use crate::host::{Host, HostConfig};

struct NoTrash;

impl Trasher for NoTrash {
    fn trash(&self, _: &Path) -> Result<(), String> {
        Err("a Codex hook edit never trashes".into())
    }
}

const HOOKS_JSON: &str = "{\n  \"hooks\": {\n    \"Stop\": [\n      {\"hooks\": [{\"type\": \"command\", \"command\": \"echo stop\"}]}\n    ]\n  }\n}\n";
const CONFIG: &str = "# mine\nmodel = \"gpt-5\"   # keep\n";

fn host(fx: &Fixture) -> Host {
    let agent: &'static dyn Agent = &Codex;
    let agents: &'static [&'static dyn Agent] = Box::leak(Box::new([agent]));
    Host::with_parts(
        HostConfig {
            shell: Shell::Serve,
            read_only: false,
            env: env(fx),
            watch_timeout: Duration::from_secs(1),
            data_dir: fx.root().join("data"),
        },
        agents,
        Box::new(NoTrash),
    )
}

fn env(fx: &Fixture) -> cm_core::env::ScanEnv {
    fx.env()
        .with_var("CODEX_HOME", fx.home_path(".codex").as_str())
}

fn read(path: &AbsPath) -> String {
    std::fs::read_to_string(path.as_path()).unwrap()
}

async fn run(host: &Host, fx: &Fixture, edit: &Edit) -> ChangeId {
    let preview = host
        .plan(PlanReq {
            cwd: fx.project(),
            edit: edit.clone(),
        })
        .await
        .unwrap();
    match host
        .apply(ApplyReq {
            cwd: fx.project(),
            edit: edit.clone(),
            digest: preview.digest,
        })
        .await
        .unwrap()
    {
        ApplyResult::Applied { change, .. } => change,
        other => panic!("expected Applied, got {other:?}"),
    }
}

#[tokio::test]
async fn a_hook_add_and_a_switch_are_recorded_and_a_revert_restores_the_bytes() {
    let fx = Fixture::new();
    fx.write(&fx.project_path(".git/HEAD"), "ref: refs/heads/main\n");
    let hooks = fx.home_path(".codex/hooks.json");
    let config = fx.home_path(".codex/config.toml");
    fx.write(&hooks, HOOKS_JSON);
    fx.write(&config, CONFIG);
    let host = host(&fx);

    let add = Edit::AddHook {
        agent: AgentId::parse("codex").unwrap(),
        file: hooks.clone(),
        draft: HookDraft {
            event: "PreToolUse".into(),
            matcher: Some("Bash".into()),
            handler: serde_json::json!({"type": "command", "command": "echo added"}),
        },
    };
    let added = run(&host, &fx, &add).await;
    assert!(read(&hooks).contains("echo added") && read(&hooks).contains("echo stop"));

    let scanned = cm_core::scan::scan_with(&[&Codex as &dyn Agent], &env(&fx));
    let stop = scanned.snapshot.agents[0]
        .hooks
        .iter()
        .find(|h| h.handler.summary == "echo stop")
        .unwrap()
        .id
        .clone();
    let switch = Edit::SetHookEnabled {
        hook: stop,
        enabled: false,
    };
    let switched = run(&host, &fx, &switch).await;
    assert!(read(&config).contains("enabled = false"));
    assert!(read(&config).starts_with(CONFIG));

    let records = host
        .history(HistoryReq {
            cwd: None,
            before: None,
            limit: 10,
        })
        .await
        .unwrap();
    let kinds: Vec<_> = records
        .iter()
        .map(|r| (r.id.clone(), r.summary.edit.clone()))
        .collect();
    assert_eq!(
        kinds,
        vec![
            (switched.clone(), ChangeEdit::SetHookEnabled),
            (added.clone(), ChangeEdit::AddHook)
        ]
    );
    assert!(records.iter().all(|r| r.state == ChangeState::Sealed));

    // The same switch again plans to nothing.
    let again = host
        .plan(PlanReq {
            cwd: fx.project(),
            edit: switch.clone(),
        })
        .await
        .unwrap();
    assert!(again.change_set.ops.is_empty());

    run(&host, &fx, &Edit::Revert { change: switched }).await;
    assert_eq!(read(&config), CONFIG);
    run(&host, &fx, &Edit::Revert { change: added }).await;
    assert_eq!(read(&hooks), HOOKS_JSON);
}
