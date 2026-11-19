//! `AGENTS.md` and the config keys that inject instructions (spec 1 and 5.5).

use super::out::{Block, BlockPart, NewLoad, Out, delivered, discovery};
use super::setup::{Layer, Setup, get_str, read_lossy};
use crate::agents::ScanCx;
use crate::limits::cut_bytes_lossy;
use crate::model::*;
use crate::probe_fs::EntryKind;
use crate::walk;

const OVERRIDE: &str = "AGENTS.override.md";
const DEFAULT: &str = "AGENTS.md";
/// Spec 1.4: the separator on the first switch from user text to project text.
const PROJECT_SEPARATOR: &str = "\n\n--- project-doc ---\n\n";

pub(super) const RULE_GLOBAL: &str = "codex.agents_md.global";
pub(super) const RULE_PROJECT: &str = "codex.agents_md.project";
pub(super) const RULE_NESTED: &str = "codex.agents_md.below_cwd";
pub(super) const RULE_MODEL_FILE: &str = "codex.config.model_instructions_file";
pub(super) const RULE_DEVELOPER: &str = "codex.config.developer_instructions";
pub(super) const RULE_PROMPTS: &str = "codex.prompts.legacy";

fn is_file(cx: &ScanCx, path: &AbsPath) -> bool {
    cx.fs
        .stat(path)
        .map(|s| s.kind == EntryKind::File)
        .unwrap_or(false)
}

fn blank(text: &str) -> bool {
    text.trim().is_empty()
}

pub(super) fn run(cx: &ScanCx, setup: &Setup, out: &mut Out) {
    // Config-injected text comes first in delivery order (spec 5.5, observed with the oracle).
    model_instructions(cx, setup, out);
    developer_instructions(setup, out);
    let global = global(cx, setup, out);
    let project = project_chain(cx, setup, out);
    nested(cx, setup, out);
    legacy_prompts(cx, setup, out);
    agents_block(setup, global, project, out);
}

/// Spec 1.1, global row: the first non-empty of override then `AGENTS.md`. Content is trimmed.
fn global(cx: &ScanCx, setup: &Setup, out: &mut Out) -> Option<(LoadId, String)> {
    let mut chosen: Option<(LoadId, String, AbsPath)> = None;
    for name in [OVERRIDE, DEFAULT] {
        let path = setup.codex_home.join(name);
        out.claim(&path, EditPolicy::Editable);
        if !is_file(cx, &path) {
            continue;
        }
        let Some((bytes, text)) = read_lossy(cx, &path) else {
            continue;
        };
        let trimmed = text.trim();
        let (status, deliv) = match &chosen {
            Some((_, _, by)) => (
                LoadStatus::Skipped {
                    skip: Skip::Shadowed {
                        by: by.clone(),
                        how: Shadow::FirstNonEmpty,
                    },
                },
                None,
            ),
            None if trimmed.is_empty() => (LoadStatus::Skipped { skip: Skip::Empty }, None),
            None => (
                LoadStatus::Loaded,
                Some(delivered(trimmed, trimmed.len() as u64)),
            ),
        };
        let loaded = matches!(status, LoadStatus::Loaded);
        let id = out.push_load(NewLoad {
            file: path.clone(),
            kind: DocKind::Instructions,
            scope: Scope::User,
            via: discovery(RULE_GLOBAL),
            when: Activation::SessionStart,
            status,
            delivered: deliv,
        });
        let _ = bytes;
        if loaded {
            chosen = Some((id, trimmed.to_owned(), path));
        }
    }
    chosen.map(|(id, text, _)| (id, text))
}

/// Spec 1.5 and the candidate list of `candidate_filenames`.
fn candidate_names(setup: &Setup) -> Vec<String> {
    let mut names = vec![OVERRIDE.to_owned(), DEFAULT.to_owned()];
    for name in &setup.fallback_names {
        let bad = name.is_empty() || name == "." || name == ".." || name.contains(['/', '\0']);
        if !bad && !names.contains(name) {
            names.push(name.clone());
        }
    }
    names
}

/// The directories from the project root down to cwd, root first (spec 1.1).
fn chain_dirs(setup: &Setup) -> Vec<AbsPath> {
    if !setup.md_root_found {
        return vec![setup.cwd.clone()];
    }
    let mut dirs = Vec::new();
    let mut cursor = setup.cwd.clone();
    loop {
        dirs.push(cursor.clone());
        if cursor == setup.md_root {
            break;
        }
        let Some(parent) = cursor.parent() else { break };
        cursor = parent;
    }
    dirs.reverse();
    dirs
}

/// Spec 1.1 and 1.3: one file per directory, one shared byte budget.
fn project_chain(cx: &ScanCx, setup: &Setup, out: &mut Out) -> Vec<(LoadId, String)> {
    let names = candidate_names(setup);
    let limit = Limit {
        kind: LimitKind::ProjectDocBudget,
        value: setup.project_doc_max_bytes,
        unit: Unit::Bytes,
    };
    let mut remaining = setup.project_doc_max_bytes;
    let mut loaded = Vec::new();
    let mut untrusted_seen = false;

    for dir in chain_dirs(setup) {
        let scope = Scope::Project { dir: dir.clone() };
        let found: Vec<AbsPath> = names
            .iter()
            .map(|n| dir.join(n))
            .filter(|p| is_file(cx, p))
            .collect();
        for name in &names {
            out.claim(&dir.join(name), EditPolicy::Editable);
        }
        let Some(first) = found.first().cloned() else {
            continue;
        };

        // The chosen file is read once, whole.
        let read = read_lossy(cx, &first);
        let whole_blank = read.as_ref().is_none_or(|(_, text)| blank(text));
        let (status, deliv, text) = if setup.md_untrusted {
            untrusted_seen = true;
            (
                LoadStatus::Skipped {
                    skip: Skip::Untrusted,
                },
                None,
                None,
            )
        } else if let Some((bytes, _)) = read.as_ref().filter(|_| remaining > 0) {
            // Spec 1.3: the crossing file is cut at the byte, lossily decoded.
            let cut = cut_bytes_lossy(bytes, remaining);
            if blank(&cut.text) {
                (LoadStatus::Skipped { skip: Skip::Empty }, None, None)
            } else {
                remaining -= cut.kept;
                let deliv = delivered(&cut.text, cut.kept);
                let status = if cut.is_whole() {
                    LoadStatus::Loaded
                } else {
                    out.warn_file(
                        &first,
                        Problem::Truncated {
                            limit: limit.clone(),
                            kept: cut.kept,
                            total: cut.total,
                        },
                    );
                    LoadStatus::Truncated {
                        limit: limit.clone(),
                        kept: cut.kept,
                        total: cut.total,
                    }
                };
                (status, Some(deliv), Some(cut.text))
            }
        } else if read.is_none() {
            (
                LoadStatus::Skipped {
                    skip: Skip::Invalid {
                        detail: "unreadable".to_owned(),
                    },
                },
                None,
                None,
            )
        } else if whole_blank {
            (LoadStatus::Skipped { skip: Skip::Empty }, None, None)
        } else {
            // Spec 1.3: every later file is dropped once the budget is spent.
            out.warn_file(
                &first,
                Problem::DroppedByBudget {
                    limit: limit.clone(),
                },
            );
            (
                LoadStatus::Skipped {
                    skip: Skip::BudgetExhausted {
                        limit: limit.clone(),
                    },
                },
                None,
                None,
            )
        };
        let id = out.push_load(NewLoad {
            file: first.clone(),
            kind: DocKind::Instructions,
            scope: scope.clone(),
            via: discovery(RULE_PROJECT),
            when: Activation::SessionStart,
            status,
            delivered: deliv,
        });
        if let Some(text) = text {
            loaded.push((id, text));
        }

        // Spec 1.1: an empty override still takes the directory.
        let first_is_empty_override = first.file_name() == Some(OVERRIDE) && whole_blank;
        for other in found.iter().skip(1) {
            if first_is_empty_override {
                out.diagnose(
                    Severity::Warning,
                    Subject::File {
                        path: first.clone(),
                        line: None,
                    },
                    Problem::ShadowedByEmptyOverride,
                );
            } else if setup.md_untrusted {
                // A shadowed file is also never read when the project is untrusted.
            }
            out.push_load(NewLoad {
                file: other.clone(),
                kind: DocKind::Instructions,
                scope: scope.clone(),
                via: discovery(RULE_PROJECT),
                when: Activation::SessionStart,
                status: LoadStatus::Skipped {
                    skip: Skip::Shadowed {
                        by: first.clone(),
                        how: if first_is_empty_override {
                            Shadow::EmptyOverride
                        } else {
                            Shadow::OnePerDirectory
                        },
                    },
                },
                delivered: None,
            });
            if first_is_empty_override {
                break;
            }
        }
    }
    if untrusted_seen {
        out.diagnose(
            Severity::Info,
            Subject::Agent,
            Problem::ProjectUntrusted {
                skipped: vec![RuleRef::new(RULE_PROJECT)],
            },
        );
    }
    loaded
}

/// Spec 1.1: `AGENTS.md` below cwd is never loaded. The model may read it with its tools.
fn nested(cx: &ScanCx, setup: &Setup, out: &mut Out) {
    let index = walk::walk(&setup.cwd, cx.fs);
    for dir in index.dirs_containing(DEFAULT) {
        if dir == &setup.cwd {
            continue;
        }
        let path = dir.join(DEFAULT);
        let Some((bytes, text)) = read_lossy(cx, &path) else {
            continue;
        };
        out.push_load(NewLoad {
            file: path,
            kind: DocKind::Instructions,
            scope: Scope::Project { dir: dir.clone() },
            via: discovery(RULE_NESTED),
            when: Activation::ModelMayRead,
            status: LoadStatus::Loaded,
            delivered: Some(delivered(&text, bytes.len() as u64)),
        });
    }
}

/// Spec 5.5: `model_instructions_file` replaces the built-in base instructions.
fn model_instructions(cx: &ScanCx, setup: &Setup, out: &mut Out) {
    let Some((layer, value)) = setup.layer_setting(&["model_instructions_file"]) else {
        return;
    };
    let Some(raw) = value.as_str() else { return };
    let path = setup.path_in_layer(layer, raw);
    out.claim(&path, EditPolicy::Editable);
    let (status, deliv, text) = match read_lossy(cx, &path) {
        None => (
            LoadStatus::Skipped {
                skip: Skip::Missing,
            },
            None,
            None,
        ),
        Some((_, text)) if blank(&text) => (LoadStatus::Skipped { skip: Skip::Empty }, None, None),
        // Codex trims the file (`try_read_non_empty_file`).
        Some((bytes, text)) => (
            LoadStatus::Loaded,
            Some(delivered(text.trim(), bytes.len() as u64)),
            Some(text.trim().to_owned()),
        ),
    };
    let id = out.push_load(NewLoad {
        file: path,
        kind: DocKind::Instructions,
        scope: layer.scope(),
        via: discovery(RULE_MODEL_FILE),
        when: Activation::SessionStart,
        status,
        delivered: deliv,
    });
    if let Some(text) = text {
        out.blocks.push(Block {
            delivery: Delivery::SystemPrompt,
            label: BlockLabel::Instructions,
            parts: vec![BlockPart::Load(id, text)],
        });
    }
}

/// Spec 5.5: `developer_instructions` is a developer-role message. The load names the config
/// file that sets it.
fn developer_instructions(setup: &Setup, out: &mut Out) {
    let Some((layer, value)) = setup.layer_setting(&["developer_instructions"]) else {
        return;
    };
    let Some(text) = value.as_str() else { return };
    let (status, deliv) = if blank(text) {
        (LoadStatus::Skipped { skip: Skip::Empty }, None)
    } else {
        (LoadStatus::Loaded, Some(delivered(text, text.len() as u64)))
    };
    let loaded = deliv.is_some();
    let id = out.push_load(NewLoad {
        file: layer.config_path.clone(),
        kind: DocKind::Instructions,
        scope: layer.scope(),
        via: discovery(RULE_DEVELOPER),
        when: Activation::SessionStart,
        status,
        delivered: deliv,
    });
    if loaded {
        out.blocks.push(Block {
            delivery: Delivery::DeveloperMessage,
            label: BlockLabel::Instructions,
            parts: vec![BlockPart::Load(id, text.to_owned())],
        });
    }
}

/// Spec 5.2: `$CODEX_HOME/prompts/*.md` has no loader in 0.165.0.
fn legacy_prompts(cx: &ScanCx, setup: &Setup, out: &mut Out) {
    let dir = setup.codex_home.join("prompts");
    let Ok(entries) = cx.fs.list(&dir) else {
        return;
    };
    for entry in entries
        .iter()
        .filter(|e| e.kind == EntryKind::File && e.name.ends_with(".md"))
    {
        let path = dir.join(&entry.name);
        let delivered_info =
            read_lossy(cx, &path).map(|(bytes, text)| delivered(&text, bytes.len() as u64));
        out.claim(&path, EditPolicy::Editable);
        let why = RuleRef::new(RULE_PROMPTS);
        out.push_load(NewLoad {
            file: path.clone(),
            kind: DocKind::CommandBody,
            scope: Scope::User,
            via: discovery(RULE_PROMPTS),
            when: Activation::OnInvoke,
            status: LoadStatus::Skipped {
                skip: Skip::NotRead { why: why.clone() },
            },
            delivered: None,
        });
        let _ = delivered_info;
        out.warn_file(&path, Problem::LegacyLocation { why });
    }
}

/// Spec 1.4: the user-role message that carries every loaded `AGENTS.md`.
fn agents_block(
    setup: &Setup,
    global: Option<(LoadId, String)>,
    project: Vec<(LoadId, String)>,
    out: &mut Out,
) {
    if global.is_none() && project.is_empty() {
        return;
    }
    let mut parts = Vec::new();
    let header = if project.is_empty() {
        "# AGENTS.md instructions\n\n<INSTRUCTIONS>\n".to_owned()
    } else {
        format!(
            "# AGENTS.md instructions for {}\n\n<INSTRUCTIONS>\n",
            setup.cwd
        )
    };
    parts.push(BlockPart::Text(header));
    let mut has_previous = false;
    if let Some((id, text)) = global {
        parts.push(BlockPart::Load(id, text));
        has_previous = true;
    }
    for (index, (id, text)) in project.into_iter().enumerate() {
        if has_previous {
            parts.push(BlockPart::Text(
                if index == 0 {
                    PROJECT_SEPARATOR
                } else {
                    "\n\n"
                }
                .to_owned(),
            ));
        }
        parts.push(BlockPart::Load(id, text));
        has_previous = true;
    }
    parts.push(BlockPart::Text("\n</INSTRUCTIONS>".to_owned()));
    out.blocks.push(Block {
        delivery: Delivery::UserMessage,
        label: BlockLabel::Instructions,
        parts,
    });
}

#[allow(dead_code)]
pub(super) fn config_string<'a>(layer: &'a Layer, key: &str) -> Option<&'a str> {
    get_str(layer.config.as_ref()?, &[key])
}
