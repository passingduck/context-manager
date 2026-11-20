//! Memories (spec 3): which root, whether it is on, the one injected file, the read-only rest.

use super::out::{Block, NewLoad, Out, delivered, discovery};
use super::setup::{Setup, get_bool, get_path, get_str};
use crate::agents::ScanCx;
use crate::limits::cut_tokens;
use crate::model::*;
use crate::probe_fs::EntryKind;

pub(super) const RULE_STORE: &str = "codex.memory.store";
pub(super) const RULE_SUMMARY: &str = "codex.memory.summary";
/// Spec 3.2: `MEMORY_TOOL_DEVELOPER_INSTRUCTIONS_SUMMARY_TOKEN_LIMIT`.
const SUMMARY_TOKENS: u32 = 2_500;

fn is_dir(cx: &ScanCx, path: &AbsPath) -> bool {
    cx.fs
        .stat(path)
        .map(|s| s.kind == EntryKind::Dir)
        .unwrap_or(false)
}

/// Spec 3.1, "Version selection".
fn root(cx: &ScanCx, setup: &Setup) -> (AbsPath, &'static str) {
    let v1 = setup.codex_home.join("memories");
    let v2 = setup.codex_home.join("memories_v2");
    let version = get_str(&setup.merged, &["memories", "version"]);
    let dual_write = get_bool(&setup.merged, &["memories", "dual_write"]) != Some(false);
    match version {
        Some("v2") => (v2, "v2"),
        Some("v1") => (v1, "v1"),
        _ if setup.features.memories && dual_write && !is_dir(cx, &v1) => (v2, "v2"),
        _ => (v1, "v1"),
    }
}

fn md_files(cx: &ScanCx, dir: &AbsPath) -> Vec<AbsPath> {
    cx.fs
        .list(dir)
        .map(|entries| {
            entries
                .iter()
                .filter(|e| e.kind == EntryKind::File && e.name.ends_with(".md"))
                .map(|e| dir.join(&e.name))
                .collect()
        })
        .unwrap_or_default()
}

fn title_of(path: &AbsPath) -> String {
    path.file_name().unwrap_or("").to_owned()
}

pub(super) fn run(cx: &ScanCx, setup: &Setup, out: &mut Out) {
    let (root, version) = root(cx, setup);
    let exists = is_dir(cx, &root);
    let enabled = setup.features.memories;
    let use_memories = get_bool(&setup.merged, &["memories", "use_memories"]).unwrap_or(true);
    if !exists && !enabled {
        return;
    }

    let summary = root.join("memory_summary.md");
    let index = root.join("MEMORY.md");
    let notes = root.join("extensions/ad_hoc/notes");
    let mut entries = Vec::new();
    let entry = |file: AbsPath, role: MemoryRole| MemoryEntry {
        title: title_of(&file),
        file,
        role,
        indexed: None,
        description: None,
    };
    for (file, role) in [
        (summary.clone(), MemoryRole::Summary),
        (index.clone(), MemoryRole::Index),
        (root.join("raw_memories.md"), MemoryRole::Generated),
        (
            root.join("extensions/ad_hoc/instructions.md"),
            MemoryRole::Generated,
        ),
    ] {
        if cx.fs.stat(&file).is_ok() {
            entries.push(entry(file, role));
        }
    }
    for file in md_files(cx, &root.join("rollout_summaries")) {
        entries.push(entry(file, MemoryRole::Generated));
    }
    for file in md_files(cx, &notes) {
        entries.push(entry(file, MemoryRole::Note));
    }
    // Memory-generated skills live under `<root>/skills/<name>/SKILL.md`.
    if let Ok(skills) = cx.fs.list(&root.join("skills")) {
        for skill in skills.iter().filter(|e| e.kind == EntryKind::Dir) {
            let file = root.join("skills").join(&skill.name).join("SKILL.md");
            if cx.fs.stat(&file).is_ok() {
                entries.push(entry(file, MemoryRole::Generated));
            }
        }
    }
    for e in &entries {
        let why = match e.role {
            MemoryRole::Note => EditPolicy::Editable,
            _ => EditPolicy::ReadOnly {
                why: ReadOnlyWhy::Generated,
            },
        };
        out.claim(&e.file, why);
    }
    out.claim(
        &summary,
        EditPolicy::ReadOnly {
            why: ReadOnlyWhy::Generated,
        },
    );

    out.memory.push(MemoryStore {
        id: "codex.memories".to_owned(),
        label: RuleRef::new(RULE_STORE),
        root: root.clone(),
        enabled: if enabled {
            Enabled::On
        } else {
            Enabled::Off {
                by: "features.memories".to_owned(),
            }
        },
        index: cx.fs.stat(&index).is_ok().then_some(index),
        entries,
        // Spec 3.2: notes are the intended edit surface. Generated files are overwritten.
        write: MemoryWrite::NotesDir { dir: notes },
    });
    out.assumptions.push(Assumption::Mode {
        name: "memories.version".to_owned(),
        value: version.to_owned(),
        set_in: None,
    });

    // The summary is the only file injected (spec 3.2), and only when both switches are on.
    let Some((bytes, text)) = super::setup::read_lossy(cx, &summary) else {
        if enabled && use_memories {
            out.push_load(NewLoad {
                file: summary,
                kind: DocKind::MemorySummary,
                scope: Scope::User,
                via: discovery(RULE_SUMMARY),
                when: Activation::SessionStart,
                status: LoadStatus::Skipped {
                    skip: Skip::Missing,
                },
                delivered: None,
            });
        }
        return;
    };
    let trimmed = text.trim();
    let _ = get_path(&setup.merged, &["memories"]);
    let (status, deliv, shown) = if !enabled || !use_memories {
        (
            LoadStatus::Skipped {
                skip: Skip::FeatureOff {
                    name: if enabled {
                        "memories.use_memories"
                    } else {
                        "features.memories"
                    }
                    .to_owned(),
                },
            },
            None,
            None,
        )
    } else if trimmed.is_empty() {
        (LoadStatus::Skipped { skip: Skip::Empty }, None, None)
    } else {
        let cut = cut_tokens(trimmed, SUMMARY_TOKENS);
        let deliv = delivered(&cut.text, bytes.len().min(cut.text.len()) as u64);
        let status = if cut.is_whole() {
            LoadStatus::Loaded
        } else {
            out.warn_file(
                &summary,
                Problem::Truncated {
                    limit: summary_limit(),
                    kept: cut.kept,
                    total: cut.total,
                },
            );
            LoadStatus::Truncated {
                limit: summary_limit(),
                kept: cut.kept,
                total: cut.total,
            }
        };
        (status, Some(deliv), Some(cut.text))
    };
    let id = out.push_load(NewLoad {
        file: summary,
        kind: DocKind::MemorySummary,
        scope: Scope::User,
        via: discovery(RULE_SUMMARY),
        when: Activation::SessionStart,
        status,
        delivered: deliv,
    });
    if let Some(shown) = shown {
        out.blocks.push(Block::one(
            Delivery::DeveloperMessage,
            BlockLabel::Memory,
            id,
            shown,
        ));
    }
}

fn summary_limit() -> Limit {
    Limit {
        kind: LimitKind::SummaryTokens,
        value: u64::from(SUMMARY_TOKENS),
        unit: Unit::Tokens,
    }
}
