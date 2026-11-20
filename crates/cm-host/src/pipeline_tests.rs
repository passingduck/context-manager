//! The write pipeline end to end: plan, apply, history, recovery and revert, over temp
//! directories. Every test has its own `Fixture` and data directory.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cm_core::agents::Agent;
use cm_core::testing::{Fixture, TestAgent};
use cm_core::*;

use crate::apply::{OsTrash, Step, Trasher};
use crate::host::{Host, HostConfig};
use crate::recovery;

/// Records the paths it is asked to trash and removes them, as a trash would.
#[derive(Clone, Default)]
struct FakeTrash(Arc<Mutex<Vec<String>>>);

impl Trasher for FakeTrash {
    fn trash(&self, path: &Path) -> Result<(), String> {
        let result = if path.is_dir() {
            std::fs::remove_dir_all(path)
        } else {
            std::fs::remove_file(path)
        };
        result.map_err(|e| e.to_string())?;
        self.0.lock().unwrap().push(path.display().to_string());
        Ok(())
    }
}

/// Refuses every call, like a Linux host with no trash.
struct NoTrash;

impl Trasher for NoTrash {
    fn trash(&self, _: &Path) -> Result<(), String> {
        Err("no trash here".into())
    }
}

fn agents(agent: TestAgent) -> &'static [&'static dyn Agent] {
    let agent: &'static dyn Agent = Box::leak(Box::new(agent));
    Box::leak(Box::new([agent]))
}

fn host_with(fx: &Fixture, agent: TestAgent, read_only: bool, trasher: Box<dyn Trasher>) -> Host {
    Host::with_parts(
        HostConfig {
            shell: Shell::Serve,
            read_only,
            env: fx.env(),
            watch_timeout: Duration::from_secs(1),
            data_dir: fx.root().join("data"),
        },
        agents(agent),
        trasher,
    )
}

fn host(fx: &Fixture, agent: TestAgent) -> Host {
    host_with(fx, agent, false, Box::new(FakeTrash::default()))
}

fn claims(files: &[&AbsPath]) -> TestAgent {
    TestAgent {
        files: files.iter().map(|p| (*p).clone()).collect(),
        ..TestAgent::default()
    }
}

fn rev(text: &str) -> Base {
    Base::Rev {
        rev: Revision::of(text.as_bytes()),
    }
}

fn save(path: &AbsPath, base: &str, text: &str) -> Edit {
    Edit::Save {
        path: path.clone(),
        base: rev(base),
        text: text.into(),
    }
}

fn read(path: &AbsPath) -> String {
    std::fs::read_to_string(path.as_path()).unwrap()
}

async fn plan(host: &Host, fx: &Fixture, edit: &Edit) -> Result<Preview, ApiError> {
    host.plan(PlanReq {
        cwd: fx.project(),
        edit: edit.clone(),
    })
    .await
}

async fn apply(
    host: &Host,
    fx: &Fixture,
    edit: &Edit,
    digest: &str,
) -> Result<ApplyResult, ApiError> {
    host.apply(ApplyReq {
        cwd: fx.project(),
        edit: edit.clone(),
        digest: digest.to_owned(),
    })
    .await
}

/// Plans, then applies with the digest the plan returned.
async fn run(host: &Host, fx: &Fixture, edit: &Edit) -> Result<ApplyResult, ApiError> {
    let preview = plan(host, fx, edit).await?;
    apply(host, fx, edit, &preview.digest).await
}

async fn history(host: &Host) -> Vec<ChangeRecord> {
    host.history(HistoryReq {
        cwd: None,
        before: None,
        limit: 100,
    })
    .await
    .unwrap()
}

type Applied = (ChangeId, Vec<(AbsPath, Option<Revision>)>, Vec<AbsPath>);

fn applied(result: ApplyResult) -> Applied {
    match result {
        ApplyResult::Applied {
            change,
            files,
            not_trashed,
        } => (change, files, not_trashed),
        other => panic!("expected Applied, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------------------------
// 2. Save
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn a_save_is_planned_applied_once_and_recorded_with_its_text() {
    let fx = Fixture::new();
    let file = fx.project_path("CLAUDE.md");
    fx.write(&file, "old\n");
    let host = host(&fx, claims(&[&file]));
    let edit = save(&file, "old\n", "new\n");

    let preview = plan(&host, &fx, &edit).await.unwrap();
    assert_eq!(preview.before[&file].as_deref(), Some("old\n"));
    assert_eq!(
        preview.change_set.ops[0].action,
        Action::Write {
            text: "new\n".into()
        }
    );
    assert_eq!(read(&file), "old\n", "plan writes nothing");

    let (change, files, not_trashed) =
        applied(apply(&host, &fx, &edit, &preview.digest).await.unwrap());
    assert_eq!(read(&file), "new\n");
    assert_eq!(files, vec![(file.clone(), Some(Revision::of(b"new\n")))]);
    assert!(not_trashed.is_empty());

    assert_eq!(
        apply(&host, &fx, &edit, &preview.digest).await.unwrap(),
        ApplyResult::AlreadyApplied
    );

    let records = history(&host).await;
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].id, change);
    assert_eq!(records[0].state, ChangeState::Sealed);
    assert_eq!(records[0].cwd, Some(fx.project()));
    let detail = host.change(ChangeReq { id: change }).await.unwrap();
    assert_eq!(detail.files.len(), 1);
    assert_eq!(detail.files[0].before.as_deref(), Some("old\n"));
    assert_eq!(detail.files[0].after.as_deref(), Some("new\n"));
    assert!(host.session(()).await.unwrap().pending.is_empty());
}

#[tokio::test]
async fn the_data_directory_is_private() {
    let fx = Fixture::new();
    let file = fx.project_path("CLAUDE.md");
    fx.write(&file, "a");
    let host = host(&fx, claims(&[&file]));
    run(&host, &fx, &save(&file, "a", "b")).await.unwrap();

    let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    let data = fx.root().join("data");
    assert_eq!(mode(data.as_path()), 0o700);
    assert_eq!(mode(data.join("history/changes").as_path()), 0o700);
    for entry in std::fs::read_dir(data.join("history/blobs").as_path()).unwrap() {
        assert_eq!(mode(&entry.unwrap().path()), 0o600);
    }
}

// ---------------------------------------------------------------------------------------------
// 3. Conflict and stale
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn an_outside_write_before_plan_is_a_conflict_with_the_current_text() {
    let fx = Fixture::new();
    let file = fx.project_path("CLAUDE.md");
    fx.write(&file, "loaded");
    let host = host(&fx, claims(&[&file]));
    let edit = save(&file, "loaded", "mine");
    std::fs::write(file.as_path(), "outside").unwrap();

    let err = plan(&host, &fx, &edit).await.unwrap_err();
    assert_eq!(
        err,
        ApiError::Conflict {
            path: file.clone(),
            expected: rev("loaded"),
            found: rev("outside"),
            current: Some("outside".into()),
        }
    );
    assert_eq!(apply(&host, &fx, &edit, "x").await.unwrap_err(), err);
    assert_eq!(read(&file), "outside");
    assert!(history(&host).await.is_empty());
}

#[tokio::test]
async fn an_outside_write_between_plan_and_apply_never_gets_overwritten() {
    let fx = Fixture::new();
    let file = fx.project_path("CLAUDE.md");
    fx.write(&file, "loaded");
    let host = host(&fx, claims(&[&file]));
    let edit = save(&file, "loaded", "mine");
    let preview = plan(&host, &fx, &edit).await.unwrap();
    std::fs::write(file.as_path(), "outside").unwrap();

    // A save carries its own base, so the second plan fails as a conflict (design section 4).
    let err = apply(&host, &fx, &edit, &preview.digest).await.unwrap_err();
    assert!(matches!(err, ApiError::Conflict { current: Some(ref c), .. } if c == "outside"));
    assert_eq!(read(&file), "outside");
    assert!(history(&host).await.is_empty());
}

fn skill_agent(entry: &AbsPath, dir: &AbsPath) -> TestAgent {
    TestAgent {
        files: vec![entry.clone()],
        slot: Some(Place::NamedDir {
            dir: dir.clone(),
            file: "SKILL.md".into(),
        }),
        skills: vec![entry.clone()],
        ..TestAgent::default()
    }
}

#[tokio::test]
async fn a_change_next_to_a_trashed_skill_between_plan_and_apply_is_stale() {
    let fx = Fixture::new();
    let dir = fx.project_path(".claude/skills");
    let entry = dir.join("deploy/SKILL.md");
    fx.write(&entry, "skill");
    fx.write(&dir.join("deploy/notes.md"), "notes");
    let trash = FakeTrash::default();
    let host = host_with(
        &fx,
        skill_agent(&entry, &dir),
        false,
        Box::new(trash.clone()),
    );
    let edit = Edit::Trash {
        path: entry.clone(),
        base: Revision::of(b"skill"),
    };
    let preview = plan(&host, &fx, &edit).await.unwrap();
    fx.write(&dir.join("deploy/added.md"), "new file");

    let stale = apply(&host, &fx, &edit, &preview.digest).await.unwrap();
    let ApplyResult::Stale { now } = stale else {
        panic!("expected Stale, got {stale:?}");
    };
    assert_ne!(now.digest, preview.digest);
    assert!(entry.as_path().exists(), "nothing was written");
    assert!(trash.0.lock().unwrap().is_empty());
    assert!(history(&host).await.is_empty());

    let result = apply(&host, &fx, &edit, &now.digest).await.unwrap();
    applied(result);
    assert!(!dir.join("deploy").as_path().exists());
}

// ---------------------------------------------------------------------------------------------
// 4. Crash and recovery
// ---------------------------------------------------------------------------------------------

struct Two {
    fx: Fixture,
    a: AbsPath,
    b: AbsPath,
}

impl Two {
    fn new() -> Two {
        let fx = Fixture::new();
        let a = fx.project_path("A.md");
        let b = fx.project_path("B.md");
        fx.write(&a, "a0");
        fx.write(&b, "b0");
        Two { fx, a, b }
    }

    fn agent(&self) -> TestAgent {
        TestAgent {
            files: vec![self.a.clone(), self.b.clone()],
            writes: vec![(self.a.clone(), "a1".into()), (self.b.clone(), "b1".into())],
            ..TestAgent::default()
        }
    }

    fn edit(&self) -> Edit {
        Edit::NewItem {
            agent: AgentId::parse("claude").unwrap(),
            slot: SlotId("s".into()),
            name: None,
        }
    }

    /// Applies the two-file change on a host that fails after `step`, as a crash would.
    async fn crash_after(&self, step: Step) -> ApiError {
        let host = host(&self.fx, self.agent());
        let preview = plan(&host, &self.fx, &self.edit()).await.unwrap();
        assert_eq!(preview.change_set.ops.len(), 2);
        *host.fail_after.lock().unwrap() = Some(step);
        apply(&host, &self.fx, &self.edit(), &preview.digest)
            .await
            .unwrap_err()
    }

    /// A new host over the same data directory: recovery runs as the host starts.
    fn restart(&self) -> Host {
        host(&self.fx, self.agent())
    }
}

/// Design section 4: recovery never writes an agent file. It seals a record whose files are all
/// at after, aborts one whose files are all at before, and leaves any other for the user.
#[tokio::test]
async fn recovery_after_a_crash_classifies_the_record_and_leaves_the_files_as_they_are() {
    for (step, files, state) in [
        (Step::Pending, ("a0", "b0"), ChangeState::Aborted),
        (Step::Op(0), ("a1", "b0"), ChangeState::NeedsAttention),
        (Step::Op(1), ("a1", "b1"), ChangeState::Sealed),
        (Step::Executed, ("a1", "b1"), ChangeState::Sealed),
        (Step::Sealed, ("a1", "b1"), ChangeState::Sealed),
    ] {
        let two = Two::new();
        let err = two.crash_after(step).await;
        assert!(
            matches!(&err, ApiError::Io { detail } if detail.contains("injected")),
            "{step:?}: {err:?}"
        );

        let restarted = two.restart();
        assert_eq!(
            (read(&two.a).as_str(), read(&two.b).as_str()),
            files,
            "{step:?}"
        );
        let records = history(&restarted).await;
        assert_eq!(records.len(), 1, "{step:?}");
        assert_eq!(records[0].state, state, "{step:?}");
        let unfinished = state == ChangeState::NeedsAttention;
        assert_eq!(
            restarted.session(()).await.unwrap().pending,
            if unfinished {
                vec![records[0].id.clone()]
            } else {
                Vec::new()
            },
            "{step:?}"
        );
        let pending = two.fx.root().join(&format!(
            "data/history/changes/{}.pending.json",
            records[0].id.0
        ));
        assert_eq!(pending.as_path().exists(), unfinished, "{step:?}");
    }
}

#[tokio::test]
async fn a_crash_before_the_pending_record_leaves_nothing_to_recover() {
    let two = Two::new();
    two.crash_after(Step::Blobs).await;
    let restarted = two.restart();
    assert_eq!(read(&two.a), "a0");
    assert_eq!(read(&two.b), "b0");
    assert!(history(&restarted).await.is_empty());

    // The blobs of that run are collected once they are a day old.
    let blobs = two.fx.root().join("data/history/blobs");
    assert!(std::fs::read_dir(blobs.as_path()).unwrap().count() > 0);
    let history = crate::History::open(two.fx.root().join("data").as_path());
    assert_eq!(history.collect_garbage(Duration::from_secs(86_400)), 0);
    assert!(history.collect_garbage(Duration::ZERO) > 0);
}

#[tokio::test]
async fn recovery_removes_a_temp_file_the_crash_left_beside_a_target() {
    let two = Two::new();
    two.crash_after(Step::Pending).await;
    let id = crate::History::open(two.fx.root().join("data").as_path())
        .pending()
        .remove(0)
        .id;
    let tmp = two.fx.project_path(&format!(".cm-{}-0.tmp", id.0));
    two.fx.write(&tmp, "half written");

    two.restart();
    assert!(!tmp.as_path().exists());
    assert_eq!(read(&two.a), "a0");
}

#[tokio::test]
async fn an_outside_edit_after_the_crash_keeps_the_record_pending_and_names_the_file() {
    let two = Two::new();
    two.crash_after(Step::Op(0)).await;
    assert_eq!((read(&two.a).as_str(), read(&two.b).as_str()), ("a1", "b0"));
    std::fs::write(two.b.as_path(), "outside").unwrap();

    let restarted = two.restart();
    let report = recovery::recover(&restarted.ctx, true);
    assert_eq!(report.sealed, Vec::<ChangeId>::new());
    assert_eq!(report.aborted, Vec::<ChangeId>::new());
    assert_eq!(report.attention.len(), 1);
    assert_eq!(
        report.attention[0].1,
        vec![
            (two.a.clone(), recovery::OpState::AtAfter),
            (two.b.clone(), recovery::OpState::Other)
        ]
    );
    assert_eq!(
        read(&two.b),
        "outside",
        "the user's edit is never overwritten"
    );
    assert_eq!(read(&two.a), "a1");

    let records = history(&restarted).await;
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].state, ChangeState::NeedsAttention);
    assert_eq!(
        restarted.session(()).await.unwrap().pending,
        vec![records[0].id.clone()]
    );

    // Running recovery again changes nothing.
    let changes = two.fx.root().join("data/history/changes");
    let snapshot = |dir: &AbsPath| {
        let mut entries: Vec<_> = std::fs::read_dir(dir.as_path())
            .unwrap()
            .map(|e| {
                let path = e.unwrap().path();
                (path.clone(), std::fs::read(path).unwrap())
            })
            .collect();
        entries.sort();
        entries
    };
    let before = snapshot(&changes);
    let again = recovery::recover(&restarted.ctx, true);
    assert_eq!(again.attention, report.attention);
    assert_eq!(snapshot(&changes), before);
}

#[tokio::test]
async fn a_read_only_host_classifies_a_pending_record_and_writes_nothing() {
    let two = Two::new();
    two.crash_after(Step::Op(0)).await;
    let ro = host_with(&two.fx, two.agent(), true, Box::new(FakeTrash::default()));
    assert_eq!(read(&two.b), "b0");
    let records = history(&ro).await;
    assert_eq!(records[0].state, ChangeState::Pending);
    assert_eq!(ro.session(()).await.unwrap().pending.len(), 1);
}

#[tokio::test]
async fn a_revert_of_a_record_that_needs_attention_restores_its_files_and_resolves_it() {
    let two = Two::new();
    two.crash_after(Step::Op(0)).await;
    let host = two.restart();
    assert_eq!((read(&two.a).as_str(), read(&two.b).as_str()), ("a1", "b0"));
    let id = history(&host).await[0].id.clone();

    applied(
        run(&host, &two.fx, &Edit::Revert { change: id.clone() })
            .await
            .unwrap(),
    );
    assert_eq!((read(&two.a).as_str(), read(&two.b).as_str()), ("a0", "b0"));
    let records = history(&host).await;
    let reverted = records.iter().find(|r| r.id == id).unwrap();
    assert_eq!(reverted.state, ChangeState::Aborted);
    assert!(host.session(()).await.unwrap().pending.is_empty());
    drop(host);
    two.restart();
    assert_eq!((read(&two.a).as_str(), read(&two.b).as_str()), ("a0", "b0"));
}

#[tokio::test]
async fn a_revert_of_a_record_that_needs_attention_skips_a_file_someone_else_changed() {
    let two = Two::new();
    two.crash_after(Step::Op(0)).await;
    std::fs::write(two.b.as_path(), "b-mine").unwrap();
    let host = two.restart();
    let id = history(&host).await[0].id.clone();
    assert_eq!(history(&host).await[0].state, ChangeState::NeedsAttention);

    let revert = Edit::Revert { change: id.clone() };
    let preview = plan(&host, &two.fx, &revert).await.unwrap();
    assert_eq!(
        preview
            .conflicts
            .iter()
            .map(|c| c.path.clone())
            .collect::<Vec<_>>(),
        vec![two.b.clone()]
    );
    applied(
        apply(&host, &two.fx, &revert, &preview.digest)
            .await
            .unwrap(),
    );
    assert_eq!(
        (read(&two.a).as_str(), read(&two.b).as_str()),
        ("a0", "b-mine")
    );
    let records = history(&host).await;
    assert_eq!(
        records.iter().find(|r| r.id == id).unwrap().state,
        ChangeState::Aborted
    );
}

#[tokio::test]
async fn a_revert_of_a_sealed_change_with_a_file_someone_else_changed_is_a_conflict() {
    let two = Two::new();
    let host = two.restart();
    applied(run(&host, &two.fx, &two.edit()).await.unwrap());
    std::fs::write(two.b.as_path(), "b-mine").unwrap();
    let id = history(&host).await[0].id.clone();

    let revert = Edit::Revert { change: id };
    let preview = plan(&host, &two.fx, &revert).await.unwrap();
    let err = apply(&host, &two.fx, &revert, &preview.digest)
        .await
        .unwrap_err();
    assert!(matches!(err, ApiError::Conflict { .. }), "{err:?}");
    assert_eq!(
        (read(&two.a).as_str(), read(&two.b).as_str()),
        ("a1", "b-mine")
    );
}

#[tokio::test]
async fn a_revert_that_finds_every_file_at_before_still_resolves_the_record() {
    let two = Two::new();
    two.crash_after(Step::Op(0)).await;
    let host = two.restart();
    let id = history(&host).await[0].id.clone();
    // The user puts the file back by hand before reverting.
    std::fs::write(two.a.as_path(), "a0").unwrap();

    let revert = Edit::Revert { change: id.clone() };
    assert_eq!(
        run(&host, &two.fx, &revert).await.unwrap(),
        ApplyResult::AlreadyApplied
    );
    // Read the record without the recovery a history request runs first.
    let history = crate::History::open(two.fx.root().join("data").as_path());
    assert_eq!(history.record(&id).unwrap().state, ChangeState::Aborted);
    assert!(history.pending().is_empty());
}

/// Design section 4, "A failed apply is not a crash": the operations before the failed one are
/// undone, the record is aborted, and nothing applies the change later.
#[tokio::test]
async fn a_failed_operation_undoes_the_ones_before_it_and_the_change_never_applies_later() {
    // SAFETY: `geteuid` reads the effective user id and has no preconditions.
    if unsafe { libc::geteuid() } == 0 {
        eprintln!("skipped: root writes into a directory without write permission");
        return;
    }
    let fx = Fixture::new();
    let a = fx.project_path("A.md");
    let b = fx.project_path("locked/B.md");
    fx.write(&a, "a0");
    fx.write(&b, "b0");
    let agent = || TestAgent {
        files: vec![a.clone(), b.clone()],
        writes: vec![(a.clone(), "a1".into()), (b.clone(), "b1".into())],
        ..TestAgent::default()
    };
    let first = host(&fx, agent());
    let edit = Two::new().edit();
    let preview = plan(&first, &fx, &edit).await.unwrap();
    let locked = fx.project_path("locked");
    let mode = |m| std::fs::set_permissions(locked.as_path(), std::fs::Permissions::from_mode(m));
    mode(0o555).unwrap();

    let err = apply(&first, &fx, &edit, &preview.digest)
        .await
        .unwrap_err();
    assert!(matches!(err, ApiError::Io { .. }), "{err:?}");
    assert_eq!((read(&a).as_str(), read(&b).as_str()), ("a0", "b0"));
    let records = history(&first).await;
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].state, ChangeState::Aborted);
    assert!(first.session(()).await.unwrap().pending.is_empty());

    // The cause goes away. Reading the history and starting again write nothing.
    mode(0o755).unwrap();
    assert_eq!(history(&first).await[0].state, ChangeState::Aborted);
    drop(first);
    let restarted = host(&fx, agent());
    assert_eq!(history(&restarted).await[0].state, ChangeState::Aborted);
    assert_eq!((read(&a).as_str(), read(&b).as_str()), ("a0", "b0"));
}

// ---------------------------------------------------------------------------------------------
// 5. Revert and trash
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn revert_restores_the_before_text_and_a_second_revert_is_already_applied() {
    let fx = Fixture::new();
    let file = fx.project_path("CLAUDE.md");
    fx.write(&file, "old");
    let host = host(&fx, claims(&[&file]));
    let (change, ..) = applied(run(&host, &fx, &save(&file, "old", "new")).await.unwrap());

    let revert = Edit::Revert {
        change: change.clone(),
    };
    let (undo, ..) = applied(run(&host, &fx, &revert).await.unwrap());
    assert_eq!(read(&file), "old");
    assert_eq!(
        run(&host, &fx, &revert).await.unwrap(),
        ApplyResult::AlreadyApplied
    );
    assert_eq!(history(&host).await.len(), 2);

    // The revert is itself a change, and reverting it redoes the save.
    applied(
        run(&host, &fx, &Edit::Revert { change: undo })
            .await
            .unwrap(),
    );
    assert_eq!(read(&file), "new");
}

#[tokio::test]
async fn a_revert_over_an_outside_edit_is_a_conflict_and_writes_nothing() {
    let fx = Fixture::new();
    let file = fx.project_path("CLAUDE.md");
    fx.write(&file, "old");
    let host = host(&fx, claims(&[&file]));
    let (change, ..) = applied(run(&host, &fx, &save(&file, "old", "new")).await.unwrap());
    std::fs::write(file.as_path(), "someone else").unwrap();

    let revert = Edit::Revert { change };
    let preview = plan(&host, &fx, &revert).await.unwrap();
    assert_eq!(preview.conflicts.len(), 1);
    assert_eq!(
        preview.conflicts[0].current.as_deref(),
        Some("someone else")
    );
    let err = apply(&host, &fx, &revert, &preview.digest)
        .await
        .unwrap_err();
    assert!(matches!(err, ApiError::Conflict { .. }));
    assert_eq!(read(&file), "someone else");
}

#[tokio::test]
async fn a_trashed_skill_directory_comes_back_from_its_blobs() {
    let fx = Fixture::new();
    let dir = fx.project_path(".claude/skills");
    let entry = dir.join("deploy/SKILL.md");
    fx.write(&entry, "skill");
    fx.write(&dir.join("deploy/refs/notes.md"), "notes");
    let trash = FakeTrash::default();
    let host = host_with(
        &fx,
        skill_agent(&entry, &dir),
        false,
        Box::new(trash.clone()),
    );

    let edit = Edit::Trash {
        path: entry.clone(),
        base: Revision::of(b"skill"),
    };
    let (change, files, not_trashed) = applied(run(&host, &fx, &edit).await.unwrap());
    assert!(!dir.join("deploy").as_path().exists());
    assert_eq!(
        *trash.0.lock().unwrap(),
        vec![dir.join("deploy").to_string()]
    );
    assert_eq!(files, vec![(dir.join("deploy"), None)]);
    assert!(not_trashed.is_empty());

    let revert = Edit::Revert { change };
    applied(run(&host, &fx, &revert).await.unwrap());
    assert_eq!(read(&entry), "skill");
    assert_eq!(read(&dir.join("deploy/refs/notes.md")), "notes");
    assert_eq!(
        run(&host, &fx, &revert).await.unwrap(),
        ApplyResult::AlreadyApplied
    );
}

/// Design section 4, step 4: without an OS trash, a directory the history does not hold
/// completely is never removed.
#[tokio::test]
async fn without_an_os_trash_a_skill_the_history_cannot_hold_stays_on_disk() {
    let fx = Fixture::new();
    let dir = fx.project_path(".claude/skills");
    let entry = dir.join("deep/SKILL.md");
    fx.write(&entry, "skill");
    let levels: String = (1..=17).map(|i| format!("d{i}/")).collect();
    let deep = dir.join(&format!("deep/{levels}f.txt"));
    let near = dir.join("deep/refs/notes.md");
    fx.write(&deep, "17 levels down");
    fx.write(&near, "notes");
    let host = host_with(&fx, skill_agent(&entry, &dir), false, Box::new(NoTrash));
    let edit = Edit::Trash {
        path: entry.clone(),
        base: Revision::of(b"skill"),
    };
    let preview = plan(&host, &fx, &edit).await.unwrap();
    assert!(preview.warnings.contains(&Warning::NotRevertible));

    let err = apply(&host, &fx, &edit, &preview.digest).await.unwrap_err();
    assert!(matches!(err, ApiError::Io { .. }), "{err:?}");
    assert_eq!(
        (read(&entry), read(&near), read(&deep)),
        ("skill".into(), "notes".into(), "17 levels down".into())
    );
    let records = history(&host).await;
    assert_eq!(records[0].state, ChangeState::Aborted);
}

#[tokio::test]
async fn trashing_a_single_file_and_reverting_it() {
    let fx = Fixture::new();
    let file = fx.project_path("CLAUDE.md");
    fx.write(&file, "keep me");
    let trash = FakeTrash::default();
    let host = host_with(&fx, claims(&[&file]), false, Box::new(trash.clone()));
    let edit = Edit::Trash {
        path: file.clone(),
        base: Revision::of(b"keep me"),
    };
    let (change, ..) = applied(run(&host, &fx, &edit).await.unwrap());
    assert!(!file.as_path().exists());
    assert_eq!(trash.0.lock().unwrap().len(), 1);
    assert_eq!(
        run(&host, &fx, &edit).await.unwrap(),
        ApplyResult::AlreadyApplied
    );

    applied(run(&host, &fx, &Edit::Revert { change }).await.unwrap());
    assert_eq!(read(&file), "keep me");
}

#[tokio::test]
async fn without_an_os_trash_the_file_is_removed_and_reported_and_revert_still_works() {
    let fx = Fixture::new();
    let file = fx.project_path("CLAUDE.md");
    fx.write(&file, "keep me");
    let host = host_with(&fx, claims(&[&file]), false, Box::new(NoTrash));
    let edit = Edit::Trash {
        path: file.clone(),
        base: Revision::of(b"keep me"),
    };
    let (change, _, not_trashed) = applied(run(&host, &fx, &edit).await.unwrap());
    assert_eq!(not_trashed, vec![file.clone()]);
    assert!(!file.as_path().exists());
    applied(run(&host, &fx, &Edit::Revert { change }).await.unwrap());
    assert_eq!(read(&file), "keep me");
}

/// The one test that calls the real OS trash. It moves a file of the fixture and nothing else.
/// It does not list `~/.Trash`, which macOS refuses to a terminal: success of the call is the
/// evidence. Every run would leave a file in the operator's trash, so it runs only with
/// `CM_TEST_REAL_TRASH=1`.
#[tokio::test]
async fn the_os_trash_takes_the_file() {
    if std::env::var("CM_TEST_REAL_TRASH").as_deref() != Ok("1") {
        eprintln!("skipped: set CM_TEST_REAL_TRASH=1 to send a temp file to the real OS trash");
        return;
    }
    let fx = Fixture::new();
    let file = fx.project_path("CLAUDE.md");
    fx.write(&file, "to the trash");
    let host = host_with(&fx, claims(&[&file]), false, Box::new(OsTrash));
    let edit = Edit::Trash {
        path: file.clone(),
        base: Revision::of(b"to the trash"),
    };
    let (change, _, not_trashed) = applied(run(&host, &fx, &edit).await.unwrap());
    assert!(!file.as_path().exists());
    assert!(
        not_trashed.is_empty(),
        "the trash call failed: {not_trashed:?}"
    );
    applied(run(&host, &fx, &Edit::Revert { change }).await.unwrap());
    assert_eq!(read(&file), "to the trash");
}

// ---------------------------------------------------------------------------------------------
// 6. Symlinks and modes
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn a_symlinked_file_is_written_through_and_the_link_survives() {
    let fx = Fixture::new();
    let target = fx.project_path("shared/rules.md");
    let link = fx.project_path("CLAUDE.md");
    fx.write(&target, "old");
    fx.symlink(&link, &target);
    let host = host(&fx, claims(&[&link]));
    let edit = save(&link, "old", "new");

    let preview = plan(&host, &fx, &edit).await.unwrap();
    assert!(preview.warnings.contains(&Warning::ThroughSymlink {
        target: target.clone()
    }));
    applied(apply(&host, &fx, &edit, &preview.digest).await.unwrap());
    assert_eq!(read(&target), "new");
    let meta = std::fs::symlink_metadata(link.as_path()).unwrap();
    assert!(meta.is_symlink());
    assert_eq!(
        std::fs::read_link(link.as_path()).unwrap(),
        target.as_path()
    );
    assert_eq!(read(&link), "new");
}

#[tokio::test]
async fn the_mode_bits_of_a_file_survive_a_write() {
    let fx = Fixture::new();
    let file = fx.project_path("run.sh");
    fx.write(&file, "#!/bin/sh\n");
    std::fs::set_permissions(file.as_path(), std::fs::Permissions::from_mode(0o750)).unwrap();
    let host = host(&fx, claims(&[&file]));
    applied(
        run(
            &host,
            &fx,
            &save(&file, "#!/bin/sh\n", "#!/bin/sh\necho hi\n"),
        )
        .await
        .unwrap(),
    );
    let mode = std::fs::metadata(file.as_path())
        .unwrap()
        .permissions()
        .mode()
        & 0o7777;
    assert_eq!(mode, 0o750);
    assert_eq!(read(&file), "#!/bin/sh\necho hi\n");
    let leftovers: Vec<_> = std::fs::read_dir(fx.project().as_path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(leftovers, vec!["run.sh"], "no temp file is left behind");
}

#[tokio::test]
async fn a_new_file_in_a_slot_is_created_with_its_directories() {
    let fx = Fixture::new();
    let dir = fx.project_path(".claude/commands");
    let path = dir.join("deploy.md");
    let host = host(
        &fx,
        TestAgent {
            slot: Some(Place::NamedFile {
                dir,
                ext: "md".into(),
            }),
            ..TestAgent::default()
        },
    );
    let edit = Edit::Save {
        path: path.clone(),
        base: Base::Absent,
        text: "go".into(),
    };
    applied(run(&host, &fx, &edit).await.unwrap());
    assert_eq!(read(&path), "go");
    assert_eq!(
        run(&host, &fx, &edit).await.unwrap(),
        ApplyResult::AlreadyApplied
    );
}

// ---------------------------------------------------------------------------------------------
// 7. Guard and 8. policy
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn a_guard_that_refuses_fails_plan_and_apply_the_same_way() {
    let fx = Fixture::new();
    let file = fx.project_path("config.toml");
    fx.write(&file, "a = 1\n");
    let host = host(
        &fx,
        TestAgent {
            files: vec![file.clone()],
            refuse: Some("trusted_hash"),
            ..TestAgent::default()
        },
    );
    let edit = save(&file, "a = 1\n", "trusted_hash = \"x\"\n");
    let planned = plan(&host, &fx, &edit).await.unwrap_err();
    assert!(matches!(planned, ApiError::Forbidden { .. }));
    let applied = apply(&host, &fx, &edit, "any digest").await.unwrap_err();
    assert_eq!(applied, planned);
    assert_eq!(read(&file), "a = 1\n");
    assert!(history(&host).await.is_empty());
}

#[tokio::test]
async fn a_read_only_host_refuses_apply_and_still_plans() {
    let fx = Fixture::new();
    let file = fx.project_path("CLAUDE.md");
    fx.write(&file, "old");
    let host = host_with(&fx, claims(&[&file]), true, Box::new(FakeTrash::default()));
    let edit = save(&file, "old", "new");
    let preview = plan(&host, &fx, &edit).await.unwrap();
    let err = apply(&host, &fx, &edit, &preview.digest).await.unwrap_err();
    assert!(matches!(err, ApiError::ReadOnly { .. }));
    assert_eq!(read(&file), "old");
}

#[tokio::test]
async fn an_agent_edit_nobody_plans_is_invalid_through_the_host() {
    let fx = Fixture::new();
    let host = host(&fx, TestAgent::default());
    let err = plan(
        &host,
        &fx,
        &Edit::SetHookEnabled {
            hook: HookId("h".into()),
            enabled: false,
        },
    )
    .await
    .unwrap_err();
    assert!(matches!(err, ApiError::InvalidEdit { .. }));
}
