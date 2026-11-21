//! The trace operations (design section 14) and the memo of the two traces opened last.
//!
//! The memo is the one exception to "the host keeps no state" (design section 2). A record only
//! grows by appending, so the trace of its first `read` bytes stays true while it grows. An entry
//! holds that pure function of a prefix, keyed by the request's `cwd`, its `TraceKey` and `read`.
//! The file in an entry came from the adapter's own listing, never from the client.

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
    /// For a trace of the whole record, the file's modification time once the read ended.
    /// `None` for a trace of a prefix.
    whole: Option<i64>,
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

fn find_agent(
    agents: &'static [&'static dyn Agent],
    key: &TraceKey,
) -> Result<&'static dyn Agent, ApiError> {
    agents
        .iter()
        .copied()
        .find(|agent| agent.id() == key.agent)
        .ok_or(ApiError::NotFound)
}

impl TraceMemo {
    /// The trace of the record as it is now (`trace`). From the memo when the record has not
    /// changed since the memo read all of it. A new trace replaces every older one of the record.
    pub(crate) fn current(
        &self,
        agents: &'static [&'static dyn Agent],
        env: &ScanEnv,
        key: &TraceKey,
    ) -> Result<Arc<OpenTrace>, ApiError> {
        let unchanged = |e: &Entry| {
            e.whole
                .is_some_and(|mtime| stamp(&e.file) == Some((e.open.read(), mtime)))
        };
        if let Some(open) = self.hit(&env.cwd, key, unchanged) {
            return Ok(open);
        }
        let agent = find_agent(agents, key)?;
        let fs = ProbeFs::new();
        let data = agent.trace(&ScanCx::new(env, &fs), &key.id, key.part.as_deref(), None)?;
        let open = Arc::new(OpenTrace::new(data));
        if let Some((_, mtime)) = stamp(&open.trace().info.file) {
            self.insert(env, key, Some(mtime), open.clone());
        }
        Ok(open)
    }

    /// The trace of the record's first `read` bytes (`trace_step`, `trace_piece`), also when the
    /// record has grown since. From the memo when it holds that trace, else read again. `None`
    /// when the record is now shorter than `read`, or is gone.
    pub(crate) fn prefix(
        &self,
        agents: &'static [&'static dyn Agent],
        env: &ScanEnv,
        key: &TraceKey,
        read: u64,
    ) -> Result<Option<Arc<OpenTrace>>, ApiError> {
        let at_read = |e: &Entry| e.open.read() == read;
        if let Some(open) = self.hit(&env.cwd, key, at_read) {
            let long_enough = stamp(&open.trace().info.file).is_some_and(|(len, _)| len >= read);
            return Ok(long_enough.then_some(open));
        }
        let agent = find_agent(agents, key)?;
        let fs = ProbeFs::new();
        let cx = ScanCx::new(env, &fs);
        let data = match agent.trace(&cx, &key.id, key.part.as_deref(), Some(read)) {
            Ok(data) => data,
            Err(ApiError::NotFound) => return Ok(None),
            Err(err) => return Err(err),
        };
        if data.read != read {
            return Ok(None);
        }
        let open = Arc::new(OpenTrace::new(data));
        self.insert(env, key, None, open.clone());
        Ok(Some(open))
    }

    /// The most recent entry of `key` that `fits`, moved to the end.
    fn hit(
        &self,
        cwd: &AbsPath,
        key: &TraceKey,
        fits: impl Fn(&Entry) -> bool,
    ) -> Option<Arc<OpenTrace>> {
        let mut entries = self.entries.lock().expect("trace memo");
        let at = entries
            .iter()
            .rposition(|e| e.cwd == *cwd && e.key == *key && fits(e))?;
        let entry = entries.remove(at);
        let open = entry.open.clone();
        entries.push(entry);
        Some(open)
    }

    /// Stores a trace. A trace of the whole record replaces every entry of the record; a trace
    /// of a prefix replaces the entry of that prefix.
    fn insert(&self, env: &ScanEnv, key: &TraceKey, whole: Option<i64>, open: Arc<OpenTrace>) {
        let mut entries = self.entries.lock().expect("trace memo");
        let read = open.read();
        entries.retain(|e| {
            !(e.cwd == env.cwd && e.key == *key && (whole.is_some() || e.open.read() == read))
        });
        entries.push(Entry {
            cwd: env.cwd.clone(),
            key: key.clone(),
            file: open.trace().info.file.clone(),
            whole,
            open,
        });
        if entries.len() > MEMO_SIZE {
            entries.remove(0);
        }
    }
}

/// Every agent's sessions that started in `env.cwd` or below it, newest first.
pub(crate) fn traces(agents: &'static [&'static dyn Agent], env: &ScanEnv) -> Vec<TraceInfo> {
    let fs = ProbeFs::new();
    let cx = ScanCx::new(env, &fs);
    let mut all: Vec<TraceInfo> = agents.iter().flat_map(|a| a.traces(&cx)).collect();
    all.sort_by(|a, b| {
        let at = |info: &TraceInfo| info.last.clone().or_else(|| info.started.clone());
        at(b).cmp(&at(a)).then_with(|| a.id.cmp(&b.id))
    });
    all
}

pub(crate) fn step_pieces(open: Option<&OpenTrace>, step: u32) -> Result<StepPieces, ApiError> {
    let Some(open) = open else {
        return Ok(StepPieces::Stale);
    };
    open.step_pieces(step)
        .map(|pieces| StepPieces::Current { pieces })
        .ok_or(ApiError::NotFound)
}

pub(crate) fn piece_text(
    open: Option<&OpenTrace>,
    step: u32,
    piece: PieceRef,
) -> Result<PieceText, ApiError> {
    let Some(open) = open else {
        return Ok(PieceText::Stale);
    };
    open.piece_text(step, piece).ok_or(ApiError::NotFound)
}
