//! The apply pipeline (design section 4, steps 1 to 5) and the file effects it runs.
//!
//! 1. Policy: refuse when the host is read only. The host does this in one place.
//! 2. Plan again against a fresh scan. An empty plan is `AlreadyApplied`. A digest that differs
//!    from the one the client saw is `Stale`.
//! 3. Journal: store the before and after bytes of each file as blobs, then write the pending
//!    record.
//! 4. Execute each operation in order. When one fails, undo the ones before it and abort the
//!    record.
//! 5. Seal: write the sealed record and remove the pending one.
//!
//! One mutex per process and a lock on a file in the data directory serialize steps 2 to 5,
//! within this process and across processes.

use std::fs;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::sync::Mutex;

use cm_core::agents::Agent;
use cm_core::edit::Planner;
use cm_core::env::ScanEnv;
use cm_core::scan::scan_with_parked;
use cm_core::{
    AbsPath, Action, ApiError, ApplyResult, Base, ChangeId, ChangeRecord, ChangeState, Edit,
    FileOp, JournalOp, Preview, Revision, Warning, sha256_hex,
};

use crate::fsio::{
    DATA_FILE_MODE, NEW_FILE_MODE, atomic_write, create_private_dir, io_detail, mode_of,
    write_target,
};
use crate::history::{History, new_change_id, now_iso};
use crate::recovery::{OpState, classify};

/// Serializes `apply` within this process.
static PIPELINE: Mutex<()> = Mutex::new(());

/// The file in the data directory whose lock `apply` holds from step 2 to step 5 (design section
/// 4, "The apply pipeline"). The operating system releases the lock of a process that exits.
pub(crate) const LOCK_FILE: &str = "apply.lock";

/// Moves a path to the OS trash. A test replaces it.
pub trait Trasher: Send + Sync {
    fn trash(&self, path: &Path) -> Result<(), String>;
}

pub struct OsTrash;

impl Trasher for OsTrash {
    fn trash(&self, path: &Path) -> Result<(), String> {
        let mut context = trash::TrashContext::default();
        // The Finder method asks the user for permission to control Finder. The file manager
        // method does not, and also works from a process with no window.
        #[cfg(target_os = "macos")]
        {
            use trash::macos::{DeleteMethod, TrashContextExtMacos};
            context.set_delete_method(DeleteMethod::NsFileManager);
        }
        context.delete(path).map_err(|e| e.to_string())
    }
}

/// A point in the pipeline, for the failure injection that tests use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// Every blob is stored.
    Blobs,
    /// The pending record is written.
    Pending,
    /// The operation at this index is executed.
    Op(usize),
    /// Every operation is executed, and the record is not yet sealed.
    Executed,
    /// The record is sealed.
    Sealed,
}

/// Called after each step. In a test build it can fail the run there, leaving the disk exactly as
/// a crash would.
pub(crate) type Checkpoint<'a> = &'a dyn Fn(Step) -> Result<(), ApiError>;

/// What `plan` and `apply` read and write.
pub(crate) struct Ctx {
    pub agents: &'static [&'static dyn Agent],
    pub env: ScanEnv,
    pub history: History,
    pub parked_dir: AbsPath,
    pub data_dir: AbsPath,
    pub trasher: Box<dyn Trasher>,
}

impl Ctx {
    /// Opens the lock file, and creates it and the data directory when they are missing.
    pub(crate) fn lock_file(&self) -> std::io::Result<fs::File> {
        create_private_dir(self.data_dir.as_path())?;
        fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .mode(DATA_FILE_MODE)
            .open(self.data_dir.join(LOCK_FILE).as_path())
    }

    pub fn plan(&self, cwd: &AbsPath, edit: &Edit) -> Result<Preview, ApiError> {
        let env = self.env.with_cwd(cwd.clone());
        let scanned = scan_with_parked(self.agents, &env, Some(&self.parked_dir));
        Planner {
            agents: self.agents,
            scanned: &scanned,
            env: &env,
            parked_dir: &self.parked_dir,
            changes: &self.history,
        }
        .plan(edit)
    }

    /// Steps 2 to 5. The caller has passed the policy step.
    pub fn apply(
        &self,
        cwd: &AbsPath,
        edit: &Edit,
        digest: &str,
        checkpoint: Checkpoint,
    ) -> Result<ApplyResult, ApiError> {
        let _serialized = PIPELINE.lock().unwrap_or_else(|e| e.into_inner());
        // Waits while another process applies. Dropping the file at the end releases the lock.
        let lock = self.lock_file().map_err(io_detail)?;
        lock.lock().map_err(io_detail)?;
        let now = self.plan(cwd, edit)?;
        // A revert of a record that never finished restores the files at after and leaves a file
        // at neither state as it is: the preview listed it as a conflict (design section 4).
        let unfinished = self.unfinished_target(edit);
        if unfinished.is_none()
            && let Some(conflict) = now.conflicts.first()
        {
            return Err(ApiError::Conflict {
                path: conflict.path.clone(),
                expected: conflict.expected.clone(),
                found: conflict.found.clone(),
                current: conflict.current.clone(),
            });
        }
        if now.change_set.ops.is_empty() {
            // Such a revert resolves the record also when it plans to nothing.
            if let Some(mut target) = unfinished {
                self.history.abort(&mut target).map_err(io_detail)?;
            }
            return Ok(ApplyResult::AlreadyApplied);
        }
        if now.digest != digest {
            return Ok(ApplyResult::Stale { now });
        }
        self.run(cwd, edit, &now, checkpoint)
    }

    fn run(
        &self,
        cwd: &AbsPath,
        edit: &Edit,
        preview: &Preview,
        checkpoint: Checkpoint,
    ) -> Result<ApplyResult, ApiError> {
        let id = new_change_id();

        // Step 3: journal.
        let (ops, ends) = self.journal(preview)?;
        let mut record = ChangeRecord {
            id: id.clone(),
            at: now_iso(),
            cwd: Some(cwd.clone()),
            summary: preview.change_set.summary.clone(),
            ops,
            state: ChangeState::Pending,
        };
        checkpoint(Step::Blobs)?;
        self.history.write_pending(&record).map_err(io_detail)?;
        checkpoint(Step::Pending)?;

        // Step 4: execute. A failed operation is not a crash: the run undoes what it did.
        let effects = self.effects();
        let held = !preview.warnings.contains(&Warning::NotRevertible);
        let mut files = Vec::new();
        let mut not_trashed = Vec::new();
        for (index, op) in preview.change_set.ops.iter().enumerate() {
            if let Err(err) = effects.run_op(&id.0, index, op, held, &mut files, &mut not_trashed) {
                let done = if index == 0 { 0 } else { ends[index - 1] };
                self.undo(&mut record, done, ends[index]);
                return Err(err);
            }
            checkpoint(Step::Op(index))?;
        }
        checkpoint(Step::Executed)?;

        // Step 5: seal. A revert of a record that never finished resolves it: none of that
        // change is on disk any more.
        if let Some(mut target) = self.unfinished_target(edit) {
            self.history.abort(&mut target).map_err(io_detail)?;
        }
        self.history.seal(&mut record).map_err(io_detail)?;
        checkpoint(Step::Sealed)?;
        Ok(ApplyResult::Applied {
            change: id,
            files,
            not_trashed,
        })
    }

    /// The record a revert targets, when it is still pending or needs attention.
    fn unfinished_target(&self, edit: &Edit) -> Option<ChangeRecord> {
        let Edit::Revert { change } = edit else {
            return None;
        };
        self.history
            .record(change)
            .filter(|r| matches!(r.state, ChangeState::Pending | ChangeState::NeedsAttention))
    }

    /// Design section 4, "A failed apply is not a crash". The journal operations before `done`
    /// belong to operations that completed, those up to `tried` also to the one that failed.
    /// Each file at its after state goes back to its before blob, last first, and the record is
    /// aborted. A completed operation's file that is at neither state was changed by someone
    /// else, and a restore can fail: then the record needs attention.
    fn undo(&self, record: &mut ChangeRecord, done: usize, tried: usize) {
        let effects = self.effects();
        let mut clean = true;
        for (index, op) in record.ops[..tried].iter().enumerate().rev() {
            let undone = match classify(op) {
                OpState::AtBefore => true,
                OpState::AtAfter => self.restore(&effects, &record.id, index, op).is_ok(),
                // The failed operation did not write this file.
                OpState::Other => index >= done,
            };
            clean &= undone;
        }
        let closed = clean && self.history.abort(record).is_ok();
        if !closed {
            record.state = ChangeState::NeedsAttention;
            let _ = self.history.write_pending(record);
        }
    }

    /// Puts one journaled file back to its before state.
    fn restore(
        &self,
        effects: &Effects,
        id: &ChangeId,
        index: usize,
        op: &JournalOp,
    ) -> std::io::Result<()> {
        match (&op.before, &op.after) {
            (Some(blob), _) => {
                let bytes = self.history.blob(blob)?;
                effects.write(&op.path, &bytes, &format!("{}-u{index}", id.0))
            }
            // A file the change created.
            (None, Some(_)) => fs::remove_file(write_target(&op.path)),
            // A directory that went to the trash: its files come back from their own blobs.
            (None, None) => fs::create_dir_all(op.path.as_path()),
        }
    }

    pub(crate) fn effects(&self) -> Effects<'_> {
        Effects {
            trasher: self.trasher.as_ref(),
            data_dir: &self.data_dir,
        }
    }

    /// Stores the blobs and builds the journal. A directory that goes to the trash whole has one
    /// operation per file it holds, and a last one for the directory itself. The second list
    /// holds, for each file operation, the length of the journal up to and including it.
    fn journal(&self, preview: &Preview) -> Result<(Vec<JournalOp>, Vec<usize>), ApiError> {
        let put = |text: &str| self.history.put_blob(text.as_bytes()).map_err(io_detail);
        let mut ops = Vec::new();
        let mut ends = Vec::new();
        for op in &preview.change_set.ops {
            match &op.action {
                Action::Write { text } => ops.push(JournalOp {
                    path: op.path.clone(),
                    before: preview
                        .before
                        .get(&op.path)
                        .and_then(|b| b.as_deref())
                        .map(put)
                        .transpose()?,
                    after: Some(put(text)?),
                }),
                Action::Trash => ops.push(JournalOp {
                    path: op.path.clone(),
                    before: preview
                        .before
                        .get(&op.path)
                        .and_then(|b| b.as_deref())
                        .map(put)
                        .transpose()?,
                    after: None,
                }),
                Action::TrashDir => {
                    for (path, text) in &preview.before {
                        if let (true, Some(text)) = (path.starts_with(&op.path), text) {
                            ops.push(JournalOp {
                                path: path.clone(),
                                before: Some(put(text)?),
                                after: None,
                            });
                        }
                    }
                    ops.push(JournalOp {
                        path: op.path.clone(),
                        before: None,
                        after: None,
                    });
                }
            }
            ends.push(ops.len());
        }
        Ok((ops, ends))
    }
}

/// What disk operations do, shared by the pipeline and its undo.
pub(crate) struct Effects<'a> {
    pub trasher: &'a dyn Trasher,
    /// Files under it are the tool's own: directories 0700, files 0600.
    pub data_dir: &'a AbsPath,
}

/// The hash of the bytes a path holds, following a symlink. `None` when nothing is there.
pub(crate) fn disk_hash(path: &AbsPath) -> std::io::Result<Option<String>> {
    match fs::read(write_target(path)) {
        Ok(bytes) => Ok(Some(sha256_hex(&bytes))),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err),
    }
}

impl Effects<'_> {
    /// `held`: the history holds every file of a directory the change trashes whole.
    fn run_op(
        &self,
        id: &str,
        index: usize,
        op: &FileOp,
        held: bool,
        files: &mut Vec<(AbsPath, Option<Revision>)>,
        not_trashed: &mut Vec<AbsPath>,
    ) -> Result<(), ApiError> {
        match &op.action {
            Action::Write { text } => {
                self.expect(op)?;
                self.write(&op.path, text.as_bytes(), &format!("{id}-{index}"))
                    .map_err(io_detail)?;
                files.push((op.path.clone(), Some(Revision::of(text.as_bytes()))));
            }
            // The plan holds the text of a file it trashes alone.
            Action::Trash => {
                self.expect(op)?;
                self.trash(&op.path, true, not_trashed).map_err(io_detail)?;
                files.push((op.path.clone(), None));
            }
            Action::TrashDir => {
                self.trash(&op.path, held, not_trashed).map_err(io_detail)?;
                files.push((op.path.clone(), None));
            }
        }
        Ok(())
    }

    /// Hashes the target again, right before the write (step 4). A mismatch means someone wrote
    /// between the plan and now, and the operation fails.
    fn expect(&self, op: &FileOp) -> Result<(), ApiError> {
        let now = disk_hash(&op.path).map_err(io_detail)?;
        let ok = match (&op.expect, &now) {
            (Base::Absent, None) => true,
            (Base::Rev { rev }, Some(hash)) => rev.0 == format!("sha256:{hash}"),
            _ => false,
        };
        if ok {
            return Ok(());
        }
        let current = fs::read_to_string(write_target(&op.path)).ok();
        Err(ApiError::Conflict {
            path: op.path.clone(),
            expected: op.expect.clone(),
            found: match &current {
                Some(text) => Base::Rev {
                    rev: Revision::of(text.as_bytes()),
                },
                None => Base::Absent,
            },
            current,
        })
    }

    /// Writes through a temp file in the directory of the target. A symlink is followed, so the
    /// link survives. The mode bits of an existing file are kept.
    pub fn write(&self, path: &AbsPath, bytes: &[u8], tag: &str) -> std::io::Result<()> {
        let target = write_target(path);
        // A symlink to nothing that appeared after the plan. The plan refuses one already there.
        if target.is_symlink() {
            return Err(std::io::ErrorKind::NotFound.into());
        }
        let own = path.starts_with(self.data_dir);
        if let Some(dir) = target.parent() {
            if own {
                create_private_dir(dir)?;
            } else {
                fs::create_dir_all(dir)?;
            }
        }
        let new_mode = if own { DATA_FILE_MODE } else { NEW_FILE_MODE };
        let mode = mode_of(&target).unwrap_or(new_mode);
        let tmp = target.with_file_name(temp_name(tag));
        atomic_write(&target, &tmp, bytes, mode)
    }

    /// Moves a file or directory to the OS trash. When the trash is not available, as on a
    /// headless Linux host, a path whose content the history `held` is removed and listed in
    /// `not_trashed`: the history blobs are the copy. Any other path stays, and the call fails.
    pub fn trash(
        &self,
        path: &AbsPath,
        held: bool,
        not_trashed: &mut Vec<AbsPath>,
    ) -> std::io::Result<()> {
        if fs::symlink_metadata(path.as_path()).is_err() {
            return Ok(());
        }
        match self.trasher.trash(path.as_path()) {
            Ok(()) => return Ok(()),
            Err(err) if !held => return Err(std::io::Error::other(err)),
            Err(_) => {}
        }
        let result = if path.as_path().is_dir() && !path.as_path().is_symlink() {
            fs::remove_dir_all(path.as_path())
        } else {
            fs::remove_file(path.as_path())
        };
        result?;
        not_trashed.push(path.clone());
        Ok(())
    }
}

/// `.cm-<change id>-<tag>.tmp`. Recovery finds leftovers of a change by this prefix.
pub(crate) fn temp_name(tag: &str) -> String {
    format!(".cm-{tag}.tmp")
}
