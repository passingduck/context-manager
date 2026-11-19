//! `ProbeFs`: the only way an adapter reads the disk.
//!
//! It caches bytes for the length of one scan, so two agents that read `AGENTS.md` see one
//! revision. It records every path read, listed, checked or resolved, misses included. The
//! fingerprint hashes the stat of exactly those paths.

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::io;
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
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirEntry {
    pub name: String,
    pub kind: EntryKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeError {
    pub kind: io::ErrorKind,
    pub detail: String,
}

impl ProbeError {
    pub fn is_not_found(&self) -> bool {
        self.kind == io::ErrorKind::NotFound
    }
}

impl From<io::Error> for ProbeError {
    fn from(err: io::Error) -> ProbeError {
        ProbeError {
            kind: err.kind(),
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

type Cached<T> = Result<T, ProbeError>;

#[derive(Default)]
struct State {
    reads: HashMap<AbsPath, Cached<Arc<Vec<u8>>>>,
    lists: HashMap<AbsPath, Cached<Arc<Vec<DirEntry>>>>,
    stats: HashMap<AbsPath, Cached<Stat>>,
    real_paths: HashMap<AbsPath, Cached<AbsPath>>,
    probes: BTreeMap<AbsPath, bool>,
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
        let result = fs::read(path.as_path())
            .map(Arc::new)
            .map_err(ProbeError::from);
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
                    kind: io::ErrorKind::InvalidData,
                    detail: "path is not UTF-8".to_owned(),
                })?;
                AbsPath::parse(raw).map_err(|e| ProbeError {
                    kind: io::ErrorKind::InvalidData,
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
