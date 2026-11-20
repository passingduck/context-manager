//! The reference pass: markdown links and path-like mentions that point to existing markdown
//! files, followed to depth 3 with at most 500 nodes. It runs over the spans no adapter
//! claimed as imports.

use std::collections::HashSet;
use std::ops::Range;

use super::spans;
use crate::model::AbsPath;
use crate::probe_fs::{EntryKind, ProbeFs};

pub const MAX_DEPTH: u32 = 3;
pub const MAX_NODES: usize = 500;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefKind {
    /// A markdown link or an autolink.
    Link,
    /// A bare path-like mention.
    Mention,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefNode {
    pub kind: RefKind,
    /// The target or mention as written.
    pub text: String,
    pub path: AbsPath,
    /// 1-based position in the referring file.
    pub line: u32,
    pub col: u32,
    /// 1 for a reference in the starting document.
    pub depth: u32,
    pub children: Vec<RefNode>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefTree {
    pub root: AbsPath,
    pub nodes: Vec<RefNode>,
    /// True when the walk stopped at [`MAX_NODES`].
    pub capped: bool,
}

/// Follows the references of `doc`. `claimed` holds byte ranges of `text` that an adapter
/// already treats as imports. A target resolves against the referring file's directory first,
/// then against `project_root`, and must be an existing `.md` file inside `project_root`. A file
/// appears once in the tree, under the first name that reaches it: links to one file are one file.
pub fn follow(
    doc: &AbsPath,
    text: &str,
    claimed: &[Range<usize>],
    project_root: &AbsPath,
    fs: &ProbeFs,
) -> RefTree {
    let mut run = Run {
        root: project_root,
        fs,
        visited: HashSet::new(),
        count: 0,
        capped: false,
    };
    run.visited.insert(run.identity(doc));
    let nodes = run.refs_of(doc, text, claimed, 1);
    RefTree {
        root: doc.clone(),
        nodes,
        capped: run.capped,
    }
}

struct Run<'a> {
    root: &'a AbsPath,
    fs: &'a ProbeFs,
    visited: HashSet<AbsPath>,
    count: usize,
    capped: bool,
}

impl Run<'_> {
    fn refs_of(
        &mut self,
        file: &AbsPath,
        text: &str,
        claimed: &[Range<usize>],
        depth: u32,
    ) -> Vec<RefNode> {
        let s = spans::scan(super::analyzed(text));
        let dir = file.parent().unwrap_or_else(AbsPath::root);
        let mut found: Vec<(RefKind, String, &spans::Span)> = Vec::new();
        for link in &s.links {
            found.push((RefKind::Link, link.target.clone(), &link.span));
        }
        for token in &s.paths {
            found.push((RefKind::Mention, token.text.clone(), &token.span));
        }
        found.sort_by_key(|(_, _, span)| span.range.start);

        let mut out = Vec::new();
        for (kind, written, span) in found {
            if claimed
                .iter()
                .any(|c| c.start < span.range.end && span.range.start < c.end)
                || s.in_code_or_comment(span.range.start)
            {
                continue;
            }
            let Some(path) = self.resolve(&dir, &written) else {
                continue;
            };
            if !self.visited.insert(self.identity(&path)) {
                continue;
            }
            if self.count >= MAX_NODES {
                self.capped = true;
                break;
            }
            self.count += 1;
            let children = if depth < MAX_DEPTH {
                match self.fs.read(&path) {
                    Ok(bytes) => {
                        let child_text = String::from_utf8_lossy(&bytes).into_owned();
                        self.refs_of(&path, &child_text, &[], depth + 1)
                    }
                    Err(_) => Vec::new(),
                }
            } else {
                Vec::new()
            };
            out.push(RefNode {
                kind,
                text: written,
                path,
                line: span.line,
                col: span.col,
                depth,
                children,
            });
            if self.capped {
                break;
            }
        }
        out
    }

    /// The real path of `path`, so that every name of one file is one file.
    fn identity(&self, path: &AbsPath) -> AbsPath {
        self.fs.real_path(path).unwrap_or_else(|_| path.clone())
    }

    fn resolve(&self, dir: &AbsPath, written: &str) -> Option<AbsPath> {
        let target = written.split(['#', '?']).next().unwrap_or("");
        if target.is_empty() || target.contains("://") || target.starts_with("mailto:") {
            return None;
        }
        let target = target.replace("%20", " ");
        if !target.to_lowercase().ends_with(".md") {
            return None;
        }
        let rel = target.trim_start_matches("./");
        let candidates = if let Some(from_root) = rel.strip_prefix('/') {
            vec![self.root.join(from_root)]
        } else {
            vec![dir.join(rel), self.root.join(rel)]
        };
        candidates.into_iter().find(|path| {
            within(path, self.root)
                && self
                    .fs
                    .stat(path)
                    .is_ok_and(|stat| stat.kind == EntryKind::File)
        })
    }
}

fn within(path: &AbsPath, root: &AbsPath) -> bool {
    path.as_str()
        .starts_with(&format!("{}/", root.as_str().trim_end_matches('/')))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::Fixture;

    fn tree(f: &Fixture, doc: &str, claimed: &[Range<usize>]) -> RefTree {
        let path = f.project_path(doc);
        let text = std::fs::read_to_string(path.as_path()).unwrap();
        follow(&path, &text, claimed, &f.project(), &ProbeFs::new())
    }

    fn paths(nodes: &[RefNode], root: &AbsPath) -> Vec<String> {
        nodes
            .iter()
            .map(|n| {
                n.path
                    .as_str()
                    .strip_prefix(root.as_str())
                    .unwrap()
                    .to_owned()
            })
            .collect()
    }

    #[test]
    fn links_and_mentions_resolve_to_existing_markdown_files() {
        let f = Fixture::new();
        f.write(
            &f.project_path("docs/README.md"),
            "[a](a.md#top) and docs/b.md and [url](https://x.dev/c.md) [none](gone.md) [pic](p.png)\n",
        );
        f.write(&f.project_path("docs/a.md"), "");
        f.write(&f.project_path("docs/b.md"), "");
        let t = tree(&f, "docs/README.md", &[]);
        assert_eq!(paths(&t.nodes, &f.project()), ["/docs/a.md", "/docs/b.md"]);
        assert_eq!(t.nodes[0].kind, RefKind::Link);
        assert_eq!(t.nodes[1].kind, RefKind::Mention);
        assert_eq!((t.nodes[0].line, t.nodes[0].col), (1, 1));
        assert!(!t.capped);
    }

    #[test]
    fn a_target_resolves_beside_the_file_first_and_then_from_the_project_root() {
        let f = Fixture::new();
        f.write(&f.project_path("docs/a.md"), "[x](x.md) [y](guide/y.md)\n");
        f.write(&f.project_path("docs/x.md"), "");
        f.write(&f.project_path("x.md"), "");
        f.write(&f.project_path("guide/y.md"), "");
        let t = tree(&f, "docs/a.md", &[]);
        assert_eq!(paths(&t.nodes, &f.project()), ["/docs/x.md", "/guide/y.md"]);
    }

    #[test]
    fn code_comments_and_claimed_spans_are_not_followed() {
        let f = Fixture::new();
        let text = "`c.md`\n<!-- d.md -->\n[claimed](e.md) f.md\n";
        f.write(&f.project_path("CLAUDE.md"), text);
        for n in ["c.md", "d.md", "e.md", "f.md"] {
            f.write(&f.project_path(n), "");
        }
        let claimed_at = text.find("[claimed](e.md)").unwrap();
        let claimed = claimed_at..claimed_at + 15;
        let t = tree(&f, "CLAUDE.md", std::slice::from_ref(&claimed));
        assert_eq!(paths(&t.nodes, &f.project()), ["/f.md"]);
    }

    #[test]
    fn the_pass_follows_three_levels_and_each_file_appears_once() {
        let f = Fixture::new();
        f.write(&f.project_path("r.md"), "[1](l1.md)\n");
        f.write(&f.project_path("l1.md"), "[2](l2.md) [back](r.md)\n");
        f.write(&f.project_path("l2.md"), "[3](l3.md)\n");
        f.write(&f.project_path("l3.md"), "[4](l4.md)\n");
        f.write(&f.project_path("l4.md"), "");
        let t = tree(&f, "r.md", &[]);
        let l1 = &t.nodes[0];
        let l3 = &l1.children[0].children[0];
        assert_eq!((l1.depth, l3.depth), (1, 3));
        assert!(l3.children.is_empty());
        assert_eq!(l1.children.len(), 1);
    }

    #[test]
    fn links_to_one_file_are_one_file_under_the_first_name() {
        let f = Fixture::new();
        f.write(&f.project_path("big.md"), "");
        for n in 0..3 {
            f.symlink(
                &f.project_path(&format!("l{n}.md")),
                &f.project_path("big.md"),
            );
        }
        f.write(
            &f.project_path("r.md"),
            "[0](l0.md) [1](l1.md) [b](big.md) [2](l2.md)\n",
        );
        let t = tree(&f, "r.md", &[]);
        assert_eq!(paths(&t.nodes, &f.project()), ["/l0.md"]);
    }

    #[test]
    fn the_pass_stops_at_500_nodes_and_says_so() {
        let f = Fixture::new();
        let mut index = String::new();
        for i in 0..MAX_NODES + 20 {
            f.write(&f.project_path(&format!("n{i}.md")), "");
            index.push_str(&format!("[n](n{i}.md)\n"));
        }
        f.write(&f.project_path("r.md"), &index);
        let t = tree(&f, "r.md", &[]);
        assert!(t.capped);
        assert_eq!(t.nodes.len(), MAX_NODES);
    }
}
