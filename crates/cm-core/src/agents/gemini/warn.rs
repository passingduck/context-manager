//! Warnings that follow a write to a Gemini CLI file whoever planned it, a plain save included
//! (design section 4, spec 3 and spec 8).

use super::memory;
use super::plan::id;
use super::world::World;
use crate::agents::{PlanCx, ScanCx};
use crate::md::spans;
use crate::model::*;

pub(super) fn write_warnings(
    cx: &PlanCx,
    path: &AbsPath,
    before: Option<&str>,
    after: Option<&str>,
) -> Vec<Warning> {
    let scan = ScanCx {
        env: cx.env,
        fs: cx.fs,
        parked_dir: None,
    };
    let w = World::new(&scan);
    let mut out = Vec::new();

    // Spec 8, settings row: the CLI rewrites these files from its startup snapshot.
    if w.settings.layers.iter().any(|l| l.path == *path) {
        out.push(Warning::SessionMayOverwrite { agent: id() });
        out.push(Warning::AgentAlsoWrites { agent: id() });
    }

    // Spec 6, subagents row: any edit of an acknowledged project agent changes its hash.
    if before.is_some() && after.is_some() && before != after && is_project_agent(cx, path) {
        out.push(Warning::AgentAckReset);
    }

    // Spec 3: a context file or a memory note that gains an `@word` gains an import attempt.
    if let Some(after) = after
        && is_context_file(&w, path)
    {
        let old = import_tokens(before.unwrap_or(""));
        for raw in import_tokens(after) {
            if !old.contains(&raw) {
                let warning = Warning::NewImportToken { raw };
                if !out.contains(&warning) {
                    out.push(warning);
                }
            }
        }
    }
    out
}

fn is_project_agent(cx: &PlanCx, path: &AbsPath) -> bool {
    cx.snapshot
        .agents
        .iter()
        .filter(|v| v.agent == id())
        .flat_map(|v| &v.slots)
        .filter(|s| s.id.0 == "gemini:subagent:project")
        .any(|s| match &s.place {
            Place::NamedFile { dir, ext } => {
                path.parent().as_ref() == Some(dir)
                    && path
                        .file_name()
                        .is_some_and(|n| n.ends_with(&format!(".{}", ext.trim_start_matches('.'))))
            }
            _ => false,
        })
}

/// A file that Gemini CLI reads through the import processor: a configured context file name,
/// or a note beside the private memory index.
fn is_context_file(w: &World, path: &AbsPath) -> bool {
    let name = path.file_name().unwrap_or_default();
    w.names.iter().any(|n| n == name)
        || memory::memory_dir(w)
            .is_some_and(|dir| path.parent().as_ref() == Some(&dir) && name.ends_with(".md"))
}

/// Spec 3, token syntax: `@` at offset 0 or after a space, tab, CR or LF, outside backtick
/// spans, followed by `.`, `/` or an ASCII letter. The path runs to the next whitespace.
fn import_tokens(text: &str) -> Vec<String> {
    let protected = spans::backtick_regions(text);
    let bytes = text.as_bytes();
    let mut out: Vec<String> = Vec::new();
    for (at, _) in text.match_indices('@') {
        if at > 0 && !matches!(bytes[at - 1], b' ' | b'\t' | b'\r' | b'\n') {
            continue;
        }
        if protected.iter().any(|r| r.contains(&at)) {
            continue;
        }
        let word: String = text[at + 1..]
            .chars()
            .take_while(|c| !c.is_whitespace())
            .collect();
        if word
            .chars()
            .next()
            .is_some_and(|c| matches!(c, '.' | '/') || c.is_ascii_alphabetic())
        {
            let raw = format!("@{word}");
            if !out.contains(&raw) {
                out.push(raw);
            }
        }
    }
    out
}
