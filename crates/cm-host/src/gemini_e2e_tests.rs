//! Gemini CLI hook edits end to end through the host: plan, apply, history, revert.

use std::path::Path;
use std::time::Duration;

use cm_core::agents::Agent;
use cm_core::agents::gemini::Gemini;
use cm_core::testing::Fixture;
use cm_core::*;
use serde_json::json;

use crate::apply::Trasher;
use crate::host::{Host, HostConfig};

/// Removes the path, so the test sends nothing to the real OS trash.
struct RemovingTrash;

impl Trasher for RemovingTrash {
    fn trash(&self, path: &Path) -> Result<(), String> {
        std::fs::remove_file(path).map_err(|e| e.to_string())
    }
}

const START: &str = "// mine\n{\n\t\"ui\": { \"theme\": \"dark\" }, // keep\n\t\"hooks\": {\n\t\t\"BeforeTool\": [ { \"hooks\": [ { \"type\": \"command\", \"command\": \"keep.sh\" } ] } ]\n\t}\n}\n";

fn host(fx: &Fixture) -> Host {
    let agents: &'static [&'static dyn Agent] = Box::leak(Box::new([&Gemini as &dyn Agent]));
    Host::with_parts(
        HostConfig {
            shell: Shell::Serve,
            read_only: false,
            env: fx.env().with_var("GEMINI_CLI_TRUST_WORKSPACE", "true"),
            watch_timeout: Duration::from_secs(1),
            data_dir: fx.root().join("data"),
        },
        agents,
        Box::new(RemovingTrash),
    )
}

async fn run(host: &Host, fx: &Fixture, edit: &Edit) -> ChangeId {
    let preview = host
        .plan(PlanReq {
            cwd: fx.project(),
            edit: edit.clone(),
        })
        .await
        .unwrap();
    assert!(!preview.change_set.ops.is_empty());
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
        other => panic!("{other:?}"),
    }
}

async fn hooks(host: &Host, fx: &Fixture) -> Vec<Hook> {
    let snapshot = host.scan(ScanReq { cwd: fx.project() }).await.unwrap();
    snapshot
        .agents
        .into_iter()
        .find(|v| v.agent.as_str() == "gemini")
        .unwrap()
        .hooks
}

#[tokio::test]
async fn a_hook_add_and_a_switch_are_recorded_and_revert_restores_the_bytes() {
    let fx = Fixture::new();
    let settings = fx.home_path(".gemini/settings.json");
    fx.write(&settings, START);
    let host = host(&fx);

    let add = Edit::AddHook {
        agent: Gemini.id(),
        file: settings.clone(),
        draft: HookDraft {
            event: "AfterTool".into(),
            matcher: None,
            handler: json!({"type": "command", "command": "new.sh", "name": "new"}),
        },
    };
    let added = run(&host, &fx, &add).await;
    let after_add = std::fs::read_to_string(settings.as_path()).unwrap();
    assert_ne!(after_add, START);

    let id = hooks(&host, &fx)
        .await
        .into_iter()
        .find(|h| h.handler.summary == "new.sh")
        .unwrap()
        .id;
    let off = Edit::SetHookEnabled {
        hook: id.clone(),
        enabled: false,
    };
    let switched = run(&host, &fx, &off).await;
    assert!(
        std::fs::read_to_string(settings.as_path())
            .unwrap()
            .contains("\"disabled\"")
    );
    let state = hooks(&host, &fx)
        .await
        .into_iter()
        .find(|h| h.id == id)
        .unwrap()
        .state;
    assert!(matches!(
        state,
        HookState::Disabled {
            by: DisabledBy::Native { .. }
        }
    ));

    let records = host
        .history(HistoryReq {
            cwd: None,
            before: None,
            limit: 10,
        })
        .await
        .unwrap();
    let kinds: Vec<_> = records.iter().map(|r| r.summary.edit.clone()).collect();
    assert_eq!(kinds, [ChangeEdit::SetHookEnabled, ChangeEdit::AddHook]);

    // Revert the switch, then the add: the starting bytes come back.
    run(&host, &fx, &Edit::Revert { change: switched }).await;
    assert_eq!(
        std::fs::read_to_string(settings.as_path()).unwrap(),
        after_add
    );
    run(&host, &fx, &Edit::Revert { change: added }).await;
    assert_eq!(std::fs::read_to_string(settings.as_path()).unwrap(), START);
}

/// Gemini's subdirectory context slot names a file in a directory of the project, not an item
/// that is a directory. A trash of `src/GEMINI.md` trashes that file alone, and the rest of
/// `src/` stays byte for byte.
#[tokio::test]
async fn trashing_a_subdirectory_context_file_leaves_the_rest_of_its_directory() {
    let fx = Fixture::new();
    let context = fx.project_path("src/GEMINI.md");
    let main = fx.project_path("src/main.rs");
    let nested = fx.project_path("src/lib/x.rs");
    fx.write(&context, "src context\n");
    fx.write(&main, "fn main() {}\n");
    fx.write(&nested, "pub fn x() {}\n");
    let host = host(&fx);
    let edit = Edit::Trash {
        path: context.clone(),
        base: Revision::of(b"src context\n"),
    };

    let preview = host
        .plan(PlanReq {
            cwd: fx.project(),
            edit: edit.clone(),
        })
        .await
        .unwrap();
    assert_eq!(
        preview
            .change_set
            .ops
            .iter()
            .map(|op| (op.path.clone(), op.action.clone()))
            .collect::<Vec<_>>(),
        vec![(context.clone(), Action::Trash)],
        "the plan does more than trash the context file"
    );
    run(&host, &fx, &edit).await;
    assert!(!context.as_path().exists());
    assert_eq!(std::fs::read(main.as_path()).unwrap(), b"fn main() {}\n");
    assert_eq!(std::fs::read(nested.as_path()).unwrap(), b"pub fn x() {}\n");
}
