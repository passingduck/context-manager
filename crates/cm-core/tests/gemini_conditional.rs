//! Gemini's conditional loading rule has one implementation, `on_access`. What the scan lists as
//! an on-access load of a directory equals what the simulator returns for a path in it.

use std::collections::BTreeSet;

use cm_core::agents::{Agent, ScanCx, gemini::Gemini};
use cm_core::env::ScanEnv;
use cm_core::scan::scan_full;
use cm_core::simulate::simulate;
use cm_core::testing::Fixture;
use cm_core::walk::walk;
use cm_core::*;

fn gemini_view(snapshot: &Snapshot) -> &AgentView {
    let id = Gemini.id();
    snapshot.agents.iter().find(|v| v.agent == id).unwrap()
}

/// For every directory below cwd and every directory under the home directory's `.gemini`, the
/// files the simulator adds for a path in it, and the files `on_access` names for it.
fn assert_scan_and_simulator_agree(env: &ScanEnv) -> BTreeSet<AbsPath> {
    let scanned = scan_full(env);
    let cx = ScanCx {
        env,
        fs: scanned.fs(),
        parked_dir: None,
    };
    let mut dirs: Vec<AbsPath> = walk(&env.cwd, scanned.fs()).dirs().cloned().collect();
    dirs.push(env.home.join(".gemini"));
    let mut all = BTreeSet::new();
    for dir in dirs {
        let target = dir.join("probe.rs");
        let sim = simulate(cm_core::agents::registry(), &cx, &scanned.snapshot, &target)
            .into_iter()
            .find(|s| s.agent == Gemini.id())
            .expect("gemini is simulated");
        let view = gemini_view(&scanned.snapshot);
        let mut simulated: BTreeSet<AbsPath> = sim
            .fired
            .iter()
            .map(|id| view.loads[id.0 as usize].file.clone())
            .collect();
        simulated.extend(sim.extra.iter().map(|load| load.file.clone()));
        let asked: BTreeSet<AbsPath> = Gemini
            .on_access(&cx, &dir)
            .into_iter()
            .map(|candidate| candidate.file)
            .collect();
        assert_eq!(simulated, asked, "directory {dir}");
        all.extend(asked);
    }
    let listed: BTreeSet<AbsPath> = gemini_view(&scanned.snapshot)
        .loads
        .iter()
        .filter(|load| matches!(load.when, Activation::OnAccess { .. }))
        .map(|load| load.file.clone())
        .collect();
    assert_eq!(listed, all, "the scan lists what on_access names");
    all
}

fn tree(fx: &Fixture) {
    fx.mkdir(&fx.project_path(".git"));
    fx.write(&fx.project_path("GEMINI.md"), "# root\n");
    fx.write(&fx.project_path("sub/GEMINI.md"), "# sub\n");
    fx.write(&fx.project_path("sub/deep/GEMINI.md"), "# deep\n");
}

#[test]
fn in_a_trusted_folder_the_scan_lists_the_conditional_loads_the_simulator_adds() {
    let fx = Fixture::new();
    tree(&fx);
    fx.write(
        &fx.home_path(".gemini/trustedFolders.json"),
        &serde_json::json!({fx.project().as_str(): "TRUST_FOLDER"}).to_string(),
    );
    let found = assert_scan_and_simulator_agree(&fx.env());
    assert_eq!(found.len(), 2, "{found:?}");
}

#[test]
fn in_an_untrusted_folder_neither_lists_a_conditional_load() {
    let fx = Fixture::new();
    tree(&fx);
    fx.write(
        &fx.home_path(".gemini/trustedFolders.json"),
        &serde_json::json!({fx.project().as_str(): "DO_NOT_TRUST"}).to_string(),
    );
    assert!(assert_scan_and_simulator_agree(&fx.env()).is_empty());
}

#[test]
fn a_directory_under_the_global_directory_is_treated_the_same_by_both() {
    // The workspace is the home directory, so the walk enters `.gemini`: its own files are the
    // global ones, and a directory below it holds a context file of its own.
    let fx = Fixture::new();
    fx.mkdir(&fx.home_path(".git"));
    fx.write(&fx.home_path(".gemini/GEMINI.md"), "# global\n");
    fx.write(&fx.home_path(".gemini/CONTEXT.md"), "# other name\n");
    fx.write(&fx.home_path(".gemini/inner/GEMINI.md"), "# inner\n");
    fx.write(
        &fx.home_path(".gemini/settings.json"),
        r#"{"security":{"folderTrust":{"enabled":false}},"context":{"fileName":["GEMINI.md","CONTEXT.md"]}}"#,
    );
    fx.write(&fx.home_path("work/GEMINI.md"), "# work\n");
    let env = fx.env().with_cwd(fx.home());
    assert_scan_and_simulator_agree(&env);
}
