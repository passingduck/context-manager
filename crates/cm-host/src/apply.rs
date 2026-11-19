//! The apply pipeline (design section 4, steps 1 to 5) and the file effects it shares with
//! recovery.
//!
//! 1. Policy: refuse when the host is read only. The host does this in one place.
//! 2. Plan again against a fresh scan. An empty plan is `AlreadyApplied`. A digest that differs
//!    from the one the client saw is `Stale`.
//! 3. Journal: store the before and after bytes of each file as blobs, then write the pending
//!    record.
//! 4. Execute each operation in order.
//! 5. Seal: write the sealed record and remove the pending one.
//!
//! One process-wide mutex serializes the whole run.

use std::fs;
use std::path::Path;
use std::sync::Mutex;

use cm_core::agents::Agent;
use cm_core::edit::Planner;
use cm_core::env::ScanEnv;
use cm_core::scan::scan_with;
use cm_core::{
    AbsPath, Action, ApiError, ApplyResult, Base, ChangeRecord, ChangeState, Edit, FileOp,
    JournalOp, Preview, Revision, sha256_hex,
};

use crate::fsio::{
    DATA_FILE_MODE, NEW_FILE_MODE, atomic_write, create_private_dir, io_detail, mode_of,
    write_target,
};
use crate::history::{History, new_change_id, now_iso};

/// Serializes `apply` and recovery within this process.
pub(crate) static PIPELINE: Mutex<()> = Mutex::new(());

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
    pub fn plan(&self, cwd: &AbsPath, edit: &Edit) -> Result<Preview, ApiError> {
        let env = self.env.with_cwd(cwd.clone());
        let scanned = scan_with(self.agents, &env);
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
        let now = self.plan(cwd, edit)?;
        if let Some(conflict) = now.conflicts.first() {
            return Err(ApiError::Conflict {
                path: conflict.path.clone(),
                expected: conflict.expected.clone(),
                found: conflict.found.clone(),
                current: conflict.current.clone(),
            });
        }
        if now.change_set.ops.is_empty() {
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
        let mut record = ChangeRecord {
            id: id.clone(),
            at: now_iso(),
            cwd: Some(cwd.clone()),
            summary: preview.change_set.summary.clone(),
            ops: self.journal(preview)?,
            state: ChangeState::Pending,
        };
        checkpoint(Step::Blobs)?;
        self.history.write_pending(&record).map_err(io_detail)?;
        checkpoint(Step::Pending)?;

        // Step 4: execute.
        let effects = self.effects();
        let mut files = Vec::new();
        let mut not_trashed = Vec::new();
        for (index, op) in preview.change_set.ops.iter().enumerate() {
            effects.run_op(&id.0, index, op, &mut files, &mut not_trashed)?;
            checkpoint(Step::Op(index))?;
        }
        checkpoint(Step::Executed)?;

        // Step 5: seal. A revert of a record that never finished resolves it first, so that
        // recovery does not roll it forward again over the revert.
        if let Edit::Revert { change } = edit
            && let Some(mut target) = self.history.record(change)
            && target.state != ChangeState::Sealed
        {
            self.history.seal(&mut target).map_err(io_detail)?;
        }
        self.history.seal(&mut record).map_err(io_detail)?;
        checkpoint(Step::Sealed)?;
        Ok(ApplyResult::Applied {
            change: id,
            files,
            not_trashed,
        })
    }

    pub(crate) fn effects(&self) -> Effects<'_> {
        Effects {
            trasher: self.trasher.as_ref(),
            data_dir: &self.data_dir,
        }
    }

    /// Stores the blobs and builds the journal. A directory that goes to the trash whole has one
    /// operation per file it holds, and a last one for the directory itself.
    fn journal(&self, preview: &Preview) -> Result<Vec<JournalOp>, ApiError> {
        let put = |text: &str| self.history.put_blob(text.as_bytes()).map_err(io_detail);
        let mut ops = Vec::new();
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
        }
        Ok(ops)
    }
}

/// What disk operations do, shared by the pipeline and recovery.
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
    fn run_op(
        &self,
        id: &str,
        index: usize,
        op: &FileOp,
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
            Action::Trash => {
                self.expect(op)?;
                self.trash(&op.path, not_trashed).map_err(io_detail)?;
                files.push((op.path.clone(), None));
            }
            Action::TrashDir => {
                self.trash(&op.path, not_trashed).map_err(io_detail)?;
                files.push((op.path.clone(), None));
            }
        }
        Ok(())
    }

    /// Hashes the target again, right before the write (step 4). A mismatch means someone wrote
    /// between the plan and now, and the record stays pending for recovery to classify.
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
    /// headless Linux host, the path is removed and listed in `not_trashed`. The history blob is
    /// then the copy.
    pub fn trash(&self, path: &AbsPath, not_trashed: &mut Vec<AbsPath>) -> std::io::Result<()> {
        if fs::symlink_metadata(path.as_path()).is_err() {
            return Ok(());
        }
        if self.trasher.trash(path.as_path()).is_ok() {
            return Ok(());
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
