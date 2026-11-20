//! The Claude planners end to end through the host: plan, apply, history and revert, on a
//! fixture home with the real adapters. Nothing here touches a real agent home or data directory.

use cm_core::agents::registry;
use cm_core::testing::Fixture;
use cm_core::*;

use crate::e2e::{history, hooks_of, read, run};
use crate::host::Host;

fn host(fx: &Fixture) -> Host {
    // The managed directory is moved into the fixture so that no real policy is read.
    let env = fx.env().with_var(
        "CONTEXT_MANAGER_MANAGED_DIR",
        fx.root().join("managed").as_str(),
    );
    crate::e2e::host(fx, env, registry())
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
    let hook = hooks_of(&host, &fx, "claude")
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
    let parked = hooks_of(&host, &fx, "claude")
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
    let records = history(&host).await;
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
