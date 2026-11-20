//! The session-start text (spec 1.2, "How it reaches the model"): one `<system-reminder>` in
//! the first user message, with a `Contents of <path> (<label>):` block per file.

use serde_json::Value;

use super::catalog;
use super::memory::cut_index;
use super::setup::Setup;
use crate::agents::ScanCx;
use crate::md::strip::strip;
use crate::model::*;
use crate::tokens;

/// Spec 1.2: the sentence that opens the block.
pub(super) const INTRO: &str = "Codebase and user instructions are shown below. Be sure to adhere to these instructions. IMPORTANT: These instructions OVERRIDE any default behavior and you MUST follow them exactly as written.";

/// The pseudo-path of a managed `claudeMd` setting (spec 1.1).
const MANAGED_SETTINGS_PATH: &str = "<managed-settings>";

pub(super) struct Built {
    pub rendered: RenderedBlock,
    pub block: ContextBlock,
}

/// Spec 1.2, label table.
fn label_of(load: &Load) -> &'static str {
    if load.kind == DocKind::MemoryIndex {
        return "user's auto-memory, persists across conversations";
    }
    match load.scope {
        Scope::User => "user's private global instructions for all projects",
        Scope::Local => "user's private project instructions, not checked in",
        Scope::Managed => "organization-managed policy instructions",
        _ => "project instructions, checked into the codebase",
    }
}

/// Whether the load reaches the model at session start: its own activation, or its host's.
fn at_start(loads: &[Load], load: &Load) -> bool {
    match (&load.when, &load.via) {
        (Activation::SessionStart, _) => true,
        (Activation::WithHost, Via::Import { host, .. }) => loads
            .get(host.0 as usize)
            .is_some_and(|host| at_start(loads, host)),
        _ => false,
    }
}

fn delivered(load: &Load) -> bool {
    matches!(
        load.status,
        LoadStatus::Loaded | LoadStatus::Truncated { .. }
    )
}

/// The text of one load as Claude Code sends it, without its header.
fn text_of(cx: &ScanCx, load: &Load) -> Option<String> {
    let bytes = cx.fs.read(&load.file).ok()?;
    let raw = String::from_utf8_lossy(&bytes);
    if matches!(&load.via, Via::Discovery { rule } if rule.0 == "claude.discovery.managed_setting")
    {
        let value: Value = serde_json::from_str(&raw).ok()?;
        return Some(value.get("claudeMd")?.as_str()?.trim().to_owned());
    }
    Some(match load.kind {
        DocKind::MemoryIndex => cut_index(&raw).content,
        _ => strip(&raw).text.trim().to_owned(),
    })
}

fn shown_path(cx: &ScanCx, load: &Load) -> String {
    if matches!(&load.via, Via::Discovery { rule } if rule.0 == "claude.discovery.managed_setting")
    {
        return MANAGED_SETTINGS_PATH.to_owned();
    }
    // Spec 1.2: a symlinked file shows its target path.
    match cx.fs.stat(&load.file) {
        Ok(stat) if stat.is_symlink => cx
            .fs
            .real_path(&load.file)
            .map_or_else(|_| load.file.to_string(), |p| p.to_string()),
        _ => load.file.to_string(),
    }
}

/// Everything that is sent at session start: the instruction and memory blocks, then the skill
/// and subagent listings.
pub(super) fn session(
    cx: &ScanCx,
    setup: &Setup,
    loads: &[Load],
    extensions: &[Extension],
) -> Vec<Built> {
    let mut blocks = build(cx, loads);
    blocks.extend(catalog::build(cx, setup, extensions));
    blocks
}

/// The instruction block and the memory block of the first user message. The reminder opens in
/// the first block and closes in the last.
pub(super) fn build(cx: &ScanCx, loads: &[Load]) -> Vec<Built> {
    let mut files: Vec<(LoadId, String)> = Vec::new();
    let mut memory: Option<(LoadId, String)> = None;
    for load in loads {
        let session_text = matches!(
            load.kind,
            DocKind::Instructions | DocKind::Rule | DocKind::MemoryIndex
        );
        if !session_text || !delivered(load) || !at_start(loads, load) {
            continue;
        }
        let Some(text) = text_of(cx, load) else {
            continue;
        };
        let piece = format!(
            "Contents of {} ({}):\n\n{text}",
            shown_path(cx, load),
            label_of(load)
        );
        if load.kind == DocKind::MemoryIndex {
            memory = Some((load.id.clone(), piece));
        } else {
            files.push((load.id.clone(), piece));
        }
    }
    if files.is_empty() && memory.is_none() {
        return Vec::new();
    }
    let mut first = format!("<system-reminder>\n{INTRO}");
    for (_, piece) in &files {
        first.push_str("\n\n");
        first.push_str(piece);
    }
    let mut out = Vec::new();
    let mut second = None;
    match &memory {
        Some((_, piece)) => second = Some(format!("{piece}\n</system-reminder>")),
        None => first.push_str("\n</system-reminder>"),
    }
    out.push(built(
        BlockLabel::Instructions,
        first,
        files
            .iter()
            .map(|(id, piece)| (id.clone(), piece.as_str()))
            .collect(),
        Fidelity::Exact,
    ));
    if let (Some(text), Some((id, piece))) = (second, &memory) {
        out.push(built(
            BlockLabel::Memory,
            text,
            vec![(id.clone(), piece.as_str())],
            Fidelity::Exact,
        ));
    }
    out
}

pub(super) fn built(
    label: BlockLabel,
    text: String,
    pieces: Vec<(LoadId, &str)>,
    fidelity: Fidelity,
) -> Built {
    let total = tokens::count(&text);
    let mut parts: Vec<Part> = Vec::new();
    let mut in_loads = 0u32;
    let mut load_parts = Vec::new();
    for (id, piece) in pieces {
        let t = tokens::count(piece);
        in_loads += t;
        load_parts.push(Part::Load {
            load: id,
            tokens: t,
        });
    }
    parts.push(Part::Glue {
        tokens: total.saturating_sub(in_loads),
    });
    parts.extend(load_parts);
    Built {
        rendered: RenderedBlock {
            delivery: Delivery::UserMessage,
            label,
            text: text.clone(),
            tokens: total,
            fidelity,
        },
        block: ContextBlock {
            delivery: Delivery::UserMessage,
            label,
            parts,
            tokens: total,
            fidelity,
            preceded_by: Vec::new(),
        },
    }
}
