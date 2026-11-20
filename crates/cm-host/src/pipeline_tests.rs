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

/// The name and bytes of every file in `dir`, sorted.
fn files_in(dir: &AbsPath) -> Vec<(std::path::PathBuf, Vec<u8>)> {
    let mut files: Vec<_> = std::fs::read_dir(dir.as_path())
        .unwrap()
        .map(|e| {
            let path = e.unwrap().path();
            (path.clone(), std::fs::read(path).unwrap())
        })
        .collect();
    files.sort();
    files
}

/// Design section 4, "The apply pipeline": a lock on a file in the data directory serializes
/// `apply` across processes. Another process holding it is applying a change, so recovery leaves
/// the pending record and its temp files alone, and an apply here waits.
#[tokio::test]
async fn while_another_process_holds_the_lock_recovery_leaves_its_change_alone_and_apply_waits() {
    let two = Two::new();
    two.crash_after(Step::Op(0)).await;
    let data = two.fx.root().join("data");
    let changes = data.join("history/changes");
    let id = crate::History::open(data.as_path()).pending().remove(0).id;
    let tmp = two.fx.project_path(&format!(".cm-{}-1.tmp", id.0));
    two.fx.write(&tmp, "half written");
    let records_before = files_in(&changes);

    let other = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(data.join(crate::apply::LOCK_FILE).as_path())
        .unwrap();
    other.lock().unwrap();

    let host = two.restart();
    let records = history(&host).await;
    assert_eq!(
        records[0].state,
        ChangeState::Pending,
        "recovery classified a change another process is applying"
    );
    assert_eq!(
        files_in(&changes),
        records_before,
        "recovery wrote a record while another process held the lock"
    );
    assert!(
        tmp.as_path().exists(),
        "recovery removed a temp file of a change in progress"
    );

    let edit = save(&two.b, "b0", "b2");
    let preview = plan(&host, &two.fx, &edit).await.unwrap();
    let digest = preview.digest.clone();
    let host = Arc::new(host);
    let waiting = {
        let (host, fx_project, edit) = (host.clone(), two.fx.project(), edit.clone());
        tokio::spawn(async move {
            host.apply(ApplyReq {
                cwd: fx_project,
                edit,
                digest,
            })
            .await
        })
    };
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        !waiting.is_finished() && read(&two.b) == "b0",
        "an apply ran while another process held the lock"
    );

    drop(other);
    applied(waiting.await.unwrap().unwrap());
    assert_eq!(read(&two.b), "b2");
}

/// Two hosts over one data directory, as two windows of the tool. A history read on the second
/// while the first is between two operations of a change returns, and leaves that change alone.
#[tokio::test]
async fn a_history_read_on_a_second_host_during_an_apply_leaves_the_change_alone() {
    let two = Two::new();
    let first = host(&two.fx, two.agent());
    let second = host(&two.fx, two.agent());
    let preview = plan(&first, &two.fx, &two.edit()).await.unwrap();
    let (paused_tx, paused) = std::sync::mpsc::channel();
    let (resume, resume_rx) = std::sync::mpsc::channel::<()>();
    let applying = {
        let (ctx, cwd, edit, digest) = (
            first.ctx.clone(),
            two.fx.project(),
            two.edit(),
            preview.digest.clone(),
        );
        tokio::task::spawn_blocking(move || {
            ctx.apply(&cwd, &edit, &digest, &|step| {
                if step == Step::Op(0) {
                    paused_tx.send(()).unwrap();
                    resume_rx.recv().unwrap();
                }
                Ok(())
            })
        })
    };
    paused.recv_timeout(Duration::from_secs(10)).unwrap();
    let changes = two.fx.root().join("data/history/changes");
    let records_before = files_in(&changes);

    let read_during = tokio::time::timeout(Duration::from_secs(5), history(&second)).await;
    let records_during = files_in(&changes);
    resume.send(()).unwrap();
    let records = read_during.expect("a history read waited for an apply on another host");
    assert_eq!(records[0].state, ChangeState::Pending);
    assert_eq!(
        records_during, records_before,
        "a history read changed the record of a change in progress"
    );

    applied(applying.await.unwrap().unwrap());
    assert_eq!((read(&two.a).as_str(), read(&two.b).as_str()), ("a1", "b1"));
    assert_eq!(history(&second).await[0].state, ChangeState::Sealed);
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

/// Trashes the skill `<project>/.claude/skills/<name>` on a host with no OS trash. The plan must
/// warn `NotRevertible`, the apply must fail, and every path in `kept` must still be there.
async fn trash_without_os_trash_keeps(fx: &Fixture, name: &str, kept: &[std::path::PathBuf]) {
    let dir = fx.project_path(".claude/skills");
    let entry = dir.join(&format!("{name}/SKILL.md"));
    let host = host_with(fx, skill_agent(&entry, &dir), false, Box::new(NoTrash));
    let edit = Edit::Trash {
        path: entry.clone(),
        base: Revision::of(b"skill"),
    };
    let preview = plan(&host, fx, &edit).await.unwrap();
    assert!(
        preview.warnings.contains(&Warning::NotRevertible),
        "{name}: the plan holds the tree as complete: {:?}",
        preview.warnings
    );
    let err = apply(&host, fx, &edit, &preview.digest).await.unwrap_err();
    assert!(matches!(err, ApiError::Io { .. }), "{name}: {err:?}");
    for path in std::iter::once(entry.as_path().to_path_buf()).chain(kept.iter().cloned()) {
        assert!(
            std::fs::symlink_metadata(&path).is_ok(),
            "{name}: {} is gone",
            path.display()
        );
    }
}

/// Design section 4, step 4: a tree that holds a symlink is not captured completely. History
/// cannot hold the link, and the files behind it are not inside the tree.
#[tokio::test]
async fn without_an_os_trash_a_skill_that_holds_a_symlink_stays_on_disk() {
    let fx = Fixture::new();
    let shared = fx.root().join("shared");
    fx.write(&shared.join("run.sh"), "echo shared");
    let skills = fx.project_path(".claude/skills");
    for (name, link, target) in [
        ("dir-link", "scripts", shared.clone()),
        ("file-link", "run.sh", shared.join("run.sh")),
    ] {
        fx.write(&skills.join(&format!("{name}/SKILL.md")), "skill");
        let link = skills.join(&format!("{name}/{link}"));
        fx.symlink(&link, &target);
        trash_without_os_trash_keeps(&fx, name, &[link.as_path().to_path_buf()]).await;
        assert_eq!(read(&shared.join("run.sh")), "echo shared");
    }
}

/// The lister has no entry for a name that is not UTF-8. The tree is then not captured
/// completely, and stays on disk without an OS trash.
#[tokio::test]
async fn without_an_os_trash_a_skill_that_holds_a_name_that_is_not_utf8_stays_on_disk() {
    use std::os::unix::ffi::OsStrExt;
    let fx = Fixture::new();
    let skill = fx.project_path(".claude/skills/odd");
    fx.write(&skill.join("SKILL.md"), "skill");
    let odd = skill
        .as_path()
        .join(std::ffi::OsStr::from_bytes(b"note-\xff.md"));
    if let Err(err) = std::fs::write(&odd, "notes") {
        eprintln!("skipped: this file system refuses a file name that is not UTF-8 ({err})");
        return;
    }
    trash_without_os_trash_keeps(&fx, "odd", &[odd]).await;
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

/// Design section 4: a path that is a symlink to nothing is an `InvalidEdit`. The tool does not
/// create a file through a link, so nothing appears outside the project.
#[tokio::test]
async fn a_save_through_a_symlink_to_nothing_is_refused_and_creates_nothing() {
    let fx = Fixture::new();
    let link = fx.project_path("CLAUDE.md");
    let outside = fx.root().join("outside");
    fx.mkdir(&outside);
    std::os::unix::fs::symlink("../outside/newdir/planted.md", link.as_path()).unwrap();
    let host = host(&fx, claims(&[&link]));
    let edit = Edit::Save {
        path: link.clone(),
        base: Base::Absent,
        text: "operator text\n".into(),
    };

    let err = plan(&host, &fx, &edit).await.unwrap_err();
    assert_eq!(
        err,
        ApiError::InvalidEdit {
            field: "path".into(),
            why: RuleRef::new("edit.dangling_symlink"),
        }
    );
    assert_eq!(
        apply(&host, &fx, &edit, "any digest").await.unwrap_err(),
        err
    );
    assert!(
        !outside.join("newdir").as_path().exists(),
        "a save created a directory outside the project"
    );
    assert!(
        std::fs::symlink_metadata(link.as_path())
            .unwrap()
            .is_symlink()
    );
    assert!(history(&host).await.is_empty());
}

/// The link to nothing appears after the plan. The write refuses it as well, and the change is
/// undone.
#[tokio::test]
async fn a_symlink_to_nothing_that_appears_during_an_apply_creates_nothing() {
    let fx = Fixture::new();
    let link = fx.project_path("CLAUDE.md");
    let outside = fx.root().join("outside");
    fx.mkdir(&outside);
    let host = host(&fx, claims(&[&link]));
    let edit = Edit::Save {
        path: link.clone(),
        base: Base::Absent,
        text: "operator text\n".into(),
    };
    let preview = plan(&host, &fx, &edit).await.unwrap();

    let result = host
        .ctx
        .apply(&fx.project(), &edit, &preview.digest, &|step| {
            if step == Step::Pending {
                std::os::unix::fs::symlink("../outside/newdir/planted.md", link.as_path()).unwrap();
            }
            Ok(())
        });
    assert!(matches!(result, Err(ApiError::Io { .. })), "{result:?}");
    assert!(
        !outside.join("newdir").as_path().exists(),
        "an apply created a directory outside the project"
    );
    assert_eq!(history(&host).await[0].state, ChangeState::Aborted);
}

/// Design section 4, `Warning::ThroughSymlink`: a directory above the path, below the project
/// root, is a link. The preview names the place the write lands.
#[tokio::test]
async fn a_save_under_a_symlinked_directory_names_the_place_it_lands() {
    let fx = Fixture::new();
    let elsewhere = fx.root().join("elsewhere/commands");
    fx.mkdir(&elsewhere);
    let dir = fx.project_path(".claude/commands");
    fx.symlink(&dir, &elsewhere);
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

    let preview = plan(&host, &fx, &edit).await.unwrap();
    assert!(
        preview.warnings.contains(&Warning::ThroughSymlink {
            target: elsewhere.join("deploy.md")
        }),
        "the preview does not say where the write lands: {:?}",
        preview.warnings
    );
    applied(apply(&host, &fx, &edit, &preview.digest).await.unwrap());
    assert_eq!(read(&elsewhere.join("deploy.md")), "go");
}

/// A link at or above the project root is where the user keeps the project, as macOS `/tmp` is.
/// A write below it does not go through a link, and the preview does not warn.
#[tokio::test]
async fn a_project_below_a_symlinked_directory_gets_no_symlink_warning() {
    let fx = Fixture::new();
    let linked = fx.root().join("linked");
    fx.mkdir(&fx.root().join("real/project"));
    fx.symlink(&linked, &fx.root().join("real"));
    let project = linked.join("project");
    let file = project.join("CLAUDE.md");
    fx.write(&file, "old");
    let host = Host::with_parts(
        HostConfig {
            shell: Shell::Serve,
            read_only: false,
            env: cm_core::env::ScanEnv::for_fixture(fx.home(), project.clone()),
            watch_timeout: Duration::from_secs(1),
            data_dir: fx.root().join("data"),
        },
        agents(claims(&[&file])),
        Box::new(FakeTrash::default()),
    );

    let preview = host
        .plan(PlanReq {
            cwd: project,
            edit: save(&file, "old", "new"),
        })
        .await
        .unwrap();
    assert!(
        !preview
            .warnings
            .iter()
            .any(|w| matches!(w, Warning::ThroughSymlink { .. })),
        "{:?}",
        preview.warnings
    );
}

/// Design section 4, step 4: a file with more than one hard link is written in place, because a
/// rename would leave the other names on the old content. A revert restores every name.
#[tokio::test]
async fn a_save_of_a_hard_linked_file_changes_every_name_and_a_revert_restores_them() {
    use std::os::unix::fs::MetadataExt;
    let fx = Fixture::new();
    let agents_md = fx.project_path("AGENTS.md");
    let claude_md = fx.project_path("CLAUDE.md");
    fx.write(&agents_md, "shared v1\n");
    std::fs::hard_link(agents_md.as_path(), claude_md.as_path()).unwrap();
    let host = host(&fx, claims(&[&agents_md, &claude_md]));
    let names = || {
        let links = std::fs::metadata(agents_md.as_path()).unwrap().nlink();
        (read(&claude_md), read(&agents_md), links)
    };

    let edit = save(&claude_md, "shared v1\n", "shared v2\n");
    let preview = plan(&host, &fx, &edit).await.unwrap();
    assert!(
        preview.warnings.contains(&Warning::HardLinked { links: 2 }),
        "the preview does not say the file has other names: {:?}",
        preview.warnings
    );
    let (change, ..) = applied(apply(&host, &fx, &edit, &preview.digest).await.unwrap());
    assert_eq!(
        names(),
        ("shared v2\n".into(), "shared v2\n".into(), 2),
        "a save split the hard link"
    );

    applied(run(&host, &fx, &Edit::Revert { change }).await.unwrap());
    assert_eq!(
        names(),
        ("shared v1\n".into(), "shared v1\n".into(), 2),
        "a revert split the hard link"
    );
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
