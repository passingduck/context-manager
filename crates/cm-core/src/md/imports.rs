//! The `@path` import expander, driven by data. [`ImportRules::claude`] encodes Claude spec 1.5,
//! [`ImportRules::gemini`] encodes Gemini spec 3, and [`ImportRules::none`] is an agent with
//! no imports.

use std::collections::HashSet;
use std::ops::Range;

use super::spans::{self, AtStyle};
use crate::model::AbsPath;
use crate::probe_fs::ProbeFs;

/// Which `@token`s count as imports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenSyntax {
    /// This agent has no imports.
    None,
    /// `@` after whitespace. `\ ` encodes a space. A `#fragment` is dropped. The path starts
    /// with `./`, `~/`, `/` or `[A-Za-z0-9._-]`.
    Claude,
    /// `@` after a space, tab, CR or LF. The path runs to the next whitespace and starts with
    /// `.`, `/` or an ASCII letter. `~` is not accepted.
    Gemini,
}

/// Which regions of a file never hold an import.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protect {
    /// Code spans, fenced code blocks (backtick and tilde) and block-level HTML comments.
    Markdown,
    /// A run of N backticks up to the next run of N, and nothing else.
    BacktickRuns,
    Nothing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CyclePolicy {
    /// One seen-set for the whole load. A file loaded anywhere is not loaded again.
    OneSeenSet,
    /// One seen-set per chain of imports. Siblings may load the same file.
    PerChain,
}

/// What happens to an import that resolves outside the allowed root.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutsidePolicy {
    NeedsApproval,
    Refuse,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileTypes {
    Any,
    /// Lowercase extensions with the dot. A name with no extension is also accepted.
    Extensions(&'static [&'static str]),
}

/// Claude spec 1.5, `gZr`: a file with another extension is skipped.
pub const CLAUDE_TEXT_EXTENSIONS: &[&str] = &[
    ".md",
    ".markdown",
    ".mdx",
    ".txt",
    ".text",
    ".json",
    ".jsonc",
    ".yaml",
    ".yml",
    ".toml",
    ".xml",
    ".csv",
    ".html",
    ".htm",
    ".css",
    ".scss",
    ".js",
    ".jsx",
    ".mjs",
    ".cjs",
    ".ts",
    ".tsx",
    ".py",
    ".rb",
    ".go",
    ".rs",
    ".java",
    ".kt",
    ".swift",
    ".c",
    ".h",
    ".cc",
    ".cpp",
    ".hpp",
    ".cs",
    ".php",
    ".sh",
    ".bash",
    ".zsh",
    ".fish",
    ".sql",
    ".lua",
    ".r",
    ".ini",
    ".cfg",
    ".conf",
    ".env",
    ".log",
    ".rst",
    ".tex",
    ".vue",
    ".svelte",
    ".graphql",
    ".proto",
];

#[derive(Debug, Clone)]
pub struct ImportRules {
    pub syntax: TokenSyntax,
    /// What `~/` resolves against. `None` means `~/` is not a path.
    pub home: Option<AbsPath>,
    /// Imports must stay inside this directory. `None` allows any path.
    pub root: Option<AbsPath>,
    pub outside: OutsidePolicy,
    /// Compare real paths, with symlinks resolved, when checking the root.
    pub root_by_real_path: bool,
    /// The deepest level that loads. The root file is depth 0.
    pub max_depth: u32,
    /// Whether a token one level below `max_depth` gets a `TooDeep` node. When false the token
    /// is left alone and no node exists.
    pub report_too_deep: bool,
    pub protect: Protect,
    pub cycles: CyclePolicy,
    pub file_types: FileTypes,
}

impl ImportRules {
    /// Claude Code (spec 1.5). `cwd` is the original working directory: an import outside it
    /// needs approval. Pass `None` for a user-scope file, which skips the approval.
    pub fn claude(home: AbsPath, cwd: Option<AbsPath>) -> ImportRules {
        ImportRules {
            syntax: TokenSyntax::Claude,
            home: Some(home),
            root: cwd,
            outside: OutsidePolicy::NeedsApproval,
            root_by_real_path: false,
            max_depth: 4,
            report_too_deep: true,
            protect: Protect::Markdown,
            cycles: CyclePolicy::OneSeenSet,
            file_types: FileTypes::Extensions(CLAUDE_TEXT_EXTENSIONS),
        }
    }

    /// Gemini CLI (spec 3). `root` is the allowed root of the top-level file.
    pub fn gemini(root: AbsPath) -> ImportRules {
        ImportRules {
            syntax: TokenSyntax::Gemini,
            home: None,
            root: Some(root),
            outside: OutsidePolicy::Refuse,
            root_by_real_path: true,
            max_depth: 5,
            report_too_deep: false,
            protect: Protect::BacktickRuns,
            cycles: CyclePolicy::PerChain,
            file_types: FileTypes::Any,
        }
    }

    /// An agent with no imports: `expand` returns a tree with no nodes.
    pub fn none() -> ImportRules {
        ImportRules {
            syntax: TokenSyntax::None,
            home: None,
            root: None,
            outside: OutsidePolicy::Refuse,
            root_by_real_path: false,
            max_depth: 0,
            report_too_deep: false,
            protect: Protect::Nothing,
            cycles: CyclePolicy::OneSeenSet,
            file_types: FileTypes::Any,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportOutcome {
    Imported,
    Missing,
    OutsideRoot,
    TooDeep,
    /// The file is already on the chain of imports that led here.
    Cycle,
    /// The file was loaded earlier in this load.
    AlreadyLoaded,
    NeedsApproval,
    NotText,
    /// The path exists but cannot be read as a file, or is not a path the agent accepts.
    Unreadable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportNode {
    /// The token as written, `@` included.
    pub token: String,
    /// The resolved path. `None` when the token has no usable path.
    pub path: Option<AbsPath>,
    /// 1-based line and column of the `@` in the importing file.
    pub line: u32,
    pub col: u32,
    /// The byte range of the token in the importing file.
    pub range: Range<usize>,
    /// The depth of the imported file. The root file is 0, its imports are 1.
    pub depth: u32,
    pub outcome: ImportOutcome,
    /// The imports found inside this file. Empty unless the outcome is `Imported`.
    pub children: Vec<ImportNode>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportTree {
    pub root: AbsPath,
    pub nodes: Vec<ImportNode>,
}

impl ImportTree {
    /// The files that were imported, depth first, parent before children.
    pub fn imported(&self) -> Vec<&ImportNode> {
        fn walk<'a>(nodes: &'a [ImportNode], out: &mut Vec<&'a ImportNode>) {
            for node in nodes {
                if node.outcome == ImportOutcome::Imported {
                    out.push(node);
                    walk(&node.children, out);
                }
            }
        }
        let mut out = Vec::new();
        walk(&self.nodes, &mut out);
        out
    }
}

/// Expands the imports of `root`. The root file's own text is read through `fs`. An unreadable
/// root has no nodes.
pub fn expand(root: &AbsPath, rules: &ImportRules, fs: &ProbeFs) -> ImportTree {
    let mut seen = HashSet::new();
    expand_with_seen(root, rules, fs, &mut seen)
}

/// Like [`expand`], with a seen-set the caller owns. Claude Code shares one set across every
/// instruction file of a session, so pass the same set for each file in load order.
pub fn expand_with_seen(
    root: &AbsPath,
    rules: &ImportRules,
    fs: &ProbeFs,
    seen: &mut HashSet<AbsPath>,
) -> ImportTree {
    let nodes = match fs.read(root) {
        Ok(bytes) if rules.syntax != TokenSyntax::None => {
            seen.insert(root.clone());
            let text = String::from_utf8_lossy(&bytes);
            let mut chain = Vec::new();
            Expander { rules, fs, seen }.nodes(root, &text, 0, &mut chain)
        }
        _ => Vec::new(),
    };
    ImportTree {
        root: root.clone(),
        nodes,
    }
}

struct Expander<'a> {
    rules: &'a ImportRules,
    fs: &'a ProbeFs,
    seen: &'a mut HashSet<AbsPath>,
}

impl Expander<'_> {
    fn nodes(
        &mut self,
        file: &AbsPath,
        text: &str,
        depth: u32,
        chain: &mut Vec<AbsPath>,
    ) -> Vec<ImportNode> {
        let protected = self.protected(text);
        let style = match self.rules.syntax {
            TokenSyntax::Gemini => AtStyle::Gemini,
            _ => AtStyle::Claude,
        };
        let dir = file.parent().unwrap_or_else(AbsPath::root);
        let mut out = Vec::new();
        for token in spans::at_tokens_of(text, style) {
            let start = token.span.range.start;
            if protected.iter().any(|r| r.contains(&start)) {
                continue;
            }
            let Some(path_text) = self.import_path(&token.text) else {
                continue;
            };
            let raw = format!("@{}", token.text);
            let mut node = ImportNode {
                token: raw,
                path: None,
                line: token.span.line,
                col: token.span.col,
                range: token.span.range.clone(),
                depth: depth + 1,
                outcome: ImportOutcome::Missing,
                children: Vec::new(),
            };
            let path = self.resolve(&dir, &path_text);
            node.path = Some(path.clone());
            if depth + 1 > self.rules.max_depth {
                if self.rules.report_too_deep {
                    node.outcome = ImportOutcome::TooDeep;
                    out.push(node);
                }
                continue;
            }
            if self.rules.syntax == TokenSyntax::Gemini && has_url_scheme(&path_text) {
                node.path = None;
                node.outcome = ImportOutcome::Unreadable;
                out.push(node);
                continue;
            }
            node.outcome = self.load(&path, &mut node.children, depth + 1, chain);
            out.push(node);
        }
        out
    }

    fn load(
        &mut self,
        path: &AbsPath,
        children: &mut Vec<ImportNode>,
        depth: u32,
        chain: &mut Vec<AbsPath>,
    ) -> ImportOutcome {
        if let Some(outcome) = self.outside_root(path) {
            return outcome;
        }
        if !self.accepts_type(path) {
            return ImportOutcome::NotText;
        }
        match self.rules.cycles {
            CyclePolicy::OneSeenSet if self.seen.contains(path) => {
                return ImportOutcome::AlreadyLoaded;
            }
            CyclePolicy::PerChain if chain.contains(path) => return ImportOutcome::Cycle,
            _ => {}
        }
        let bytes = match self.fs.read(path) {
            Ok(bytes) => bytes,
            Err(err) if err.is_not_found() => return ImportOutcome::Missing,
            Err(_) => return ImportOutcome::Unreadable,
        };
        let Ok(text) = std::str::from_utf8(&bytes) else {
            return ImportOutcome::NotText;
        };
        self.seen.insert(path.clone());
        chain.push(path.clone());
        *children = self.nodes(path, text, depth, chain);
        chain.pop();
        ImportOutcome::Imported
    }

    fn protected(&self, text: &str) -> Vec<Range<usize>> {
        match self.rules.protect {
            Protect::Nothing => Vec::new(),
            Protect::BacktickRuns => spans::backtick_regions(text),
            Protect::Markdown => {
                let s = spans::scan(text);
                s.fences
                    .iter()
                    .chain(&s.code_spans)
                    .chain(&s.comments)
                    .map(|x| x.range.clone())
                    .collect()
            }
        }
    }

    /// The path text of a token, or `None` when the token is not an import.
    fn import_path(&self, token: &str) -> Option<String> {
        match self.rules.syntax {
            TokenSyntax::None => None,
            TokenSyntax::Claude => {
                let unescaped = token.replace("\\ ", " ");
                let path = unescaped.split('#').next().unwrap_or("");
                let first = path.chars().next()?;
                let ok = path.starts_with("./")
                    || path.starts_with("~/")
                    || (path.starts_with('/') && path != "/")
                    || first.is_ascii_alphanumeric()
                    || matches!(first, '.' | '_' | '-');
                ok.then(|| path.to_owned())
            }
            TokenSyntax::Gemini => {
                let first = token.chars().next()?;
                (first == '.' || first == '/' || first.is_ascii_alphabetic())
                    .then(|| token.to_owned())
            }
        }
    }

    fn resolve(&self, dir: &AbsPath, path: &str) -> AbsPath {
        if let Some(rest) = path.strip_prefix("~/")
            && let Some(home) = &self.rules.home
        {
            return home.join(rest);
        }
        dir.join(path)
    }

    fn outside_root(&self, path: &AbsPath) -> Option<ImportOutcome> {
        let root = self.rules.root.as_ref()?;
        let (path, root) = if self.rules.root_by_real_path {
            (
                self.fs.real_path(path).unwrap_or_else(|_| path.clone()),
                self.fs.real_path(root).unwrap_or_else(|_| root.clone()),
            )
        } else {
            (path.clone(), root.clone())
        };
        if is_within(&path, &root) {
            return None;
        }
        Some(match self.rules.outside {
            OutsidePolicy::NeedsApproval => ImportOutcome::NeedsApproval,
            OutsidePolicy::Refuse => ImportOutcome::OutsideRoot,
        })
    }

    fn accepts_type(&self, path: &AbsPath) -> bool {
        let FileTypes::Extensions(allowed) = self.rules.file_types else {
            return true;
        };
        let name = path.file_name().unwrap_or("");
        match name.rfind('.') {
            Some(dot) if dot > 0 => allowed.contains(&name[dot..].to_lowercase().as_str()),
            _ => true,
        }
    }
}

fn has_url_scheme(path: &str) -> bool {
    path.starts_with("file://") || path.starts_with("http://") || path.starts_with("https://")
}

fn is_within(path: &AbsPath, root: &AbsPath) -> bool {
    path == root
        || path
            .as_str()
            .starts_with(&format!("{}/", root.as_str().trim_end_matches('/')))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::Fixture;
    use ImportOutcome::*;

    struct Case {
        f: Fixture,
    }

    impl Case {
        fn new(files: &[(&str, &str)]) -> Case {
            let f = Fixture::new();
            for (name, body) in files {
                f.write(&f.project_path(name), body);
            }
            Case { f }
        }

        fn claude(&self) -> ImportRules {
            ImportRules::claude(self.f.home(), Some(self.f.project()))
        }

        fn gemini(&self) -> ImportRules {
            ImportRules::gemini(self.f.project())
        }

        fn run(&self, rules: &ImportRules, root: &str) -> ImportTree {
            expand(&self.f.project_path(root), rules, &ProbeFs::new())
        }
    }

    /// `(token, outcome, depth)` of every node, depth first.
    fn flat(tree: &ImportTree) -> Vec<(String, ImportOutcome, u32)> {
        fn go(nodes: &[ImportNode], out: &mut Vec<(String, ImportOutcome, u32)>) {
            for n in nodes {
                out.push((n.token.clone(), n.outcome, n.depth));
                go(&n.children, out);
            }
        }
        let mut out = Vec::new();
        go(&tree.nodes, &mut out);
        out
    }

    fn row(token: &str, outcome: ImportOutcome, depth: u32) -> (String, ImportOutcome, u32) {
        (token.to_owned(), outcome, depth)
    }

    // ---- Claude, spec 1.5 ----

    #[test]
    fn claude_email_like_text_is_not_an_import() {
        let c = Case::new(&[
            ("CLAUDE.md", "mail foo@docs/email.md now\n"),
            ("docs/email.md", "x"),
        ]);
        assert!(c.run(&c.claude(), "CLAUDE.md").nodes.is_empty());
    }

    #[test]
    fn claude_escaped_space_is_part_of_the_path_and_a_fragment_is_dropped() {
        let c = Case::new(&[
            ("CLAUDE.md", "@with\\ space.md and @frag.md#section\n"),
            ("with space.md", "x"),
            ("frag.md", "y"),
        ]);
        let tree = c.run(&c.claude(), "CLAUDE.md");
        assert_eq!(
            flat(&tree),
            [
                row("@with\\ space.md", Imported, 1),
                row("@frag.md#section", Imported, 1)
            ]
        );
        assert_eq!(tree.nodes[1].path, Some(c.f.project_path("frag.md")));
    }

    #[test]
    fn claude_skips_code_spans_fences_and_html_comments() {
        let text = "`@span.md`\n```\n@fence.md\n```\n~~~\n@tilde.md\n~~~\n<!-- @comment.md -->\n@real.md\n";
        let c = Case::new(&[
            ("CLAUDE.md", text),
            ("span.md", ""),
            ("fence.md", ""),
            ("tilde.md", ""),
            ("comment.md", ""),
            ("real.md", ""),
        ]);
        assert_eq!(
            flat(&c.run(&c.claude(), "CLAUDE.md")),
            [row("@real.md", Imported, 1)]
        );
    }

    #[test]
    fn claude_path_forms_quotes_and_relative_resolution() {
        let c = Case::new(&[
            ("CLAUDE.md", "@docs/i1.md \"@quoted.md\" @/ @~/x.md\n"),
            ("docs/i1.md", "@i2.md\n"),
            ("docs/i2.md", ""),
        ]);
        let tree = c.run(&c.claude(), "CLAUDE.md");
        // `i2.md` resolves against the importing file's directory, not the working directory.
        assert_eq!(
            flat(&tree),
            [
                row("@docs/i1.md", Imported, 1),
                row("@i2.md", Imported, 2),
                row("@~/x.md", NeedsApproval, 1)
            ]
        );
    }

    #[test]
    fn claude_chain_stops_after_four_hops() {
        let files: Vec<(String, String)> = (1..=6)
            .map(|i| (format!("i{i}.md"), format!("@i{}.md\n", i + 1)))
            .collect();
        let mut all: Vec<(&str, &str)> = vec![("CLAUDE.md", "@i1.md\n")];
        all.extend(files.iter().map(|(a, b)| (a.as_str(), b.as_str())));
        let c = Case::new(&all);
        assert_eq!(
            flat(&c.run(&c.claude(), "CLAUDE.md")),
            [
                row("@i1.md", Imported, 1),
                row("@i2.md", Imported, 2),
                row("@i3.md", Imported, 3),
                row("@i4.md", Imported, 4),
                row("@i5.md", TooDeep, 5),
            ]
        );
    }

    #[test]
    fn claude_loads_a_file_once() {
        let c = Case::new(&[
            ("CLAUDE.md", "@a.md @b.md @CLAUDE.md\n"),
            ("a.md", "@shared.md\n"),
            ("b.md", "@shared.md\n"),
            ("shared.md", ""),
        ]);
        assert_eq!(
            flat(&c.run(&c.claude(), "CLAUDE.md")),
            [
                row("@a.md", Imported, 1),
                row("@shared.md", Imported, 2),
                row("@b.md", Imported, 1),
                row("@shared.md", AlreadyLoaded, 2),
                row("@CLAUDE.md", AlreadyLoaded, 1),
            ]
        );
    }

    #[test]
    fn claude_a_path_outside_the_working_directory_needs_approval_and_is_not_read() {
        let c = Case::new(&[("CLAUDE.md", "@../outside.md\n")]);
        c.f.write(&c.f.root().join("outside.md"), "secret");
        let fs = ProbeFs::new();
        let tree = expand(&c.f.project_path("CLAUDE.md"), &c.claude(), &fs);
        assert_eq!(flat(&tree), [row("@../outside.md", NeedsApproval, 1)]);
        assert!(!fs.probed().contains(&c.f.root().join("outside.md")));
        // A user-scope file has no approval step.
        let user = ImportRules::claude(c.f.home(), None);
        assert_eq!(
            flat(&c.run(&user, "CLAUDE.md")),
            [row("@../outside.md", Imported, 1)]
        );
    }

    #[test]
    fn claude_skips_non_text_files_and_reports_missing_ones() {
        let c = Case::new(&[
            ("CLAUDE.md", "@pic.png @gone.md @noext\n"),
            ("pic.png", "x"),
            ("noext", "y"),
        ]);
        assert_eq!(
            flat(&c.run(&c.claude(), "CLAUDE.md")),
            [
                row("@pic.png", NotText, 1),
                row("@gone.md", Missing, 1),
                row("@noext", Imported, 1)
            ]
        );
    }

    // ---- Gemini, spec 3 ----

    #[test]
    fn gemini_a_bare_word_is_a_failed_import_but_tilde_and_digits_stay_literal() {
        let c = Case::new(&[("GEMINI.md", "@someone said @~/x.md and @1.md\n")]);
        assert_eq!(
            flat(&c.run(&c.gemini(), "GEMINI.md")),
            [row("@someone", Missing, 1)]
        );
    }

    #[test]
    fn gemini_backticks_protect_and_tilde_fences_do_not() {
        let c = Case::new(&[
            (
                "GEMINI.md",
                "`@span.md`\n```\n@fence.md\n```\n~~~\n@tilde.md\n~~~\n",
            ),
            ("span.md", ""),
            ("fence.md", ""),
            ("tilde.md", ""),
        ]);
        assert_eq!(
            flat(&c.run(&c.gemini(), "GEMINI.md")),
            [row("@tilde.md", Imported, 1)]
        );
    }

    #[test]
    fn gemini_a_cycle_inlines_the_root_once_more_and_then_stops() {
        let c = Case::new(&[("A.md", "@B.md\n"), ("B.md", "@A.md\n")]);
        assert_eq!(
            flat(&c.run(&c.gemini(), "A.md")),
            [
                row("@B.md", Imported, 1),
                row("@A.md", Imported, 2),
                row("@B.md", Cycle, 3),
            ]
        );
    }

    #[test]
    fn gemini_siblings_may_import_the_same_file_because_the_set_is_per_chain() {
        let c = Case::new(&[
            ("A.md", "@B.md @C.md\n"),
            ("B.md", "@S.md\n"),
            ("C.md", "@S.md\n"),
            ("S.md", ""),
        ]);
        let tokens: Vec<_> = flat(&c.run(&c.gemini(), "A.md"))
            .into_iter()
            .map(|r| r.1)
            .collect();
        assert_eq!(tokens, [Imported; 4]);
    }

    #[test]
    fn gemini_depth_five_is_the_last_level_and_its_tokens_are_left_alone() {
        let files: Vec<(String, String)> = (1..=7)
            .map(|i| (format!("f{i}.md"), format!("@f{}.md\n", i + 1)))
            .collect();
        let mut all: Vec<(&str, &str)> = vec![("GEMINI.md", "@f1.md\n")];
        all.extend(files.iter().map(|(a, b)| (a.as_str(), b.as_str())));
        let c = Case::new(&all);
        let rows = flat(&c.run(&c.gemini(), "GEMINI.md"));
        assert_eq!(rows.len(), 5);
        assert_eq!(rows[4], row("@f5.md", Imported, 5));
    }

    #[test]
    fn gemini_a_path_outside_the_root_is_refused_by_real_path() {
        let c = Case::new(&[("GEMINI.md", "@../outside.md @link.md\n")]);
        c.f.write(&c.f.root().join("outside.md"), "x");
        c.f.symlink(&c.f.project_path("link.md"), &c.f.root().join("outside.md"));
        assert_eq!(
            flat(&c.run(&c.gemini(), "GEMINI.md")),
            [
                row("@../outside.md", OutsideRoot, 1),
                row("@link.md", OutsideRoot, 1)
            ]
        );
    }

    #[test]
    fn gemini_urls_fail_and_any_file_type_loads() {
        let c = Case::new(&[
            ("GEMINI.md", "@https://x.dev/a @pic.png\n"),
            ("pic.png", "text"),
        ]);
        assert_eq!(
            flat(&c.run(&c.gemini(), "GEMINI.md")),
            [
                row("@https://x.dev/a", Unreadable, 1),
                row("@pic.png", Imported, 1)
            ]
        );
    }

    // ---- no imports ----

    #[test]
    fn an_agent_without_imports_has_no_nodes() {
        let c = Case::new(&[("AGENTS.md", "@a.md\n"), ("a.md", "")]);
        assert!(c.run(&ImportRules::none(), "AGENTS.md").nodes.is_empty());
    }
}
