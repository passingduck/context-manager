//! The Claude planners end to end through the host: plan, apply, history and revert, on a
//! fixture home with the real adapters. Nothing here touches a real agent home or data directory.

use std::path::Path;
use std::time::Duration;

use cm_core::agents::registry;
use cm_core::testing::Fixture;
use cm_core::*;

use crate::apply::Trasher;
use crate::host::{Host, HostConfig};

struct RemovingTrash;

impl Trasher for RemovingTrash {
    fn trash(&self, path: &Path) -> Result<(), String> {
        std::fs::remove_file(path).map_err(|e| e.to_string())
    }
}

fn host(fx: &Fixture) -> Host {
    // The managed directory is moved into the fixture so that no real policy is read.
    let env = fx.env().with_var(
        "CONTEXT_MANAGER_MANAGED_DIR",
        fx.root().join("managed").as_str(),
    );
    Host::with_parts(
        HostConfig {
            shell: Shell::Serve,
            read_only: false,
            env,
            watch_timeout: Duration::from_secs(1),
            data_dir: fx.root().join("data"),
        },
        registry(),
        Box::new(RemovingTrash),
    )
}

async fn run(host: &Host, fx: &Fixture, edit: Edit) -> (Preview, ChangeId) {
    let preview = host
        .plan(PlanReq {
            cwd: fx.project(),
            edit: edit.clone(),
        })
        .await
        .unwrap();
    let result = host
        .apply(ApplyReq {
            cwd: fx.project(),
            edit,
            digest: preview.digest.clone(),
        })
        .await
        .unwrap();
    match result {
        ApplyResult::Applied { change, .. } => (preview, change),
        other => panic!("expected Applied, got {other:?}"),
    }
}

async fn claude_hooks(host: &Host, fx: &Fixture) -> Vec<Hook> {
    let snapshot = host.scan(ScanReq { cwd: fx.project() }).await.unwrap();
    snapshot
        .agents
        .into_iter()
        .find(|v| v.agent == AgentId::parse("claude").unwrap())
        .unwrap()
        .hooks
}

fn read(path: &AbsPath) -> String {
    std::fs::read_to_string(path.as_path()).unwrap()
}

#[tokio::test]
async fn a_hook_add_and_a_switch_are_recorded_and_a_revert_restores_the_starting_bytes() {
    let fx = Fixture::new();
    fx.mkdir(&fx.project_path(".git"));
    let settings = fx.home_path(".claude/settings.json");
    let start = "{\n    \"model\": \"opus\",\n    \"hooks\": {\n        \"Stop\": [\n            {\n                \"hooks\": [\n                    {\n                        \"type\": \"command\",\n                        \"command\": \"keep.sh\"\n                    }\n                ]\n            }\n        ]\n    }\n}\n";
    fx.write(&settings, start);
    let host = host(&fx);

    // Add a hook.
    let add = Edit::AddHook {
        agent: AgentId::parse("claude").unwrap(),
        file: settings.clone(),
        draft: HookDraft {
            event: "PreToolUse".into(),
            matcher: Some("Bash".into()),
            handler: serde_json::json!({"type": "command", "command": "added.sh", "timeout": 3}),
        },
    };
    let (preview, add_change) = run(&host, &fx, add).await;
    assert_eq!(preview.change_set.ops.len(), 1);
    let added = read(&settings);
    let untouched = &start[..start.find("        ]").unwrap()];
    assert!(
        added.starts_with(untouched),
        "the bytes before the entry stay"
    );
    let hook = claude_hooks(&host, &fx)
        .await
        .into_iter()
        .find(|h| h.handler.summary == "added.sh")
        .unwrap();
    assert_eq!(hook.state, HookState::Active);

    // Switch it off: the handler leaves the file and a record appears in the data directory.
    let (off, off_change) = run(
        &host,
        &fx,
        Edit::SetHookEnabled {
            hook: hook.id.clone(),
            enabled: false,
        },
    )
    .await;
    assert!(off.warnings.contains(&Warning::ParkedOnThisMachine));
    assert!(!read(&settings).contains("added.sh"));
    let record = fx.root().join("data/parked");
    assert_eq!(std::fs::read_dir(record.as_path()).unwrap().count(), 1);
    let parked = claude_hooks(&host, &fx)
        .await
        .into_iter()
        .find(|h| h.handler.summary == "added.sh")
        .unwrap();
    assert_eq!(
        parked.state,
        HookState::Disabled {
            by: DisabledBy::Parked
        }
    );

    // History holds both changes, newest first.
    let records = host
        .history(HistoryReq {
            cwd: None,
            before: None,
            limit: 10,
        })
        .await
        .unwrap();
    let edits: Vec<ChangeEdit> = records.iter().map(|r| r.summary.edit.clone()).collect();
    assert_eq!(edits, [ChangeEdit::SetHookEnabled, ChangeEdit::AddHook]);
    assert!(records.iter().all(|r| r.state == ChangeState::Sealed));
    assert_eq!(records[0].id, off_change);
    assert_eq!(records[1].id, add_change);

    // Reverting the switch puts the handler back and removes the record; reverting the add
    // restores the starting bytes.
    run(&host, &fx, Edit::Revert { change: off_change }).await;
    assert_eq!(read(&settings), added);
    assert_eq!(std::fs::read_dir(record.as_path()).unwrap().count(), 0);
    run(&host, &fx, Edit::Revert { change: add_change }).await;
    assert_eq!(read(&settings), start);
}
