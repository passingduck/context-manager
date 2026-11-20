//! Recovery (design section 4): classifies each change that was journaled and not closed. It
//! never writes an agent file.
//!
//! For each pending record it classifies every operation by the hash on disk: at the before
//! state, at the after state, or other.
//!
//! - Every operation at after: the whole change is on disk, and the record is sealed.
//! - Every operation at before: nothing of the change is on disk, and the record is aborted.
//! - Anything else: the record becomes `NeedsAttention`, and the user reverts it.
//!
//! The decision is a function of the disk, so a second run changes nothing. Recovery runs when
//! the host starts and when the history is read. In a read-only host it classifies and reports
//! and writes nothing.
//!
//! Recovery takes the lock of the apply pipeline without waiting. While a process holds it, that
//! process is applying a change: its record is pending and its temp files are in use, so the
//! round leaves every record and temp file alone.

use std::fs;

use cm_core::{AbsPath, ChangeId, ChangeRecord, ChangeState, JournalOp};

use crate::apply::{Ctx, disk_hash};
use crate::fsio::write_target;
use crate::history::BLOB_MAX_AGE;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpState {
    AtBefore,
    AtAfter,
    Other,
}

/// What a recovery run found.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Report {
    /// Records whose operations are all at after.
    pub sealed: Vec<ChangeId>,
    /// Records whose operations are all at before.
    pub aborted: Vec<ChangeId>,
    /// Records left for the user, with the state of each file.
    pub attention: Vec<(ChangeId, Vec<(AbsPath, OpState)>)>,
}

/// An operation with neither a before nor an after blob stands for a directory that went to the
/// trash whole. It is at before while the directory exists.
fn is_directory_op(op: &JournalOp) -> bool {
    op.before.is_none() && op.after.is_none()
}

pub fn classify(op: &JournalOp) -> OpState {
    if is_directory_op(op) {
        return if fs::symlink_metadata(op.path.as_path()).is_ok() {
            OpState::AtBefore
        } else {
            OpState::AtAfter
        };
    }
    match disk_hash(&op.path) {
        Ok(now) if now == op.after => OpState::AtAfter,
        Ok(now) if now == op.before => OpState::AtBefore,
        _ => OpState::Other,
    }
}

pub fn inspect(record: &ChangeRecord) -> Vec<(AbsPath, OpState)> {
    record
        .ops
        .iter()
        .map(|op| (op.path.clone(), classify(op)))
        .collect()
}

/// Removes the temp files a crashed run of `record` left next to its targets.
fn remove_temp_files(record: &ChangeRecord) {
    let prefix = format!(".cm-{}-", record.id.0);
    for op in &record.ops {
        let Some(dir) = write_target(&op.path).parent().map(|d| d.to_path_buf()) else {
            continue;
        };
        let Ok(entries) = fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with(&prefix) && name.ends_with(".tmp") {
                let _ = fs::remove_file(entry.path());
            }
        }
    }
}

/// `write` is false in a read-only host: the records are classified and left as they are.
pub(crate) fn recover(ctx: &Ctx, write: bool) -> Report {
    let mut report = Report::default();
    // Held to the end of the round. A read-only round writes nothing, so it takes no lock.
    let _lock = match write.then(|| ctx.lock_file()) {
        None => None,
        Some(Ok(file)) if file.try_lock().is_ok() => Some(file),
        Some(_) => return report,
    };
    if write {
        ctx.history.drop_closed_twins();
    }
    for mut record in ctx.history.pending() {
        let states = inspect(&record);
        let all = |wanted: OpState| states.iter().all(|(_, state)| *state == wanted);
        if write {
            remove_temp_files(&record);
        }
        if all(OpState::AtAfter) {
            if !write || ctx.history.seal(&mut record).is_ok() {
                report.sealed.push(record.id.clone());
            }
        } else if all(OpState::AtBefore) {
            if !write || ctx.history.abort(&mut record).is_ok() {
                report.aborted.push(record.id.clone());
            }
        } else {
            if write && record.state != ChangeState::NeedsAttention {
                record.state = ChangeState::NeedsAttention;
                let _ = ctx.history.write_pending(&record);
            }
            report.attention.push((record.id.clone(), states));
        }
    }
    if write {
        ctx.history.collect_garbage(BLOB_MAX_AGE);
    }
    report
}
