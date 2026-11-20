//! Private project memory (spec 4): `~/.gemini/tmp/<slug>/memory/`.

use serde_json::Value;

use super::context::Builder;
use super::util::{accessible, is_file, read_lossy, rule};
use super::world::World;
use crate::agents::{FileClaim, ScanCx};
use crate::md::frontmatter;
use crate::model::*;
use crate::probe_fs::EntryKind;
use crate::tokens;
use crate::walk;

pub(super) const INDEX: &str = "MEMORY.md";

/// Spec 4, slug row: `<runtime>/projects.json` is `{"projects": {"<abs cwd>": "<slug>"}}`.
/// This never creates an entry.
pub(super) fn slug_in(cx: &ScanCx, runtime: &AbsPath, cwd: &AbsPath) -> Option<String> {
    let text = read_lossy(cx, &runtime.join("projects.json"))?;
    let value: Value = serde_json::from_str(&text).ok()?;
    value
        .get("projects")?
        .get(cwd.as_str())?
        .as_str()
        .filter(|slug| !slug.is_empty())
        .map(str::to_owned)
}

/// The runtime directory under macOS Seatbelt (spec 7).
pub(super) fn sandbox_runtime(w: &World) -> AbsPath {
    w.home.join(".cache/.gemini")
}

pub(super) fn memory_dir(w: &World) -> Option<AbsPath> {
    let slug = slug_in(w.cx, &w.gdir, &w.cwd)?;
    Some(w.gdir.join("tmp").join(&slug).join("memory"))
}

pub(super) fn sandbox_memory_dir(w: &World) -> Option<AbsPath> {
    let runtime = sandbox_runtime(w);
    let slug = slug_in(w.cx, &runtime, &w.cwd)?;
    Some(runtime.join("tmp").join(&slug).join("memory"))
}

/// `getUserProjectMemoryPaths`: `MEMORY.md`, or else every configured name in the folder.
pub(super) fn private_files(w: &World) -> Vec<AbsPath> {
    let Some(dir) = memory_dir(w) else {
        return Vec::new();
    };
    let index = dir.join(INDEX);
    if accessible(w.cx, &index) {
        return vec![index];
    }
    w.names
        .iter()
        .map(|name| dir.join(name))
        .filter(|path| accessible(w.cx, path))
        .collect()
}

pub(super) struct Stores {
    pub stores: Vec<MemoryStore>,
    pub claims: Vec<FileClaim>,
}

pub(super) fn stores(w: &World) -> Stores {
    let mut out = Stores {
        stores: Vec::new(),
        claims: Vec::new(),
    };
    let cx = w.cx;
    for runtime in [&w.gdir, &sandbox_runtime(w)] {
        let registry = runtime.join("projects.json");
        if is_file(cx, &registry) {
            out.claims.push(FileClaim {
                path: registry,
                policy: EditPolicy::ReadOnly {
                    why: ReadOnlyWhy::AgentState,
                },
            });
        }
    }
    let primary = memory_dir(w);
    let sandbox = sandbox_memory_dir(w);
    for (id, label, dir, enabled) in [
        (
            "gemini.private",
            "gemini.memory.private",
            primary,
            Enabled::On,
        ),
        (
            "gemini.private.sandbox",
            "gemini.memory.private_sandbox",
            sandbox,
            Enabled::Off {
                by: "SANDBOX".to_owned(),
            },
        ),
    ] {
        let Some(dir) = dir else { continue };
        let mut store = MemoryStore {
            id: id.to_owned(),
            label: rule(label),
            root: dir.clone(),
            enabled,
            index: None,
            entries: Vec::new(),
            write: MemoryWrite::Direct,
        };
        fill(w, &dir, &mut store, &mut out.claims);
        out.stores.push(store);
    }
    out
}

fn fill(w: &World, dir: &AbsPath, store: &mut MemoryStore, claims: &mut Vec<FileClaim>) {
    let cx = w.cx;
    let project_root = dir.parent().map(|d| d.join(".project_root"));
    if let Some(marker) = project_root.filter(|m| is_file(cx, m)) {
        claims.push(FileClaim {
            path: marker,
            policy: EditPolicy::ReadOnly {
                why: ReadOnlyWhy::AgentState,
            },
        });
    }
    let index_path = dir.join(INDEX);
    let index_text = read_lossy(cx, &index_path);
    if index_text.is_some() {
        store.index = Some(index_path.clone());
    } else if let Some(legacy) = w.names.iter().map(|n| dir.join(n)).find(|p| is_file(cx, p)) {
        store.index = Some(legacy);
    }
    let index_body = store
        .index
        .as_ref()
        .and_then(|p| read_lossy(cx, p))
        .unwrap_or_default();
    for entry in walk::list_dir(cx.fs, dir) {
        let path = entry.path.clone();
        match entry.kind {
            EntryKind::File if entry.name().ends_with(".md") => {
                let is_index = Some(&path) == store.index.as_ref();
                let text = read_lossy(cx, &path).unwrap_or_default();
                let (title, description) = describe(&text, entry.name());
                store.entries.push(MemoryEntry {
                    file: path,
                    role: if is_index {
                        MemoryRole::Index
                    } else {
                        MemoryRole::Note
                    },
                    indexed: (!is_index).then(|| index_body.contains(entry.name())),
                    title,
                    description,
                });
            }
            EntryKind::File
                if entry.name() == ".extraction-state.json"
                    || entry.name() == ".extraction.lock" =>
            {
                claims.push(FileClaim {
                    path: path.clone(),
                    policy: EditPolicy::ReadOnly {
                        why: ReadOnlyWhy::AgentState,
                    },
                });
                store.entries.push(generated(path, entry.name()));
            }
            EntryKind::Dir if entry.name() == ".inbox" || entry.name() == "skills" => {
                generated_below(w, &path, store, claims);
            }
            _ => {}
        }
    }
}

/// Everything under `.inbox` and `skills/`, two directories deep, belongs to the Auto Memory
/// service (spec 4, Auto Memory row; spec 8): the tool lists it and never edits it.
fn generated_below(w: &World, dir: &AbsPath, store: &mut MemoryStore, claims: &mut Vec<FileClaim>) {
    for entry in walk::list_tree(w.cx.fs, dir, |e| e.depth() <= 2) {
        if entry.kind != EntryKind::File {
            continue;
        }
        claims.push(FileClaim {
            path: entry.path.clone(),
            policy: EditPolicy::ReadOnly {
                why: ReadOnlyWhy::Generated,
            },
        });
        let name = entry.name().to_owned();
        store.entries.push(generated(entry.path, &name));
    }
}

fn generated(file: AbsPath, name: &str) -> MemoryEntry {
    MemoryEntry {
        file,
        role: MemoryRole::Generated,
        indexed: None,
        title: name.to_owned(),
        description: None,
    }
}

/// The first heading, else the file stem; and the frontmatter description.
fn describe(text: &str, name: &str) -> (String, Option<String>) {
    let fm = frontmatter::parse(text);
    let description = fm
        .map
        .as_ref()
        .and_then(|m| m.get("description")?.as_str().map(str::to_owned));
    let title = text[fm.body.clone()]
        .lines()
        .find_map(|line| line.strip_prefix("# ").map(|t| t.trim().to_owned()))
        .unwrap_or_else(|| name.trim_end_matches(".md").to_owned());
    (title, description)
}

/// Sibling notes are not loaded; the model may open them from the index (spec 4).
pub(super) fn note_loads(b: &mut Builder, stores: &Stores) {
    let Some(primary) = stores.stores.iter().find(|s| s.id == "gemini.private") else {
        return;
    };
    for entry in primary
        .entries
        .iter()
        .filter(|e| e.role == MemoryRole::Note)
    {
        let cx = b.w.cx;
        let text = read_lossy(cx, &entry.file).unwrap_or_default();
        let id = b.next_id();
        b.push(Load {
            id,
            file: entry.file.clone(),
            kind: DocKind::MemoryTopic,
            scope: Scope::User,
            via: Via::Discovery {
                rule: rule("gemini.memory.note"),
            },
            when: Activation::ModelMayRead,
            status: LoadStatus::Loaded,
            delivered: Some(Delivered {
                bytes: text.len() as u64,
                tokens: tokens::count(&text),
                transformed: false,
            }),
        });
    }
}
