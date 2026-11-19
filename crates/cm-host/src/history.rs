//! The tool's own change history (design section 4, "What the tool owns on disk").
//!
//! ```text
//! history/changes/<ulid>.json           a sealed change
//! history/changes/<ulid>.pending.json   a change between journal and seal
//! history/blobs/<sha256>                the bytes of a file before or after a change
//! ```
//!
//! One file per change, so two instances never write the same history file. A blob is named by
//! the SHA-256 of its bytes, so storing it twice is harmless.

use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use cm_core::edit::ChangeSource;
use cm_core::{
    ApiError, ChangeDetail, ChangeFile, ChangeId, ChangeRecord, ChangeState, JournalOp, sha256_hex,
};

use crate::fsio::{create_private_dir, io_detail, write_data_file};

/// A blob that no record refers to is collected once it is this old (design section 4).
pub const BLOB_MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60);

pub struct History {
    changes: PathBuf,
    blobs: PathBuf,
}

impl History {
    /// `data_dir` is the tool's data directory. Nothing is created until the first write.
    pub fn open(data_dir: &std::path::Path) -> History {
        History {
            changes: data_dir.join("history/changes"),
            blobs: data_dir.join("history/blobs"),
        }
    }

    // -----------------------------------------------------------------------------------------
    // Writing
    // -----------------------------------------------------------------------------------------

    /// Stores `bytes` and returns its blob id. A blob already stored is left as it is.
    pub fn put_blob(&self, bytes: &[u8]) -> io::Result<String> {
        let id = sha256_hex(bytes);
        let path = self.blobs.join(&id);
        if !path.exists() {
            write_data_file(&path, bytes)?;
        }
        Ok(id)
    }

    pub fn blob(&self, id: &str) -> io::Result<Vec<u8>> {
        if id.is_empty() || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "bad blob id"));
        }
        fs::read(self.blobs.join(id))
    }

    pub fn write_pending(&self, record: &ChangeRecord) -> io::Result<()> {
        self.write_record(record, true)
    }

    /// Closes a change whose operations are all on disk.
    pub fn seal(&self, record: &mut ChangeRecord) -> io::Result<()> {
        self.close(record, ChangeState::Sealed)
    }

    /// Closes a change none of whose operations is on disk (design section 4).
    pub fn abort(&self, record: &mut ChangeRecord) -> io::Result<()> {
        self.close(record, ChangeState::Aborted)
    }

    /// Writes `<id>.json` in its final state, then removes `<id>.pending.json`. A crash between
    /// the two leaves both files, and the closed one wins.
    fn close(&self, record: &mut ChangeRecord, state: ChangeState) -> io::Result<()> {
        record.state = state;
        self.write_record(record, false)?;
        match fs::remove_file(self.path_of(&record.id, true)) {
            Err(err) if err.kind() != io::ErrorKind::NotFound => Err(err),
            _ => Ok(()),
        }
    }

    fn write_record(&self, record: &ChangeRecord, pending: bool) -> io::Result<()> {
        create_private_dir(&self.changes)?;
        let mut text = serde_json::to_string_pretty(record).expect("a record serializes");
        text.push('\n');
        write_data_file(&self.path_of(&record.id, pending), text.as_bytes())
    }

    fn path_of(&self, id: &ChangeId, pending: bool) -> PathBuf {
        let suffix = if pending { ".pending.json" } else { ".json" };
        self.changes.join(format!("{}{suffix}", id.0))
    }

    // -----------------------------------------------------------------------------------------
    // Reading
    // -----------------------------------------------------------------------------------------

    /// Every record, newest first. A closed record hides a pending one with the same id. A file
    /// that does not parse is skipped.
    pub fn all(&self) -> Vec<ChangeRecord> {
        let Ok(entries) = fs::read_dir(&self.changes) else {
            return Vec::new();
        };
        let mut by_id: Vec<(String, bool, ChangeRecord)> = Vec::new();
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let (stem, pending) = match name.strip_suffix(".pending.json") {
                Some(stem) => (stem.to_owned(), true),
                None => match name.strip_suffix(".json") {
                    Some(stem) if !stem.starts_with('.') => (stem.to_owned(), false),
                    _ => continue,
                },
            };
            let Ok(bytes) = fs::read(entry.path()) else {
                continue;
            };
            let Ok(record) = serde_json::from_slice::<ChangeRecord>(&bytes) else {
                continue;
            };
            by_id.push((stem, pending, record));
        }
        let closed: HashSet<String> = by_id
            .iter()
            .filter(|(_, pending, _)| !pending)
            .map(|(id, _, _)| id.clone())
            .collect();
        let mut records: Vec<ChangeRecord> = by_id
            .into_iter()
            .filter(|(id, pending, _)| !*pending || !closed.contains(id))
            .map(|(_, _, record)| record)
            .collect();
        records.sort_by(|a, b| b.id.cmp(&a.id));
        records
    }

    pub fn record(&self, id: &ChangeId) -> Option<ChangeRecord> {
        self.all().into_iter().find(|r| r.id == *id)
    }

    /// The records not yet closed, oldest first.
    pub fn pending(&self) -> Vec<ChangeRecord> {
        let mut pending: Vec<ChangeRecord> = self
            .all()
            .into_iter()
            .filter(|r| matches!(r.state, ChangeState::Pending | ChangeState::NeedsAttention))
            .collect();
        pending.reverse();
        pending
    }

    /// Removes `<id>.pending.json` files whose closed twin exists.
    pub fn drop_closed_twins(&self) {
        let Ok(entries) = fs::read_dir(&self.changes) else {
            return;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if let Some(stem) = name.strip_suffix(".pending.json")
                && self.changes.join(format!("{stem}.json")).exists()
            {
                let _ = fs::remove_file(entry.path());
            }
        }
    }

    /// The text of a blob. A blob that is missing or not UTF-8 is an error, because reading it as
    /// "absent" would make a revert delete a file.
    fn text_of(&self, id: &Option<String>) -> Result<Option<String>, ApiError> {
        let Some(id) = id else { return Ok(None) };
        let bytes = self.blob(id).map_err(io_detail)?;
        String::from_utf8(bytes)
            .map(Some)
            .map_err(|_| ApiError::Io {
                detail: format!("blob {id} is not UTF-8"),
            })
    }

    /// The record with the text of each file before and after. An operation with neither (a
    /// directory that went whole to the trash) is left out: it has no text to show or restore.
    pub fn detail_of(&self, record: ChangeRecord) -> Result<ChangeDetail, ApiError> {
        let mut files = Vec::new();
        for op in &record.ops {
            if op.before.is_none() && op.after.is_none() {
                continue;
            }
            files.push(ChangeFile {
                path: op.path.clone(),
                before: self.text_of(&op.before)?,
                after: self.text_of(&op.after)?,
            });
        }
        Ok(ChangeDetail { record, files })
    }

    // -----------------------------------------------------------------------------------------
    // Collecting
    // -----------------------------------------------------------------------------------------

    /// Removes blobs that no record refers to and that are older than `min_age`. A pending
    /// record counts as a reference. Returns how many it removed.
    pub fn collect_garbage(&self, min_age: Duration) -> usize {
        let Ok(entries) = fs::read_dir(&self.blobs) else {
            return 0;
        };
        let mut referenced: HashSet<String> = HashSet::new();
        for record in self.all() {
            for JournalOp { before, after, .. } in record.ops {
                referenced.extend(before);
                referenced.extend(after);
            }
        }
        let now = SystemTime::now();
        let mut removed = 0;
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if referenced.contains(&name) {
                continue;
            }
            let old = entry
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|modified| now.duration_since(modified).ok())
                .is_some_and(|age| age >= min_age);
            if old && fs::remove_file(entry.path()).is_ok() {
                removed += 1;
            }
        }
        removed
    }
}

impl ChangeSource for History {
    fn detail(&self, id: &ChangeId) -> Result<ChangeDetail, ApiError> {
        let record = self.record(id).ok_or(ApiError::NotFound)?;
        self.detail_of(record)
    }
}

// ---------------------------------------------------------------------------------------------
// Ids and time
// ---------------------------------------------------------------------------------------------

const CROCKFORD: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
static LAST_ID: Mutex<u128> = Mutex::new(0);

/// A ULID: 48 bits of milliseconds and 80 random bits, so ids sort by time. Two ids made in one
/// process never go backwards, even within one millisecond.
pub fn new_change_id() -> ChangeId {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis());
    let mut random = [0u8; 10];
    getrandom::fill(&mut random).expect("the operating system provides random bytes");
    let tail = random
        .iter()
        .fold(0u128, |acc, b| (acc << 8) | u128::from(*b));
    let mut value = (millis << 80) | tail;
    let mut last = LAST_ID.lock().expect("id state");
    if value <= *last {
        value = *last + 1;
    }
    *last = value;
    let mut chars = [0u8; 26];
    for (i, slot) in chars.iter_mut().enumerate() {
        let shift = 5 * (25 - i);
        *slot = CROCKFORD[((value >> shift) & 0x1f) as usize];
    }
    ChangeId(String::from_utf8_lossy(&chars).into_owned())
}

/// The current time as `2026-11-20T09:30:00Z`.
pub fn now_iso() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64);
    iso_of(secs)
}

fn iso_of(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    // Civil date from a day count (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use cm_core::{AbsPath, ChangeEdit, ChangeSummary};

    fn record(id: &ChangeId, cwd: Option<&str>, ops: Vec<JournalOp>) -> ChangeRecord {
        ChangeRecord {
            id: id.clone(),
            at: now_iso(),
            cwd: cwd.map(|c| AbsPath::parse(c).unwrap()),
            summary: ChangeSummary {
                edit: ChangeEdit::Save,
                agent: None,
                targets: Vec::new(),
            },
            ops,
            state: ChangeState::Pending,
        }
    }

    #[test]
    fn ids_sort_by_creation_order_and_are_26_characters() {
        let ids: Vec<_> = (0..50).map(|_| new_change_id()).collect();
        assert!(ids.iter().all(|id| id.0.len() == 26));
        assert!(ids.windows(2).all(|w| w[0] < w[1]));
    }

    #[test]
    fn the_iso_time_of_a_known_instant() {
        assert_eq!(iso_of(0), "1970-01-01T00:00:00Z");
        assert_eq!(iso_of(1_782_000_000), "2026-06-21T00:00:00Z");
    }

    #[test]
    fn a_sealed_record_hides_its_pending_twin_and_lists_newest_first() {
        let dir = tempfile::tempdir().unwrap();
        let history = History::open(dir.path());
        let (a, b) = (new_change_id(), new_change_id());
        let mut first = record(&a, Some("/p"), Vec::new());
        history.write_pending(&first).unwrap();
        history
            .write_pending(&record(&b, None, Vec::new()))
            .unwrap();
        assert_eq!(history.pending().len(), 2);

        history.seal(&mut first).unwrap();
        let ids: Vec<_> = history.all().into_iter().map(|r| r.id).collect();
        assert_eq!(ids, vec![b.clone(), a.clone()]);
        assert_eq!(history.pending().len(), 1);

        // A crash between the two writes of a seal leaves both files. The sealed one wins.
        history.write_pending(&first).unwrap();
        assert_eq!(history.all().len(), 2);
        assert_eq!(history.record(&a).unwrap().state, ChangeState::Sealed);
        history.drop_closed_twins();
        assert!(
            !dir.path()
                .join(format!("history/changes/{}.pending.json", a.0))
                .exists()
        );
    }

    #[test]
    fn detail_reads_before_and_after_text_and_refuses_a_missing_blob() {
        let dir = tempfile::tempdir().unwrap();
        let history = History::open(dir.path());
        let before = history.put_blob(b"old").unwrap();
        let after = history.put_blob(b"new").unwrap();
        assert_eq!(history.put_blob(b"old").unwrap(), before);
        let id = new_change_id();
        let path = AbsPath::parse("/p/a.md").unwrap();
        let op = JournalOp {
            path: path.clone(),
            before: Some(before),
            after: Some(after),
        };
        let mut rec = record(&id, None, vec![op]);
        history.seal(&mut rec).unwrap();
        let detail = history.detail(&id).unwrap();
        assert_eq!(detail.files[0].before.as_deref(), Some("old"));
        assert_eq!(detail.files[0].after.as_deref(), Some("new"));
        assert_eq!(history.detail(&new_change_id()), Err(ApiError::NotFound));

        let broken = new_change_id();
        let mut rec = record(
            &broken,
            None,
            vec![JournalOp {
                path,
                before: Some("0".repeat(64)),
                after: None,
            }],
        );
        history.seal(&mut rec).unwrap();
        assert!(matches!(history.detail(&broken), Err(ApiError::Io { .. })));
    }

    #[test]
    fn the_collector_keeps_referenced_and_young_blobs_only() {
        let dir = tempfile::tempdir().unwrap();
        let history = History::open(dir.path());
        let kept = history.put_blob(b"referenced").unwrap();
        let orphan = history.put_blob(b"orphan").unwrap();
        let id = new_change_id();
        let mut rec = record(
            &id,
            None,
            vec![JournalOp {
                path: AbsPath::parse("/p/a").unwrap(),
                before: Some(kept.clone()),
                after: None,
            }],
        );
        history.write_pending(&rec).unwrap();

        assert_eq!(
            history.collect_garbage(BLOB_MAX_AGE),
            0,
            "the orphan is young"
        );
        assert_eq!(history.collect_garbage(Duration::ZERO), 1);
        assert!(history.blob(&kept).is_ok());
        assert!(history.blob(&orphan).is_err());
        history.seal(&mut rec).unwrap();
    }
}
