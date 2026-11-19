//! Two wrong answers found by driving the product: a shared-file warning that named an agent
//! that does not read the file, and a missing-reference diagnostic for prose in a link.

use cm_core::agents::registry;
use cm_core::edit::{ChangeSource, Planner};
use cm_core::scan::{Scanned, scan_full, scan_with_parked};
use cm_core::testing::Fixture;
use cm_core::*;

struct NoHistory;
impl ChangeSource for NoHistory {
    fn detail(&self, _: &ChangeId) -> Result<ChangeDetail, ApiError> {
        Err(ApiError::NotFound)
    }
}

fn save_warnings(fx: &Fixture, path: &AbsPath, old: &str) -> Vec<Warning> {
    let env = fx.env();
    let parked = fx.root().join("data/parked");
    let scanned = scan_with_parked(registry(), &env, Some(&parked));
    Planner {
        agents: registry(),
        scanned: &scanned,
        env: &env,
        parked_dir: &parked,
        changes: &NoHistory,
    }
    .plan(&Edit::Save {
        path: path.clone(),
        base: Base::Rev {
            rev: Revision::of(old.as_bytes()),
        },
        text: format!("{old}more\n"),
    })
    .unwrap()
    .warnings
}

fn shared(warnings: &[Warning]) -> Option<Vec<String>> {
    warnings.iter().find_map(|w| match w {
        Warning::SharedFile { agents } => Some(agents.iter().map(|a| a.to_string()).collect()),
        _ => None,
    })
}

#[test]
fn a_claude_md_only_claude_loads_has_no_shared_file_warning() {
    let fx = Fixture::new();
    let file = fx.project_path("CLAUDE.md");
    fx.write(&file, "# Claude only\n");
    let warnings = save_warnings(&fx, &file, "# Claude only\n");
    assert_eq!(shared(&warnings), None, "{warnings:?}");
}

#[test]
fn an_agents_md_codex_loads_and_claude_imports_names_both() {
    let fx = Fixture::new();
    let file = fx.project_path("AGENTS.md");
    fx.write(&file, "# Shared\n");
    fx.write(&fx.project_path("CLAUDE.md"), "@AGENTS.md\n");
    let warnings = save_warnings(&fx, &file, "# Shared\n");
    let mut names = shared(&warnings).unwrap_or_else(|| panic!("no SharedFile in {warnings:?}"));
    names.sort();
    assert_eq!(names, ["claude", "codex"]);
}

fn missing(scanned: &Scanned) -> Vec<Diagnostic> {
    scanned
        .snapshot
        .diagnostics
        .iter()
        .filter(|d| matches!(d.problem, Problem::ReferenceMissing { .. }))
        .cloned()
        .collect()
}

#[test]
fn prose_in_a_link_target_is_not_a_missing_reference() {
    let fx = Fixture::new();
    fx.write(
        &fx.project_path("CLAUDE.md"),
        "See the [Team roster](its link), [gone](docs/gone.md) and [bare](gone.md).\n",
    );
    let found = missing(&scan_full(&fx.env()));
    let raws: Vec<_> = found
        .iter()
        .map(|d| match &d.problem {
            Problem::ReferenceMissing { raw } => raw.as_str(),
            _ => unreachable!(),
        })
        .collect();
    assert_eq!(raws, ["docs/gone.md", "gone.md"]);
    assert!(found.iter().all(|d| d.severity == Severity::Warning));
}

#[test]
fn a_missing_reference_in_a_read_only_file_is_info() {
    let fx = Fixture::new();
    let ext = fx.home_path(".gemini/extensions/team");
    fx.write(
        &ext.join("gemini-extension.json"),
        r#"{"name":"team","version":"1.0.0"}"#,
    );
    fx.write(&ext.join("GEMINI.md"), "See [gone](docs/gone.md).\n");
    let found = missing(&scan_full(&fx.env()));
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].severity, Severity::Info);
}
