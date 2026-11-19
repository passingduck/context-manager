//! One bounded walk of the project tree, shared by every adapter.
//!
//! It lists directories through [`ProbeFs`], so each listed directory lands in the probe
//! record. It honours `.gitignore` files (each applies below its own directory), skips hidden
//! directories except the agent homes, does not descend into symlinked directories, and stops
//! at [`MAX_ENTRIES`] entries.

use std::collections::{BTreeSet, HashMap, VecDeque};

use ignore::gitignore::{Gitignore, GitignoreBuilder};

use crate::model::AbsPath;
use crate::probe_fs::{EntryKind, ProbeFs};

/// The walk stops after this many files and directories.
pub const MAX_ENTRIES: usize = 20_000;

/// Hidden directories the walk still enters.
pub const VISIBLE_HIDDEN: [&str; 4] = [".claude", ".codex", ".gemini", ".agents"];

/// What the walk found. Paths are in path order.
#[derive(Debug, Clone)]
pub struct WalkIndex {
    pub root: AbsPath,
    /// True when the walk stopped at [`MAX_ENTRIES`]. The index is then partial.
    pub capped: bool,
    dirs: BTreeSet<AbsPath>,
    files: BTreeSet<AbsPath>,
    by_name: HashMap<String, Vec<AbsPath>>,
}

impl WalkIndex {
    /// Every directory the walk entered, the root included.
    pub fn dirs(&self) -> impl Iterator<Item = &AbsPath> {
        self.dirs.iter()
    }

    pub fn files(&self) -> impl Iterator<Item = &AbsPath> {
        self.files.iter()
    }

    pub fn entries(&self) -> usize {
        self.dirs.len() + self.files.len()
    }

    /// The directories that contain a file named `name`, in path order.
    pub fn dirs_containing(&self, name: &str) -> &[AbsPath] {
        self.by_name.get(name).map_or(&[], Vec::as_slice)
    }

    /// Every file below `dir`, at any depth, in path order.
    pub fn files_under<'a>(&'a self, dir: &AbsPath) -> impl Iterator<Item = &'a AbsPath> + 'a {
        let prefix = if dir.as_str() == "/" {
            "/".to_owned()
        } else {
            format!("{}/", dir.as_str())
        };
        let start = AbsPath::parse(prefix.trim_end_matches('/')).unwrap_or_else(|_| dir.clone());
        let skip = prefix.clone();
        self.files
            .range(start..)
            .skip_while(move |p| !p.as_str().starts_with(&skip))
            .take_while(move |p| p.as_str().starts_with(&prefix))
    }
}

/// Walks `root` breadth first.
pub fn walk(root: &AbsPath, fs: &ProbeFs) -> WalkIndex {
    let mut index = WalkIndex {
        root: root.clone(),
        capped: false,
        dirs: BTreeSet::new(),
        files: BTreeSet::new(),
        by_name: HashMap::new(),
    };
    index.dirs.insert(root.clone());
    let mut queue: VecDeque<(AbsPath, Vec<Gitignore>)> = VecDeque::new();
    queue.push_back((root.clone(), Vec::new()));
    let mut entries = 0usize;

    'walk: while let Some((dir, inherited)) = queue.pop_front() {
        let Ok(listing) = fs.list(&dir) else {
            continue;
        };
        let mut rules = inherited;
        if listing
            .iter()
            .any(|e| e.name == ".gitignore" && e.kind == EntryKind::File)
            && let Some(ignore) = load_gitignore(&dir, fs)
        {
            rules.push(ignore);
        }
        for entry in listing.iter() {
            let is_dir = entry.kind == EntryKind::Dir;
            let path = dir.join(&entry.name);
            if is_dir
                && entry.name.starts_with('.')
                && !VISIBLE_HIDDEN.contains(&entry.name.as_str())
            {
                continue;
            }
            if ignored(&rules, &path, is_dir) {
                continue;
            }
            if entries >= MAX_ENTRIES {
                index.capped = true;
                break 'walk;
            }
            entries += 1;
            match entry.kind {
                EntryKind::Dir => {
                    if fs.stat(&path).is_ok_and(|s| s.is_symlink) {
                        continue;
                    }
                    index.dirs.insert(path.clone());
                    queue.push_back((path, rules.clone()));
                }
                EntryKind::File | EntryKind::Other => {
                    index.files.insert(path);
                    index
                        .by_name
                        .entry(entry.name.clone())
                        .or_default()
                        .push(dir.clone());
                }
            }
        }
    }
    for dirs in index.by_name.values_mut() {
        dirs.sort();
    }
    index
}

fn load_gitignore(dir: &AbsPath, fs: &ProbeFs) -> Option<Gitignore> {
    let bytes = fs.read(&dir.join(".gitignore")).ok()?;
    let text = String::from_utf8_lossy(&bytes);
    let mut builder = GitignoreBuilder::new(dir.as_path());
    for line in text.lines() {
        let _ = builder.add_line(None, line);
    }
    builder.build().ok()
}

/// The deepest `.gitignore` with an opinion wins, as in git.
fn ignored(rules: &[Gitignore], path: &AbsPath, is_dir: bool) -> bool {
    for set in rules.iter().rev() {
        let m = set.matched(path.as_path(), is_dir);
        if m.is_ignore() {
            return true;
        }
        if m.is_whitelist() {
            return false;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::Fixture;

    fn names<'a>(paths: impl Iterator<Item = &'a AbsPath>, root: &AbsPath) -> Vec<String> {
        paths
            .map(|p| {
                p.as_str()
                    .strip_prefix(root.as_str())
                    .unwrap()
                    .trim_start_matches('/')
                    .to_owned()
            })
            .collect()
    }

    fn fixture() -> Fixture {
        let f = Fixture::new();
        f.write(
            &f.project_path(".gitignore"),
            "node_modules\n/build/\n*.log\n",
        );
        f.write(&f.project_path("CLAUDE.md"), "root");
        f.write(&f.project_path("pkg/CLAUDE.md"), "pkg");
        f.write(&f.project_path("pkg/deep/CLAUDE.md"), "deep");
        f.write(&f.project_path("pkg/deep/a.log"), "log");
        f.write(&f.project_path(".claude/rules/r.md"), "rule");
        f.write(&f.project_path(".git/HEAD"), "ref");
        f.write(&f.project_path(".cache/x.md"), "hidden");
        f.write(&f.project_path("node_modules/dep/CLAUDE.md"), "ignored");
        f.write(&f.project_path("build/out.md"), "ignored");
        f.write(&f.project_path("pkg/.gitignore"), "deep/skip.md\n");
        f.write(&f.project_path("pkg/deep/skip.md"), "ignored by nested");
        f
    }

    #[test]
    fn the_walk_honours_gitignore_and_skips_hidden_directories_except_agent_homes() {
        let f = fixture();
        let fs = ProbeFs::new();
        let index = walk(&f.project(), &fs);
        assert!(!index.capped);
        let root = f.project();
        assert_eq!(
            names(index.dirs(), &root),
            ["", ".claude", ".claude/rules", "pkg", "pkg/deep"]
        );
        let files = names(index.files(), &root);
        assert_eq!(
            files,
            [
                ".claude/rules/r.md",
                ".gitignore",
                "CLAUDE.md",
                "pkg/.gitignore",
                "pkg/CLAUDE.md",
                "pkg/deep/CLAUDE.md",
            ]
        );
    }

    #[test]
    fn the_index_answers_which_directories_hold_a_file_and_which_files_lie_under_one() {
        let f = fixture();
        let fs = ProbeFs::new();
        let index = walk(&f.project(), &fs);
        let root = f.project();
        assert_eq!(
            names(index.dirs_containing("CLAUDE.md").iter(), &root),
            ["", "pkg", "pkg/deep"]
        );
        assert!(index.dirs_containing("nope.md").is_empty());
        assert_eq!(
            names(index.files_under(&f.project_path("pkg")), &root),
            ["pkg/.gitignore", "pkg/CLAUDE.md", "pkg/deep/CLAUDE.md"]
        );
        // `pkg2` shares a prefix with `pkg` and is not under it.
        f.write(&f.project_path("pkg2/a.md"), "x");
        let index = walk(&f.project(), &ProbeFs::new());
        assert_eq!(index.files_under(&f.project_path("pkg")).count(), 3);
    }

    #[test]
    fn every_listed_directory_is_in_the_probe_record_and_ignored_ones_are_not() {
        let f = fixture();
        let fs = ProbeFs::new();
        let index = walk(&f.project(), &fs);
        let probed = fs.probed();
        for dir in index.dirs() {
            assert!(
                probed.contains(dir),
                "{} was listed but not probed",
                dir.as_str()
            );
        }
        assert!(!probed.contains(&f.project_path("node_modules")));
        assert!(!probed.contains(&f.project_path(".git")));
    }

    #[test]
    fn the_walk_stops_at_the_entry_cap_and_says_so() {
        let f = Fixture::new();
        for i in 0..MAX_ENTRIES + 5 {
            std::fs::write(f.project_path(&format!("f{i:05}.md")).as_path(), "").unwrap();
        }
        let index = walk(&f.project(), &ProbeFs::new());
        assert!(index.capped);
        assert_eq!(index.entries(), MAX_ENTRIES + 1);
    }

    #[test]
    fn symlinked_directories_are_not_entered() {
        let f = Fixture::new();
        f.write(&f.project_path("real/a.md"), "x");
        f.symlink(&f.project_path("link"), &f.project_path("real"));
        let index = walk(&f.project(), &ProbeFs::new());
        assert_eq!(index.files_under(&f.project_path("link")).count(), 0);
        assert_eq!(index.files_under(&f.project_path("real")).count(), 1);
    }
}
