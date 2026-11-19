//! Recovery (design section 4): finishes or flags each change that was journaled and not sealed.
//!
//! For each pending record it classifies every operation by the hash on disk: at the before
//! state, at the after state, or other.
//!
//! - No operation is "other": the user asked for this change, so the operations still at before
//!   are applied from the stored blobs and the record is sealed.
//! - Some operation is "other": someone edited that file in between. The record stays pending,
//!   its state becomes `NeedsAttention`, and the report names the file.
//!
//! The decision is a function of the disk, so a second run changes nothing, and two processes
//! that recover one record write the same bytes. Recovery runs when the host starts and when the
//! history is read. In a read-only host it classifies and reports and writes nothing.

use std::fs;

use cm_core::{AbsPath, ChangeId, ChangeRecord, ChangeState, JournalOp};

use crate::apply::{Ctx, PIPELINE, disk_hash};
use crate::fsio::write_target;
use crate::history::BLOB_MAX_AGE;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpState {
    AtBefore,
    AtAfter,
    Other,
}

/// What a recovery run did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Report {
    /// Records rolled forward and sealed.
    pub sealed: Vec<ChangeId>,
    /// Records left pending, with the files that were edited in between.
    pub attention: Vec<(ChangeId, Vec<AbsPath>)>,
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

pub(crate) fn recover(ctx: &Ctx, roll_forward: bool) -> Report {
    let _serialized = PIPELINE.lock().unwrap_or_else(|e| e.into_inner());
    let mut report = Report::default();
    if roll_forward {
        ctx.history.drop_sealed_twins();
    }
    for mut record in ctx.history.pending() {
        let states = inspect(&record);
        let others: Vec<AbsPath> = states
            .iter()
            .filter(|(_, state)| *state == OpState::Other)
            .map(|(path, _)| path.clone())
            .collect();
        if !others.is_empty() {
            if roll_forward && record.state != ChangeState::NeedsAttention {
                record.state = ChangeState::NeedsAttention;
                let _ = ctx.history.write_pending(&record);
            }
            report.attention.push((record.id.clone(), others));
            continue;
        }
        if !roll_forward {
            continue;
        }
        remove_temp_files(&record);
        if roll_record_forward(ctx, &record).is_ok() && ctx.history.seal(&mut record).is_ok() {
            report.sealed.push(record.id.clone());
        }
    }
    if roll_forward {
        ctx.history.collect_garbage(BLOB_MAX_AGE);
    }
    report
}

/// Applies the operations still at before, from the blobs the journal stored.
fn roll_record_forward(ctx: &Ctx, record: &ChangeRecord) -> std::io::Result<()> {
    let effects = ctx.effects();
    let mut not_trashed = Vec::new();
    let directories: Vec<&AbsPath> = record
        .ops
        .iter()
        .filter(|op| is_directory_op(op) && classify(op) == OpState::AtBefore)
        .map(|op| &op.path)
        .collect();
    for dir in &directories {
        effects.trash(dir, &mut not_trashed)?;
    }
    for (index, op) in record.ops.iter().enumerate() {
        if is_directory_op(op) || directories.iter().any(|dir| op.path.starts_with(dir)) {
            continue;
        }
        if classify(op) != OpState::AtBefore {
            continue;
        }
        match &op.after {
            Some(blob) => {
                let bytes = ctx.history.blob(blob)?;
                effects.write(&op.path, &bytes, &format!("{}-r{index}", record.id.0))?;
            }
            None => effects.trash(&op.path, &mut not_trashed)?,
        }
    }
    Ok(())
}
