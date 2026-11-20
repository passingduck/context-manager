//! The session-start text (spec 1.2, "How it reaches the model"): one `<system-reminder>` in
//! the first user message, with a `Contents of <path> (<label>):` block per file.

use serde_json::Value;

use super::catalog;
use super::extensions::body_of;
use super::instructions::delivered_text;
use super::memory::cut_index;
use super::setup::Setup;
use crate::agents::{Pieces, ScanCx, assemble};
use crate::model::*;

/// Spec 1.2: the sentence that opens the block.
pub(super) const INTRO: &str = "Codebase and user instructions are shown below. Be sure to adhere to these instructions. IMPORTANT: These instructions OVERRIDE any default behavior and you MUST follow them exactly as written.";

/// The pseudo-path of a managed `claudeMd` setting (spec 1.1).
const MANAGED_SETTINGS_PATH: &str = "<managed-settings>";

pub(super) struct Built {
    pub rendered: RenderedBlock,
    pub block: ContextBlock,
}

pub(super) fn built(
    delivery: Delivery,
    label: BlockLabel,
    fidelity: Fidelity,
    pieces: Pieces,
) -> Built {
    let (block, rendered) = assemble(delivery, label, fidelity, pieces);
    Built { rendered, block }
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

/// The text Claude Code sends for one delivered load, without the header or list mark around
/// it. The scan counts the same text: `instructions::delivered_text` and the on-demand cut for
/// instruction files, `cut_index` for a memory index, `body_of` for a body on invoke. A listing
/// line is not file text, so it has no case here: the catalog builds it.
pub(super) fn text_of(cx: &ScanCx, load: &Load) -> Option<String> {
    let bytes = cx.fs.read(&load.file).ok()?;
    let raw = String::from_utf8_lossy(&bytes);
    if matches!(&load.via, Via::Discovery { rule } if rule.0 == "claude.discovery.managed_setting")
    {
        let value: Value = serde_json::from_str(&raw).ok()?;
        return Some(value.get("claudeMd")?.as_str()?.trim().to_owned());
    }
    Some(match load.kind {
        DocKind::MemoryIndex => cut_index(&raw).content,
        DocKind::Instructions | DocKind::Rule => {
            let (text, _) = delivered_text(&raw);
            match &load.status {
                // Spec 1.3: a file that loads on demand is cut at the limit, in characters.
                LoadStatus::Truncated { limit, .. } if limit.kind == LimitKind::OnDemandChars => {
                    crate::limits::cut_chars(&text, limit.value).text
                }
                _ => text,
            }
        }
        DocKind::SkillBody | DocKind::CommandBody | DocKind::SubagentPrompt => {
            body_of(&raw).to_owned()
        }
        // Memory topic files and references: the model reads the file as it is.
        _ => raw.into_owned(),
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
/// the first block and closes in the last. Each file gets a `Contents of` header, which is frame.
pub(super) fn build(cx: &ScanCx, loads: &[Load]) -> Vec<Built> {
    let mut files: Vec<(&Load, String)> = Vec::new();
    let mut memory: Option<(&Load, String)> = None;
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
        if load.kind == DocKind::MemoryIndex {
            memory = Some((load, text));
        } else {
            files.push((load, text));
        }
    }
    if files.is_empty() && memory.is_none() {
        return Vec::new();
    }
    let header = |load: &Load| {
        format!(
            "Contents of {} ({}):\n\n",
            shown_path(cx, load),
            label_of(load)
        )
    };
    let mut first = Pieces::default();
    first.frame(&format!("<system-reminder>\n{INTRO}"));
    for (load, text) in files {
        first.frame("\n\n");
        first.frame(&header(load));
        first.load(load.id.clone(), text);
    }
    if memory.is_none() {
        first.frame("\n</system-reminder>");
    }
    let mut out = vec![built(
        Delivery::UserMessage,
        BlockLabel::Instructions,
        Fidelity::Exact,
        first,
    )];
    if let Some((load, text)) = memory {
        let mut second = Pieces::default();
        second.frame(&header(load));
        second.load(load.id.clone(), text);
        second.frame("\n</system-reminder>");
        out.push(built(
            Delivery::UserMessage,
            BlockLabel::Memory,
            Fidelity::Exact,
            second,
        ));
    }
    out
}

pub(super) fn budget(blocks: &[Built], loads: &[Load]) -> Budget {
    let mut by_label: Vec<(BlockLabel, u32)> = Vec::new();
    for b in blocks {
        match by_label.iter_mut().find(|(l, _)| *l == b.block.label) {
            Some((_, t)) => *t += b.block.tokens,
            None => by_label.push((b.block.label, b.block.tokens)),
        }
    }
    let on_access_max = loads
        .iter()
        .filter(|l| delivered(l) && root_is_on_access(loads, l))
        .filter_map(|l| l.delivered.as_ref())
        .map(|d| d.tokens)
        .sum();
    Budget {
        session_start: blocks.iter().map(|b| b.block.tokens).sum(),
        by_label,
        on_access_max,
        method: TokenMethod::O200kEstimate,
    }
}

fn root_is_on_access(loads: &[Load], load: &Load) -> bool {
    match (&load.when, &load.via) {
        (Activation::OnAccess { .. }, _) => true,
        (Activation::WithHost, Via::Import { host, .. }) => loads
            .get(host.0 as usize)
            .is_some_and(|host| root_is_on_access(loads, host)),
        _ => false,
    }
}
