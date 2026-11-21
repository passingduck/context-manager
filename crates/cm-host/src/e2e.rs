//! What the per-agent end-to-end tests share: a host over a fixture, a plan then an apply, the
//! hooks of one agent after a scan, and the history. Nothing here touches a real agent home, the
//! real data directory or the real OS trash.

use std::path::Path;
use std::time::Duration;

use cm_core::agents::Agent;
use cm_core::env::ScanEnv;
use cm_core::testing::Fixture;
use cm_core::*;

use crate::apply::Trasher;
use crate::host::{Host, HostConfig};

/// Removes the path, so a test sends nothing to the real OS trash.
pub(crate) struct FileRemover;

impl Trasher for FileRemover {
    fn trash(&self, path: &Path) -> Result<(), String> {
        std::fs::remove_file(path).map_err(|e| e.to_string())
    }
}

/// A host over `agents`, reading `env` and keeping its data in the fixture.
pub(crate) fn host(fx: &Fixture, env: ScanEnv, agents: &'static [&'static dyn Agent]) -> Host {
    Host::with_parts(
        HostConfig {
            shell: Shell::Serve,
            read_only: false,
            env,
            watch_timeout: Duration::from_secs(1),
            data_dir: fx.root().join("data"),
        },
        agents,
        Box::new(FileRemover),
    )
}

/// Plans, then applies with the digest the plan returned. The plan must write something.
pub(crate) async fn run(host: &Host, fx: &Fixture, edit: Edit) -> (Preview, ChangeId) {
    let preview = host
        .plan(PlanReq {
            cwd: fx.project(),
            edit: edit.clone(),
        })
        .await
        .unwrap();
    assert!(
        !preview.change_set.ops.is_empty(),
        "{edit:?} plans to nothing"
    );
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

/// The hooks one agent lists after a fresh scan.
pub(crate) async fn hooks_of(host: &Host, fx: &Fixture, agent: &str) -> Vec<Hook> {
    let snapshot = host.scan(ScanReq { cwd: fx.project() }).await.unwrap();
    snapshot
        .agents
        .into_iter()
        .find(|view| view.agent.as_str() == agent)
        .unwrap()
        .hooks
}

/// The recorded changes, newest first.
pub(crate) async fn history(host: &Host) -> Vec<ChangeRecord> {
    host.history(HistoryReq {
        cwd: None,
        before: None,
        limit: 10,
    })
    .await
    .unwrap()
}

pub(crate) fn read(path: &AbsPath) -> String {
    std::fs::read_to_string(path.as_path()).unwrap()
}
