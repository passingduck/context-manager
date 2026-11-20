//! `ProbeFs`: the only way an adapter reads the disk.
//!
//! `read` returns the bytes of a regular file of at most [`MAX_READ_BYTES`] and a refusal for
//! anything else, so no caller checks the kind or the size itself (design section 5). It counts
//! the bytes it reads and refuses every file once a scan has read [`MAX_SCAN_READ_BYTES`]. It
//! caches bytes for the length of one scan, so two agents that read `AGENTS.md` see one revision.
//! It records every path read, listed, checked or resolved, misses included. The fingerprint
//! hashes the stat of exactly those paths.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::io::{self, Read};
use std::sync::{Arc, Mutex};
use std::time::UNIX_EPOCH;

use sha2::{Digest, Sha256};

use crate::model::{AbsPath, Fingerprint};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    File,
    Dir,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stat {
    pub kind: EntryKind,
    pub is_symlink: bool,
    pub len: u64,
    pub mtime_ms: i64,
    /// Any execute bit is set.
    pub executable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirEntry {
    pub name: String,
    pub kind: EntryKind,
}

/// `read` refuses a larger file. A file is listed with its size and never read.
pub const MAX_READ_BYTES: u64 = 8 * 1024 * 1024;

/// `read` refuses every file once one scan has read this many bytes. The cache holds what a scan
/// read until the next scan, so this also bounds its memory. A scan of this repository with the
/// development machine's full agent homes reads about 15 MB, most of it for one Codex plugin
/// hash; links make a repository of one file and many names read that file once per name.
pub const MAX_SCAN_READ_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeErrorKind {
    Io(io::ErrorKind),
    /// `read` met a directory, a device, a named pipe or a socket, and did not open it.
    NotRegular,
    /// `read` met a regular file over [`MAX_READ_BYTES`].
    TooLarge {
        bytes: u64,
    },
    /// The scan had read [`MAX_SCAN_READ_BYTES`], so `read` did not open the file.
    BudgetSpent,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeError {
    pub kind: ProbeErrorKind,
    pub detail: String,
}

impl ProbeError {
    pub fn is_not_found(&self) -> bool {
        self.kind == ProbeErrorKind::Io(io::ErrorKind::NotFound)
    }

    fn not_regular() -> ProbeError {
        ProbeError {
            kind: ProbeErrorKind::NotRegular,
            detail: "not a regular file".to_owned(),
        }
    }

    fn too_large(bytes: u64) -> ProbeError {
        ProbeError {
            kind: ProbeErrorKind::TooLarge { bytes },
            detail: "file is too big".to_owned(),
        }
    }

    fn budget_spent() -> ProbeError {
        ProbeError {
            kind: ProbeErrorKind::BudgetSpent,
            detail: "the scan read budget is spent".to_owned(),
        }
    }
}

impl From<io::Error> for ProbeError {
    fn from(err: io::Error) -> ProbeError {
        ProbeError {
            kind: ProbeErrorKind::Io(err.kind()),
            detail: err.to_string(),
        }
    }
}

/// One path a scan looked at. A probe is a hit when the path exists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Probe {
    pub path: AbsPath,
    pub hit: bool,
}

/// A budget that one scan ran out of. The scan reports each as a diagnostic.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Exceeded {
    /// `read` refused files once the scan had read `max` bytes.
    Reads { max: u64 },
    /// A tree listing of `dir` stopped at its entry budget.
    ListingEntries { dir: AbsPath, max: u32 },
    /// A tree listing of `dir` did not list directories deeper than `max`.
    ListingDepth { dir: AbsPath, max: u32 },
}

type Cached<T> = Result<T, ProbeError>;

#[derive(Default)]
struct State {
    reads: HashMap<AbsPath, Cached<Arc<Vec<u8>>>>,
    lists: HashMap<AbsPath, Cached<Arc<Vec<DirEntry>>>>,
    stats: HashMap<AbsPath, Cached<Stat>>,
    real_paths: HashMap<AbsPath, Cached<AbsPath>>,
    probes: BTreeMap<AbsPath, bool>,
    exceeded: BTreeSet<Exceeded>,
    bytes_read: u64,
}

#[derive(Default)]
pub struct ProbeFs {
    state: Mutex<State>,
}

impl ProbeFs {
    pub fn new() -> ProbeFs {
        ProbeFs::default()
    }

    pub fn read(&self, path: &AbsPath) -> Cached<Arc<Vec<u8>>> {
        let mut state = self.state.lock().expect("probe state");
        if let Some(cached) = state.reads.get(path) {
            return cached.clone();
        }
        let left = MAX_SCAN_READ_BYTES.saturating_sub(state.bytes_read);
        let result = read_regular(path, left).map(Arc::new);
        match &result {
            Ok(bytes) => state.bytes_read += bytes.len() as u64,
            Err(err) if err.kind == ProbeErrorKind::BudgetSpent => {
                state.exceeded.insert(Exceeded::Reads {
                    max: MAX_SCAN_READ_BYTES,
                });
            }
            Err(_) => {}
        }
        state.probes.insert(path.clone(), result.is_ok());
        state.reads.insert(path.clone(), result.clone());
        result
    }

    /// Entries sorted by name.
    pub fn list(&self, path: &AbsPath) -> Cached<Arc<Vec<DirEntry>>> {
        let mut state = self.state.lock().expect("probe state");
        if let Some(cached) = state.lists.get(path) {
            return cached.clone();
        }
        let result = list_dir(path).map(Arc::new);
        state.probes.insert(path.clone(), result.is_ok());
        state.lists.insert(path.clone(), result.clone());
        result
    }

    /// Follows symlinks. `is_symlink` tells whether the path itself is one.
    pub fn stat(&self, path: &AbsPath) -> Cached<Stat> {
        let mut state = self.state.lock().expect("probe state");
        if let Some(cached) = state.stats.get(path) {
            return cached.clone();
        }
        let result = stat_path(path);
        state.probes.insert(path.clone(), result.is_ok());
        state.stats.insert(path.clone(), result.clone());
        result
    }

    pub fn real_path(&self, path: &AbsPath) -> Cached<AbsPath> {
        let mut state = self.state.lock().expect("probe state");
        if let Some(cached) = state.real_paths.get(path) {
            return cached.clone();
        }
        let result = fs::canonicalize(path.as_path())
            .map_err(ProbeError::from)
            .and_then(|real| {
                let raw = real.to_str().ok_or_else(|| ProbeError {
                    kind: ProbeErrorKind::Io(io::ErrorKind::InvalidData),
                    detail: "path is not UTF-8".to_owned(),
                })?;
                AbsPath::parse(raw).map_err(|e| ProbeError {
                    kind: ProbeErrorKind::Io(io::ErrorKind::InvalidData),
                    detail: e.to_string(),
                })
            });
        state.probes.insert(path.clone(), result.is_ok());
        state.real_paths.insert(path.clone(), result.clone());
        result
    }

    /// Every path looked at so far, in path order, with whether it existed.
    pub fn record(&self) -> Vec<Probe> {
        let state = self.state.lock().expect("probe state");
        state
            .probes
            .iter()
            .map(|(path, hit)| Probe {
                path: path.clone(),
                hit: *hit,
            })
            .collect()
    }

    pub fn probed(&self) -> Vec<AbsPath> {
        self.record().into_iter().map(|probe| probe.path).collect()
    }

    /// The bytes this scan has read from the disk. A cached read counts once.
    pub fn bytes_read(&self) -> u64 {
        self.state.lock().expect("probe state").bytes_read
    }

    pub(crate) fn note_exceeded(&self, exceeded: Exceeded) {
        let mut state = self.state.lock().expect("probe state");
        state.exceeded.insert(exceeded);
    }

    /// Every budget this scan ran out of, each once, in a stable order.
    pub fn exceeded(&self) -> Vec<Exceeded> {
        let state = self.state.lock().expect("probe state");
        state.exceeded.iter().cloned().collect()
    }

    /// Hash over the stat of every probed path, taken now.
    pub fn fingerprint(&self) -> Fingerprint {
        fingerprint_of(&self.probed())
    }
}

fn kind_of(file_type: fs::FileType) -> EntryKind {
    if file_type.is_dir() {
        EntryKind::Dir
    } else if file_type.is_file() {
        EntryKind::File
    } else {
        EntryKind::Other
    }
}

fn mtime_ns(meta: &fs::Metadata) -> i128 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos() as i128)
}

#[cfg(unix)]
fn is_executable(meta: &fs::Metadata) -> bool {
    std::os::unix::fs::PermissionsExt::mode(&meta.permissions()) & 0o111 != 0
}

#[cfg(not(unix))]
fn is_executable(_: &fs::Metadata) -> bool {
    true
}

/// Checks the kind and the size before it opens the file: opening a named pipe blocks until a
/// writer comes, and a device can stream without end. It reads one byte past the limit at most,
/// whatever the stat said, because a file can grow after the stat. A file larger than
/// `budget_left` is not read.
fn read_regular(path: &AbsPath, budget_left: u64) -> Cached<Vec<u8>> {
    let meta = fs::metadata(path.as_path())?;
    if !meta.is_file() {
        return Err(ProbeError::not_regular());
    }
    if meta.len() > MAX_READ_BYTES {
        return Err(ProbeError::too_large(meta.len()));
    }
    if meta.len() > budget_left {
        return Err(ProbeError::budget_spent());
    }
    let mut bytes = Vec::with_capacity(meta.len() as usize);
    fs::File::open(path.as_path())?
        .take(MAX_READ_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_READ_BYTES {
        return Err(ProbeError::too_large(bytes.len() as u64));
    }
    if bytes.len() as u64 > budget_left {
        return Err(ProbeError::budget_spent());
    }
    Ok(bytes)
}

fn stat_path(path: &AbsPath) -> Cached<Stat> {
    let meta = fs::metadata(path.as_path())?;
    let is_symlink = fs::symlink_metadata(path.as_path())
        .map(|m| m.is_symlink())
        .unwrap_or(false);
    Ok(Stat {
        kind: kind_of(meta.file_type()),
        is_symlink,
        len: meta.len(),
        mtime_ms: (mtime_ns(&meta) / 1_000_000) as i64,
        executable: is_executable(&meta),
    })
}

fn list_dir(path: &AbsPath) -> Cached<Vec<DirEntry>> {
    let mut entries = Vec::new();
    for entry in fs::read_dir(path.as_path())? {
        let entry = entry?;
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        // A symlink to a directory lists as a directory, as the agents see it.
        let kind = match fs::metadata(entry.path()) {
            Ok(meta) => kind_of(meta.file_type()),
            Err(_) => EntryKind::Other,
        };
        entries.push(DirEntry { name, kind });
    }
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(entries)
}

/// Hashes the length and modification time of each path, or that it is missing. The host calls
/// this once a second over the paths of the last scan, which is how `watch` notices a change.
pub fn fingerprint_of(paths: &[AbsPath]) -> Fingerprint {
    let mut hasher = Sha256::new();
    for path in paths {
        hasher.update(path.as_str().as_bytes());
        hasher.update([0]);
        match fs::metadata(path.as_path()) {
            Ok(meta) => {
                let kind = if meta.is_dir() { 'd' } else { 'f' };
                hasher.update(format!("{kind}:{}:{}", meta.len(), mtime_ns(&meta)).as_bytes());
                if let Ok(link) = fs::symlink_metadata(path.as_path())
                    && link.is_symlink()
                {
                    hasher.update(format!(":l:{}", mtime_ns(&link)).as_bytes());
                }
            }
            Err(_) => hasher.update(b"absent"),
        }
        hasher.update([b'\n']);
    }
    Fingerprint(format!("fp:{}", crate::model::hex(&hasher.finalize())))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn abs(dir: &tempfile::TempDir, rel: &str) -> AbsPath {
        AbsPath::parse(&format!(
            "{}/{rel}",
            dir.path().canonicalize().unwrap().display()
        ))
        .unwrap()
    }

    #[test]
    fn a_missing_path_is_recorded_as_a_miss_and_its_creation_changes_the_fingerprint() {
        let dir = tempfile::tempdir().unwrap();
        let target = abs(&dir, "CLAUDE.md");
        let fs = ProbeFs::new();
        assert!(fs.stat(&target).unwrap_err().is_not_found());
        assert_eq!(
            fs.record(),
            vec![Probe {
                path: target.clone(),
                hit: false
            }]
        );
        let before = fs.fingerprint();
        std::fs::write(target.as_path(), "x").unwrap();
        assert_ne!(fs.fingerprint(), before);
    }

    #[test]
    fn bytes_are_cached_for_the_life_of_the_scan() {
        let dir = tempfile::tempdir().unwrap();
        let target = abs(&dir, "AGENTS.md");
        std::fs::write(target.as_path(), "one").unwrap();
        let fs = ProbeFs::new();
        assert_eq!(fs.read(&target).unwrap().as_slice(), b"one");
        std::fs::write(target.as_path(), "two").unwrap();
        assert_eq!(fs.read(&target).unwrap().as_slice(), b"one");
    }

    #[test]
    fn reads_stop_at_the_scan_budget_and_a_link_is_read_once_per_name() {
        let dir = tempfile::tempdir().unwrap();
        let big = abs(&dir, "big.md");
        std::fs::write(big.as_path(), vec![b'x'; MAX_READ_BYTES as usize]).unwrap();
        let links = MAX_SCAN_READ_BYTES / MAX_READ_BYTES + 2;
        let fs = ProbeFs::new();
        let mut read = 0;
        for n in 0..links {
            let link = abs(&dir, &format!("l{n}.md"));
            std::os::unix::fs::symlink(big.as_path(), link.as_path()).unwrap();
            match fs.read(&link) {
                Ok(_) => read += 1,
                Err(err) => assert_eq!(err.kind, ProbeErrorKind::BudgetSpent),
            }
        }
        assert_eq!(read, MAX_SCAN_READ_BYTES / MAX_READ_BYTES);
        assert_eq!(fs.bytes_read(), MAX_SCAN_READ_BYTES);
        assert_eq!(
            fs.exceeded(),
            [Exceeded::Reads {
                max: MAX_SCAN_READ_BYTES
            }]
        );
        // A refusal does not hide a missing file, and a cached file still reads.
        assert!(fs.read(&abs(&dir, "gone.md")).unwrap_err().is_not_found());
        assert!(fs.read(&abs(&dir, "l0.md")).is_ok());
    }

    #[test]
    fn list_is_sorted_and_real_path_resolves_symlinks() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(abs(&dir, "b").as_path(), "").unwrap();
        std::fs::write(abs(&dir, "a").as_path(), "").unwrap();
        std::os::unix::fs::symlink(abs(&dir, "a").as_path(), abs(&dir, "link").as_path()).unwrap();
        let fs = ProbeFs::new();
        let names: Vec<_> = fs
            .list(&abs(&dir, ""))
            .unwrap()
            .iter()
            .map(|e| e.name.clone())
            .collect();
        assert_eq!(names, ["a", "b", "link"]);
        assert_eq!(fs.real_path(&abs(&dir, "link")).unwrap(), abs(&dir, "a"));
        assert!(fs.stat(&abs(&dir, "link")).unwrap().is_symlink);
    }
}
