//! The trace operations (design section 14) and the memo of the two traces opened last.
//!
//! The memo is the one exception to "the host keeps no state" (design section 2). It holds a pure
//! function of a file, keyed by the request's `cwd` and `TraceKey`, and is checked against the
//! file's length and modification time on every use. The file in an entry came from the
//! adapter's own listing, never from the client.

use std::sync::{Arc, Mutex};

use cm_core::agents::{Agent, ScanCx};
use cm_core::env::ScanEnv;
use cm_core::probe_fs::ProbeFs;
use cm_core::trace::OpenTrace;
use cm_core::*;

/// How many open traces the host keeps.
const MEMO_SIZE: usize = 2;

struct Entry {
    cwd: AbsPath,
    key: TraceKey,
    file: AbsPath,
    /// The file's length when the adapter started reading it.
    len: u64,
    /// The file's modification time once the read ended.
    mtime_ms: i64,
    open: Arc<OpenTrace>,
}

#[derive(Default)]
pub(crate) struct TraceMemo {
    /// Most recently used last.
    entries: Mutex<Vec<Entry>>,
}

/// The file's length and modification time now, or `None` when it cannot be read.
fn stamp(file: &AbsPath) -> Option<(u64, i64)> {
    let stat = ProbeFs::new().stat(file).ok()?;
    Some((stat.len, stat.mtime_ms))
}

impl TraceMemo {
    /// The open trace of `key`, from the memo when the file is unchanged, else read again.
    pub(crate) fn open(
        &self,
        agents: &'static [&'static dyn Agent],
        env: &ScanEnv,
        key: &TraceKey,
    ) -> Result<Arc<OpenTrace>, ApiError> {
        if let Some(open) = self.hit(&env.cwd, key) {
            return Ok(open);
        }
        let agent = agents
            .iter()
            .copied()
            .find(|agent| agent.id() == key.agent)
            .ok_or(ApiError::NotFound)?;
        let fs = ProbeFs::new();
        let cx = ScanCx {
            env,
            fs: &fs,
            parked_dir: None,
        };
        let data = agent.trace(&cx, &key.id, key.part.as_deref())?;
        let file = data.info.file.clone();
        let len = data.info.bytes;
        let open = Arc::new(OpenTrace::new(data));
        if let Some((_, mtime_ms)) = stamp(&file) {
            let mut entries = self.entries.lock().expect("trace memo");
            entries.retain(|e| !(e.cwd == env.cwd && e.key == *key));
            entries.push(Entry {
                cwd: env.cwd.clone(),
                key: key.clone(),
                file,
                len,
                mtime_ms,
                open: open.clone(),
            });
            if entries.len() > MEMO_SIZE {
                entries.remove(0);
            }
        }
        Ok(open)
    }

    fn hit(&self, cwd: &AbsPath, key: &TraceKey) -> Option<Arc<OpenTrace>> {
        let mut entries = self.entries.lock().expect("trace memo");
        let at = entries
            .iter()
            .position(|e| e.cwd == *cwd && e.key == *key)?;
        let entry = entries.remove(at);
        if stamp(&entry.file) != Some((entry.len, entry.mtime_ms)) {
            return None;
        }
        let open = entry.open.clone();
        entries.push(entry);
        Some(open)
    }
}

/// Every agent's sessions that started in `env.cwd` or below it, newest first.
pub(crate) fn traces(agents: &'static [&'static dyn Agent], env: &ScanEnv) -> Vec<TraceInfo> {
    let fs = ProbeFs::new();
    let cx = ScanCx {
        env,
        fs: &fs,
        parked_dir: None,
    };
    let mut all: Vec<TraceInfo> = agents.iter().flat_map(|a| a.traces(&cx)).collect();
    all.sort_by(|a, b| {
        let at = |info: &TraceInfo| info.last.clone().or_else(|| info.started.clone());
        at(b).cmp(&at(a)).then_with(|| a.id.cmp(&b.id))
    });
    all
}

pub(crate) fn step_pieces(open: &OpenTrace, read: u64, step: u32) -> Result<StepPieces, ApiError> {
    if open.read() != read {
        return Ok(StepPieces::Stale);
    }
    open.step_pieces(step)
        .map(|pieces| StepPieces::Current { pieces })
        .ok_or(ApiError::NotFound)
}

pub(crate) fn piece_text(
    open: &OpenTrace,
    read: u64,
    step: u32,
    piece: PieceRef,
) -> Result<PieceText, ApiError> {
    if open.read() != read {
        return Ok(PieceText::Stale);
    }
    open.piece_text(step, piece).ok_or(ApiError::NotFound)
}
