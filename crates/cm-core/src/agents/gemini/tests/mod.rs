//! Fixture tests for the Gemini CLI adapter. Each test is named after the rule of
//! `docs/agents/gemini-cli.md` that it checks, and its expected value comes from that spec.

use super::Gemini;
use crate::agents::{Agent, AgentScan, ScanCx};
use crate::env::ScanEnv;
use crate::model::*;
use crate::probe_fs::ProbeFs;
use crate::testing::Fixture;

mod context;
mod exts;
mod hooks;
mod memory;
mod misc;
mod render;
mod trust;

/// An environment in which the folder is trusted, so project files load.
pub(super) fn trusted(fx: &Fixture) -> ScanEnv {
    fx.env().with_var("GEMINI_CLI_TRUST_WORKSPACE", "true")
}

pub(super) fn scan(env: &ScanEnv) -> AgentScan {
    let fs = ProbeFs::new();
    Gemini.scan(&ScanCx {
        env,
        fs: &fs,
        parked_dir: None,
    })
}

pub(super) fn render(env: &ScanEnv) -> Vec<RenderedBlock> {
    let fs = ProbeFs::new();
    let cx = ScanCx {
        env,
        fs: &fs,
        parked_dir: None,
    };
    let scanned = Gemini.scan(&cx);
    Gemini.render(&cx, &scanned.view)
}

pub(super) fn on_access(env: &ScanEnv, dir: &AbsPath) -> Vec<crate::agents::Candidate> {
    let fs = ProbeFs::new();
    Gemini.on_access(
        &ScanCx {
            env,
            fs: &fs,
            parked_dir: None,
        },
        dir,
    )
}

pub(super) fn git(fx: &Fixture, dir: &AbsPath) {
    fx.mkdir(&dir.join(".git"));
}

pub(super) fn user_settings(fx: &Fixture, text: &str) {
    fx.write(&fx.home_path(".gemini/settings.json"), text);
}

pub(super) fn find<'a>(scan: &'a AgentScan, file: &AbsPath) -> Vec<&'a Load> {
    scan.view.loads.iter().filter(|l| l.file == *file).collect()
}

pub(super) fn only<'a>(scan: &'a AgentScan, file: &AbsPath) -> &'a Load {
    let loads = find(scan, file);
    assert_eq!(loads.len(), 1, "loads of {file}: {loads:?}");
    loads[0]
}

pub(super) fn discovered(scan: &AgentScan, rule: &str) -> Vec<String> {
    scan.view
        .loads
        .iter()
        .filter(|l| matches!(&l.via, Via::Discovery { rule: r } if r.0 == rule))
        .filter(|l| l.status == LoadStatus::Loaded)
        .map(|l| l.file.as_str().to_owned())
        .collect()
}

pub(super) fn skip(load: &Load) -> &Skip {
    match &load.status {
        LoadStatus::Skipped { skip } => skip,
        other => panic!("expected a skipped load, got {other:?}"),
    }
}

pub(super) fn problems(scan: &AgentScan) -> Vec<&Problem> {
    scan.diagnostics.iter().map(|d| &d.problem).collect()
}
