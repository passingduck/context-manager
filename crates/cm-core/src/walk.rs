//! One bounded walk of the project tree, shared by every adapter, and the one bounded lister
//! through which an adapter lists any directory (design section 5).
//!
//! The walk lists directories through [`ProbeFs`], so each listed directory lands in the probe
//! record. It honours `.gitignore` files (each applies below its own directory), skips hidden
//! directories except the agent homes, does not descend into symlinked directories, and stops
//! at [`MAX_ENTRIES`] entries.

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::sync::Arc;

use ignore::gitignore::{Gitignore, GitignoreBuilder};

use crate::model::AbsPath;
use crate::probe_fs::{DirEntry, EntryKind, Exceeded, ProbeFs};

/// A tree listing lists directories down to this depth, the listed directory being depth 0. The
/// specs give no depth for the trees an adapter lists (Codex skills stop at 6 themselves), and
/// the deepest real one, a command namespace, is 3. Links cannot make a tree deeper than its real
/// directories, because each real directory is listed once.
pub const MAX_TREE_DEPTH: u32 = 8;

/// A tree listing stops after this many entries. The largest real tree on the development
/// machine, `~/.codex/skills`, has 958 entries in all, and Codex's own skill scan already stops a
/// plugin hash at this many in a scan. Each entry can become a load, so the budget bounds the
/// work an adapter does for one listing.
pub const MAX_TREE_ENTRIES: usize = 5_000;

/// One entry of a tree listing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listed {
    pub path: AbsPath,
    /// The names from the listed directory down to this entry, its own name last.
    pub rel: Vec<String>,
    /// A link to a directory lists as a directory, as the agents see it.
    pub kind: EntryKind,
}

impl Listed {
    pub fn name(&self) -> &str {
        self.rel.last().map_or("", String::as_str)
    }

    /// 1 for an entry of the listed directory itself.
    pub fn depth(&self) -> u32 {
        self.rel.len() as u32
    }
}

/// Where a tree listing stops.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TreeLimits {
    pub depth: u32,
    pub entries: usize,
}

/// The limits of every listing that looks for items: commands, skills, agents, rules.
pub const ITEM_LIMITS: TreeLimits = TreeLimits {
    depth: MAX_TREE_DEPTH,
    entries: MAX_TREE_ENTRIES,
};

/// What a tree listing found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tree {
    pub entries: Vec<Listed>,
    /// True when the listing stopped at a limit, so entries below the root can be missing.
    pub capped: bool,
}

/// The entries of `dir`, in name order, at most [`MAX_TREE_ENTRIES`] of them.
pub fn list_dir(fs: &ProbeFs, dir: &AbsPath) -> Vec<Listed> {
    list_tree(fs, dir, |_| false)
}

/// [`list_tree_with`] at [`ITEM_LIMITS`].
pub fn list_tree(
    fs: &ProbeFs,
    root: &AbsPath,
    descend: impl FnMut(&Listed) -> bool,
) -> Vec<Listed> {
    list_tree_with(fs, root, ITEM_LIMITS, descend).entries
}

/// Lists `root` and every directory below it that `descend` accepts, depth first: siblings in
/// name order, and the entries of a directory right after the directory. It lists each real
/// directory once, whatever number of links reaches it, and stops at `limits`, the listed
/// directory being depth 0. [`ProbeFs::exceeded`] records each stop. A root that cannot be
/// listed yields nothing.
pub fn list_tree_with(
    fs: &ProbeFs,
    root: &AbsPath,
    limits: TreeLimits,
    mut descend: impl FnMut(&Listed) -> bool,
) -> Tree {
    struct Frame {
        dir: AbsPath,
        rel: Vec<String>,
        entries: Arc<Vec<DirEntry>>,
        next: usize,
    }
    let mut out = Vec::new();
    let mut capped = false;
    let (Ok(real_root), Ok(entries)) = (fs.real_path(root), fs.list(root)) else {
        return Tree {
            entries: out,
            capped,
        };
    };
    let mut visited: HashSet<AbsPath> = HashSet::from([real_root]);
    let mut stack = vec![Frame {
        dir: root.clone(),
        rel: Vec::new(),
        entries,
        next: 0,
    }];
    while let Some(frame) = stack.last_mut() {
        let Some(entry) = frame.entries.get(frame.next) else {
            stack.pop();
            continue;
        };
        frame.next += 1;
        if out.len() >= limits.entries {
            fs.note_exceeded(Exceeded::ListingEntries {
                dir: root.clone(),
                max: u32::try_from(limits.entries).unwrap_or(u32::MAX),
            });
            capped = true;
            break;
        }
        let mut rel = frame.rel.clone();
        rel.push(entry.name.clone());
        let listed = Listed {
            path: frame.dir.join(&entry.name),
            rel,
            kind: entry.kind,
        };
        let child = (listed.kind == EntryKind::Dir && descend(&listed))
            .then(|| (listed.path.clone(), listed.rel.clone()));
        out.push(listed);
        let Some((dir, rel)) = child else {
            continue;
        };
        let Ok(real) = fs.real_path(&dir) else {
            continue;
        };
        if visited.contains(&real) {
            continue;
        }
        if rel.len() as u32 > limits.depth {
            fs.note_exceeded(Exceeded::ListingDepth {
                dir: root.clone(),
                max: limits.depth,
            });
            capped = true;
            continue;
        }
        visited.insert(real);
        if let Ok(entries) = fs.list(&dir) {
            stack.push(Frame {
                dir,
                rel,
                entries,
                next: 0,
            });
        }
    }
    Tree {
        entries: out,
        capped,
    }
}

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

    fn rels(tree: &[Listed]) -> Vec<String> {
        tree.iter().map(|e| e.rel.join("/")).collect()
    }

    #[test]
    fn a_tree_listing_lists_each_real_directory_once_whatever_links_reach_it() {
        let f = Fixture::new();
        let dir = f.project_path("commands");
        f.write(&dir.join("a/x.md"), "x");
        f.write(&dir.join("c.md"), "c");
        for link in ["l0", "l1", "l2"] {
            f.symlink(&dir.join(link), &dir);
        }
        f.symlink(&dir.join("a/up"), &dir);
        f.symlink(&dir.join("b"), &dir.join("a"));
        let fs = ProbeFs::new();
        let tree = list_tree(&fs, &dir, |_| true);
        // `a` is listed once, under its first path in name order. Every link shows as an entry.
        assert_eq!(
            rels(&tree),
            ["a", "a/up", "a/x.md", "b", "c.md", "l0", "l1", "l2"]
        );
        assert!(fs.exceeded().is_empty());
    }

    #[test]
    fn a_tree_listing_follows_descend_and_lists_a_flat_directory_without_it() {
        let f = Fixture::new();
        let dir = f.project_path("skills");
        f.write(&dir.join("s/SKILL.md"), "s");
        f.write(&dir.join(".hidden/SKILL.md"), "h");
        let fs = ProbeFs::new();
        let tree = list_tree(&fs, &dir, |e| !e.name().starts_with('.'));
        assert_eq!(rels(&tree), [".hidden", "s", "s/SKILL.md"]);
        assert_eq!(rels(&list_dir(&fs, &dir)), [".hidden", "s"]);
        assert!(list_dir(&fs, &f.project_path("missing")).is_empty());
    }

    #[test]
    fn a_tree_listing_stops_at_its_entry_budget_and_says_so() {
        let f = Fixture::new();
        let dir = f.project_path("big");
        f.mkdir(&dir);
        for i in 0..MAX_TREE_ENTRIES + 5 {
            std::fs::write(dir.join(&format!("f{i:05}.md")).as_path(), "").unwrap();
        }
        let fs = ProbeFs::new();
        let tree = list_tree(&fs, &dir, |_| true);
        assert_eq!(tree.len(), MAX_TREE_ENTRIES);
        assert_eq!(tree[0].name(), "f00000.md");
        assert_eq!(
            fs.exceeded(),
            [Exceeded::ListingEntries {
                dir,
                max: MAX_TREE_ENTRIES as u32
            }]
        );
    }

    #[test]
    fn a_tree_listing_stops_below_its_depth_limit_and_says_so() {
        let f = Fixture::new();
        let dir = f.project_path("deep");
        let segs: Vec<String> = (1..=MAX_TREE_DEPTH + 2).map(|d| format!("d{d}")).collect();
        f.write(&dir.join(&segs.join("/")).join("x.md"), "x");
        let fs = ProbeFs::new();
        let tree = list_tree(&fs, &dir, |_| true);
        // Directories down to the limit are listed, so the entries reach one level further.
        assert_eq!(tree.len(), MAX_TREE_DEPTH as usize + 1);
        assert_eq!(tree.last().unwrap().depth(), MAX_TREE_DEPTH + 1);
        assert_eq!(
            fs.exceeded(),
            [Exceeded::ListingDepth {
                dir,
                max: MAX_TREE_DEPTH
            }]
        );
    }

    /// Design section 5: no adapter lists a directory by itself. Each listing goes through
    /// [`list_tree`], which is what bounds it.
    #[test]
    fn no_adapter_calls_the_raw_directory_listing() {
        fn rust_files(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
            for entry in std::fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    rust_files(&path, out);
                } else if path.extension().is_some_and(|e| e == "rs") {
                    out.push(path);
                }
            }
        }
        let agents = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/agents");
        let mut files = Vec::new();
        rust_files(&agents, &mut files);
        let offenders: Vec<String> = files
            .iter()
            .flat_map(|file| {
                let text = std::fs::read_to_string(file).unwrap();
                text.lines()
                    .enumerate()
                    .filter(|(_, line)| line.contains(".list("))
                    .map(|(n, _)| format!("{}:{}", file.display(), n + 1))
                    .collect::<Vec<_>>()
            })
            .collect();
        assert!(
            offenders.is_empty(),
            "list directories with walk::list_tree or walk::list_dir, not ProbeFs::list: {offenders:?}"
        );
    }
}
