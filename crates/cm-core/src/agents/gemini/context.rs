//! Context files in three tiers (spec 1), their imports (spec 3) and the loads they produce.

use std::collections::{BTreeMap, HashMap, HashSet};

use super::memory;
use super::util::{
    accessible, cmp_utf16, collapse_newlines, fold, js_trim, kind, read_lossy, real, rule,
};
use super::world::World;
use crate::md::imports::{ImportNode, ImportOutcome, ImportRules, expand};
use crate::model::*;
use crate::probe_fs::EntryKind;
use crate::tokens;
use crate::walk;

/// One top-level context file as the CLI concatenates it.
#[derive(Debug, Clone)]
pub(super) struct Item {
    pub load: LoadId,
    pub path: AbsPath,
    /// The file's text with its imports inlined and its ends trimmed, which the CLI puts inside a
    /// `--- Context from: <p> ---` frame. `None` when the file is skipped or empty.
    pub text: Option<String>,
}

#[derive(Default)]
pub(super) struct Gathered {
    pub global: Vec<Item>,
    pub extension: Vec<Item>,
    pub project: Vec<Item>,
    pub private: Vec<Item>,
    pub jit: Vec<Item>,
}

pub(super) struct Builder<'w, 'a> {
    pub w: &'w World<'a>,
    pub loads: Vec<Load>,
    pub diagnostics: Vec<Diagnostic>,
    /// The text of each delivered load, as the scan counted it. A session block uses it as the
    /// load's piece, and `load_texts` returns it for a load that fires later.
    pub texts: BTreeMap<LoadId, String>,
    /// Identity (the folded real path) of every file loaded so far, with its load. Stands in for
    /// the CLI's `dev:inode` dedupe.
    identities: HashMap<String, LoadId>,
}

impl<'w, 'a> Builder<'w, 'a> {
    pub fn new(w: &'w World<'a>) -> Builder<'w, 'a> {
        Builder {
            w,
            loads: Vec::new(),
            diagnostics: Vec::new(),
            texts: BTreeMap::new(),
            identities: HashMap::new(),
        }
    }

    pub fn next_id(&self) -> LoadId {
        LoadId(self.loads.len() as u32)
    }

    pub fn push(&mut self, load: Load) -> LoadId {
        let id = load.id.clone();
        self.loads.push(load);
        id
    }

    fn identity(&self, path: &AbsPath) -> String {
        fold(self.w.cx.env.os, real(self.w.cx, path).as_str())
    }

    /// Adds a context file as a top-level load, and its imports as loads under it.
    pub fn add_context_file(
        &mut self,
        path: &AbsPath,
        scope: Scope,
        doc: DocKind,
        rule_id: &'static str,
        when: Activation,
    ) -> Item {
        let cx = self.w.cx;
        let id = self.next_id();
        let load = |status: LoadStatus, delivered: Option<Delivered>| Load {
            id: LoadId(id.0),
            file: path.clone(),
            kind: doc,
            scope: scope.clone(),
            via: Via::Discovery {
                rule: rule(rule_id),
            },
            when: when.clone(),
            status,
            delivered,
        };
        let skipped = |skip: Skip| load(LoadStatus::Skipped { skip }, None);
        let bare = |id: LoadId, text: Option<String>| Item {
            load: id,
            path: path.clone(),
            text,
        };

        let identity = self.identity(path);
        if let Some(first) = self.identities.get(&identity).cloned() {
            let l = self.push(skipped(Skip::AlreadyLoaded { first }));
            return bare(l, None);
        }
        if kind(cx, path) == Some(EntryKind::Dir) {
            // `readFile` fails with EISDIR and the CLI skips the path without a word.
            let l = self.push(skipped(Skip::Invalid {
                why: Invalid::NotRegular,
            }));
            return bare(l, None);
        }
        let Ok(bytes) = cx.fs.read(path) else {
            let l = self.push(skipped(Skip::Missing));
            return bare(l, None);
        };
        self.identities.insert(identity, LoadId(id.0));

        let text = String::from_utf8_lossy(&bytes).into_owned();
        let dir = path.parent().unwrap_or_else(AbsPath::root);
        let tree = expand(path, &ImportRules::gemini(self.w.import_root(&dir)), cx.fs);
        if let Some(max) = tree.capped {
            self.diagnose(
                Subject::File {
                    path: path.clone(),
                    line: None,
                },
                Problem::ImportBudgetExhausted { max },
            );
        }
        let inlined = inline(&text, &tree.nodes, &|p| read_lossy(cx, p));
        let trimmed = js_trim(&inlined);
        if trimmed.is_empty() {
            let l = self.push(skipped(Skip::Empty));
            return bare(l, None);
        }
        // Tier 1 (global and private files) goes through the newline cleanup of
        // `promptProvider.ts`. The frame around the text belongs to the session block.
        let shown = match rule_id {
            "gemini.context.global" | "gemini.memory.private" => collapse_newlines(trimmed),
            _ => trimmed.to_owned(),
        };
        let delivered = Delivered {
            bytes: shown.len() as u64,
            tokens: tokens::count(&shown),
            transformed: shown != text,
        };
        let l = self.push(load(LoadStatus::Loaded, Some(delivered)));
        self.texts.insert(LoadId(l.0), shown.clone());
        let top = (path.clone(), LoadId(l.0));
        self.add_imports(LoadId(l.0), path, &scope, &tree.nodes, &mut vec![top]);
        bare(l, Some(shown))
    }

    fn add_imports(
        &mut self,
        host: LoadId,
        top: &AbsPath,
        scope: &Scope,
        nodes: &[ImportNode],
        chain: &mut Vec<(AbsPath, LoadId)>,
    ) {
        let cx = self.w.cx;
        for node in nodes {
            let subject = Subject::File {
                path: top.clone(),
                line: Some(node.line),
            };
            let raw = node.token.clone();
            let Some(path) = node.path.clone() else {
                self.diagnose(subject, Problem::ImportOutsideRoot { raw });
                continue;
            };
            let id = self.next_id();
            let this = id.0;
            let (status, delivered) = match node.outcome {
                ImportOutcome::Imported => {
                    // Spec 3: the file's text, with its own imports inlined, inside the host's
                    // text. That is what the model receives from it.
                    let raw = read_lossy(cx, &path).unwrap_or_default();
                    let text = inline(&raw, &node.children, &|p| read_lossy(cx, p));
                    let delivered = Delivered {
                        bytes: text.len() as u64,
                        tokens: tokens::count(&text),
                        transformed: text != raw,
                    };
                    self.texts.insert(id.clone(), text);
                    (LoadStatus::Loaded, Some(delivered))
                }
                ImportOutcome::Missing => {
                    let accidental = !raw.contains('/') && !raw.contains('.');
                    let problem = if accidental {
                        Problem::AccidentalImport { raw }
                    } else {
                        Problem::ImportMissing { raw }
                    };
                    self.diagnose(subject, problem);
                    (
                        LoadStatus::Skipped {
                            skip: Skip::Missing,
                        },
                        None,
                    )
                }
                ImportOutcome::OutsideRoot => {
                    self.diagnose(subject, Problem::ImportOutsideRoot { raw });
                    (
                        LoadStatus::Skipped {
                            skip: Skip::Invalid {
                                why: Invalid::OutsideRoot,
                            },
                        },
                        None,
                    )
                }
                ImportOutcome::Cycle | ImportOutcome::AlreadyLoaded => {
                    self.diagnose(subject, Problem::ImportCycle { path: path.clone() });
                    let first = chain
                        .iter()
                        .find(|(p, _)| *p == path)
                        .map_or_else(|| host.clone(), |(_, load)| load.clone());
                    (
                        LoadStatus::Skipped {
                            skip: Skip::AlreadyLoaded { first },
                        },
                        None,
                    )
                }
                _ => {
                    self.diagnose(subject, Problem::ImportMissing { raw });
                    (
                        LoadStatus::Skipped {
                            skip: Skip::Invalid {
                                why: Invalid::Unreadable { os: None },
                            },
                        },
                        None,
                    )
                }
            };
            self.push(Load {
                id,
                file: path.clone(),
                kind: DocKind::Instructions,
                scope: scope.clone(),
                via: Via::Import {
                    host: host.clone(),
                    line: node.line,
                    depth: node.depth.min(255) as u8,
                },
                when: Activation::WithHost,
                status,
                delivered,
            });
            if node.outcome == ImportOutcome::Imported {
                chain.push((path, LoadId(this)));
                self.add_imports(LoadId(this), top, scope, &node.children, chain);
                chain.pop();
            }
        }
    }

    fn diagnose(&mut self, at: Subject, problem: Problem) {
        self.diagnostics.push(Diagnostic {
            severity: Severity::Warning,
            agent: None,
            at,
            problem,
            fix: None,
        });
    }

    /// A file that the folder's trust keeps out of the session (spec 1, "Only loaded when
    /// trusted").
    pub fn add_untrusted(
        &mut self,
        path: &AbsPath,
        scope: Scope,
        when: Activation,
        rule_id: &'static str,
    ) {
        let id = self.next_id();
        self.push(Load {
            id,
            file: path.clone(),
            kind: DocKind::Instructions,
            scope,
            via: Via::Discovery {
                rule: rule(rule_id),
            },
            when,
            status: LoadStatus::Skipped {
                skip: Skip::Untrusted,
            },
            delivered: None,
        });
    }
}

/// `concatenateInstructions`, as inlined by the import processor (`importFormat: "tree"`).
pub(super) fn inline(
    text: &str,
    nodes: &[ImportNode],
    read: &dyn Fn(&AbsPath) -> Option<String>,
) -> String {
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    for node in nodes {
        out.push_str(&text[last..node.range.start]);
        last = node.range.end;
        let raw = node.token.strip_prefix('@').unwrap_or(&node.token);
        match (&node.path, node.outcome) {
            (Some(path), ImportOutcome::Imported) => {
                let child = read(path).unwrap_or_default();
                let body = inline(&child, &node.children, read);
                out.push_str(&format!(
                    "<!-- Imported from: {raw} -->\n{body}\n<!-- End of import from: {raw} -->"
                ));
            }
            (Some(_), ImportOutcome::Cycle | ImportOutcome::AlreadyLoaded) => {
                out.push_str(&format!("<!-- File already processed: {raw} -->"));
            }
            (Some(path), ImportOutcome::Missing) => out.push_str(&format!(
                "<!-- Import failed: {raw} - ENOENT: no such file or directory, access '{path}' -->"
            )),
            (Some(_), ImportOutcome::Unreadable | ImportOutcome::NotText) => {
                out.push_str(&format!(
                    "<!-- Import failed: {raw} - EISDIR: illegal operation on a directory, read -->"
                ))
            }
            _ => out.push_str(&format!(
                "<!-- Import failed: {raw} - Path traversal attempt -->"
            )),
        }
    }
    out.push_str(&text[last..]);
    out
}

/// Spec 1, global row: every configured name under `~/.gemini`, in list order.
pub(super) fn global_files(w: &World) -> Vec<AbsPath> {
    w.names
        .iter()
        .map(|name| w.gdir.join(name))
        .filter(|path| accessible(w.cx, path))
        .collect()
}

/// `findUpwardGeminiFiles`: from `start` up to `stop`, never past `~/.gemini`. Each hit comes
/// with the directory it was found in.
pub(super) fn upward(w: &World, start: &AbsPath, stop: &AbsPath) -> Vec<(AbsPath, AbsPath)> {
    let os = w.cx.env.os;
    let global_key = fold(os, w.gdir.as_str());
    let stop_key = fold(os, stop.as_str());
    let mut found: Vec<(AbsPath, AbsPath)> = Vec::new();
    let mut current = start.clone();
    loop {
        if fold(os, current.as_str()) == global_key {
            break;
        }
        let here: Vec<_> = w
            .names
            .iter()
            .map(|name| (current.join(name), current.clone()))
            .filter(|(path, _)| accessible(w.cx, path))
            .collect();
        found.splice(0..0, here);
        let parent = current.parent();
        if fold(os, current.as_str()) == stop_key {
            break;
        }
        match parent {
            Some(parent) => current = parent,
            None => break,
        }
    }
    found
}

pub(super) struct ProjectFiles {
    /// Sorted by UTF-16 code units of the absolute path (spec 1, concatenation order).
    pub files: Vec<AbsPath>,
    /// Paths dropped because another path is the same file, with the one that was kept.
    pub duplicates: Vec<(AbsPath, AbsPath)>,
}

/// Spec 1, project row: the upward walk from every workspace root, pooled, deduped and sorted.
pub(super) fn project_files(w: &World) -> ProjectFiles {
    let os = w.cx.env.os;
    let mut pool: Vec<AbsPath> = Vec::new();
    for root in &w.roots {
        let ceiling = w.boundary(root).unwrap_or_else(|| root.clone());
        pool.extend(upward(w, root, &ceiling).into_iter().map(|(path, _)| path));
    }
    let mut kept: HashMap<String, AbsPath> = HashMap::new();
    let mut files = Vec::new();
    let mut duplicates = Vec::new();
    for path in pool {
        let key = fold(os, real(w.cx, &path).as_str());
        match kept.get(&key) {
            Some(first) => duplicates.push((path, first.clone())),
            None => {
                kept.insert(key, path.clone());
                files.push(path);
            }
        }
    }
    files.sort_by(|a, b| cmp_utf16(a.as_str(), b.as_str()));
    ProjectFiles { files, duplicates }
}

/// Spec 6, "Extension context files": deduped case-insensitively, then sorted.
pub(super) fn extension_files(w: &World) -> Vec<(AbsPath, String)> {
    let mut seen = HashSet::new();
    let mut files: Vec<(AbsPath, String)> = Vec::new();
    for ext in w.exts.iter().filter(|e| e.active()) {
        for path in &ext.context_files {
            if seen.insert(path.as_str().to_lowercase()) {
                files.push((path.clone(), ext.name.clone()));
            }
        }
    }
    files.sort_by(|a, b| cmp_utf16(a.0.as_str(), b.0.as_str()));
    files
}

/// Every file the session start loads, by identity. JIT skips these.
fn session_identities(w: &World) -> HashSet<String> {
    let os = w.cx.env.os;
    let mut paths = global_files(w);
    paths.extend(extension_files(w).into_iter().map(|(p, _)| p));
    paths.extend(project_files(w).files);
    paths.extend(memory::private_files(w));
    paths
        .iter()
        .map(|p| fold(os, real(w.cx, p).as_str()))
        .collect()
}

/// Spec 1, subdirectory row: what the CLI adds when a tool touches a path in `dir`.
pub(super) fn jit_files(w: &World, dir: &AbsPath) -> Vec<(AbsPath, AbsPath)> {
    if !w.trusted() {
        return Vec::new();
    }
    let Some(root) = w.root_of(dir) else {
        return Vec::new();
    };
    let ceiling = w.boundary(root).unwrap_or_else(|| root.clone());
    let loaded = session_identities(w);
    upward(w, dir, &ceiling)
        .into_iter()
        .filter(|(path, _)| !loaded.contains(&fold(w.cx.env.os, real(w.cx, path).as_str())))
        .collect()
}

/// Builds every context-file load of the session and the JIT loads the walk found.
pub(super) fn gather(b: &mut Builder) -> Gathered {
    let w = b.w;
    let mut out = Gathered::default();
    for path in global_files(w) {
        let item = b.add_context_file(
            &path,
            Scope::User,
            DocKind::Instructions,
            "gemini.context.global",
            Activation::SessionStart,
        );
        out.global.push(item);
    }
    for (path, name) in extension_files(w) {
        let item = b.add_context_file(
            &path,
            Scope::Plugin { id: name },
            DocKind::Instructions,
            "gemini.context.extension",
            Activation::SessionStart,
        );
        out.extension.push(item);
    }
    let project = project_files(w);
    for path in &project.files {
        let scope = Scope::Project {
            dir: path.parent().unwrap_or_else(AbsPath::root),
        };
        if w.trusted() {
            let item = b.add_context_file(
                path,
                scope,
                DocKind::Instructions,
                "gemini.context.project",
                Activation::SessionStart,
            );
            out.project.push(item);
        } else {
            b.add_untrusted(
                path,
                scope,
                Activation::SessionStart,
                "gemini.context.project",
            );
        }
    }
    for (path, first) in &project.duplicates {
        let Some(first_id) = b
            .loads
            .iter()
            .find(|l| l.file == *first)
            .map(|l| l.id.clone())
        else {
            continue;
        };
        let id = b.next_id();
        b.push(Load {
            id,
            file: path.clone(),
            kind: DocKind::Instructions,
            scope: Scope::Project {
                dir: path.parent().unwrap_or_else(AbsPath::root),
            },
            via: Via::Discovery {
                rule: rule("gemini.context.project"),
            },
            when: Activation::SessionStart,
            status: LoadStatus::Skipped {
                skip: Skip::AlreadyLoaded { first: first_id },
            },
            delivered: None,
        });
    }
    for path in memory::private_files(w) {
        let item = b.add_context_file(
            &path,
            Scope::User,
            DocKind::MemoryIndex,
            "gemini.memory.private",
            Activation::SessionStart,
        );
        out.private.push(item);
    }
    // The JIT files the scan can see: what `jit_files` finds for every directory the walk
    // entered, for every workspace root. The scan and the simulator share that one rule.
    let mut seen: HashSet<String> = HashSet::new();
    let mut jit: Vec<(AbsPath, AbsPath)> = Vec::new();
    let mut capped = false;
    for root in &w.roots {
        let index = walk::walk(root, w.cx.fs);
        capped |= index.capped;
        for dir in index.dirs() {
            for (file, at) in jit_files(w, dir) {
                if seen.insert(fold(w.cx.env.os, real(w.cx, &file).as_str())) {
                    jit.push((file, at));
                }
            }
        }
    }
    jit.sort_by(|a, b| cmp_utf16(a.0.as_str(), b.0.as_str()));
    if capped {
        b.diagnostics.push(Diagnostic {
            severity: Severity::Info,
            agent: None,
            at: Subject::Agent,
            problem: Problem::WalkCapped {
                entries: walk::MAX_ENTRIES as u32,
            },
            fix: None,
        });
    }
    for (path, dir) in jit {
        let scope = Scope::Project { dir: dir.clone() };
        let when = Activation::OnAccess {
            rule: AccessRule::UnderDir { dir },
        };
        let item = b.add_context_file(
            &path,
            scope,
            DocKind::Instructions,
            "gemini.context.jit",
            when,
        );
        out.jit.push(item);
    }
    out
}
