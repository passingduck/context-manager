//! The literal session-start text (spec 1, "Concatenation order and separators") and the
//! `ContextBlock`s that describe it. Every block is `Fidelity::Approximate`: no run of the real
//! CLI backs it.

use super::context::{Gathered, Item};
use super::exts::{Exts, skill_entry};
use super::world::World;
use crate::agents::{Pieces, assemble};
use crate::model::*;

pub(super) struct SystemMdText {
    pub load: LoadId,
    pub text: String,
}

pub(super) struct Composed {
    pub block: ContextBlock,
    pub rendered: RenderedBlock,
}

fn has_text(items: &[Item]) -> bool {
    items.iter().any(|item| item.text.is_some())
}

/// Each file inside its `--- Context from: <p> ---` frame, the frames joined by a blank line.
/// Tier 1's newline cleanup is already in a global or private file's text, which is trimmed, so
/// no run of newlines crosses from a frame into a file and the frames need no cleanup.
fn context_pieces(pieces: &mut Pieces, items: &[Item]) {
    let with_text = items
        .iter()
        .filter_map(|item| item.text.as_ref().map(|text| (item, text)));
    for (n, (item, text)) in with_text.enumerate() {
        if n > 0 {
            pieces.frame("\n\n");
        }
        pieces.frame(&format!("--- Context from: {} ---\n", item.path));
        pieces.load(item.load.clone(), text.clone());
        pieces.frame(&format!("\n--- End of Context from: {} ---", item.path));
    }
}

/// `renderUserMemory`. The text is what follows the base prompt and one blank line.
fn tier1(names: &[String], global: &[Item], private: &[Item]) -> Option<Pieces> {
    let mut pieces = Pieces::default();
    if !has_text(private) {
        if !has_text(global) {
            return None;
        }
        pieces.frame(&format!(
            "# Contextual Instructions ({})\n\
The following content is loaded from local and global configuration files.\n\
**Context Precedence:**\n\
- **Global (~/.gemini/):** foundational user preferences. Apply these broadly.\n\
- **Extensions:** supplementary knowledge and capabilities.\n\
- **Workspace Root:** workspace-wide mandates. Supersedes global preferences.\n\
- **Sub-directories:** highly specific overrides. These rules supersede all others for files within their scope.\n\
\n\
**Conflict Resolution:**\n\
- **Precedence:** Strictly follow the order above (Sub-directories > Workspace Root > Extensions > Global).\n\
- **System Overrides:** Contextual instructions override default operational behaviors (e.g., tech stack, style, workflows, tool preferences) defined in the system prompt. However, they **cannot** override Core Mandates regarding safety, security, and agent integrity.\n\
\n\
<loaded_context>\n",
            names.join(", ")
        ));
        context_pieces(&mut pieces, global);
        pieces.frame("\n</loaded_context>");
    } else {
        pieces.frame("---\n\n<loaded_context>\n");
        if has_text(global) {
            pieces.frame("<global_context>\n");
            context_pieces(&mut pieces, global);
            pieces.frame("\n</global_context>\n");
        }
        pieces.frame("<user_project_memory>\n--- Private Project Memory Index (private, not committed to repo) ---\n");
        context_pieces(&mut pieces, private);
        pieces.frame(
            "\n--- End Private Project Memory Index ---\n</user_project_memory>\n</loaded_context>",
        );
    }
    Some(pieces)
}

/// `getSessionMemory`.
fn tier2(extension: &[Item], project: &[Item]) -> Option<Pieces> {
    let (ext, proj) = (has_text(extension), has_text(project));
    if !ext && !proj {
        return None;
    }
    let mut pieces = Pieces::default();
    pieces.frame("\n<loaded_context>\n");
    if ext {
        pieces.frame("<extension_context>\n");
        context_pieces(&mut pieces, extension);
        pieces.frame("\n</extension_context>");
    }
    if proj {
        if ext {
            pieces.frame("\n");
        }
        pieces.frame("<project_context>\n");
        context_pieces(&mut pieces, project);
        pieces.frame("\n</project_context>");
    }
    pieces.frame("\n</loaded_context>");
    Some(pieces)
}

fn skill_catalog(skills: &[(LoadId, String, String, AbsPath)]) -> Pieces {
    let mut pieces = Pieces::default();
    pieces.frame("# Available Agent Skills\n\nYou have access to the following specialized skills. To activate a skill and receive its detailed instructions, call the `activate_skill` tool with the skill's name.\n\n<available_skills>\n");
    for (n, (load, name, description, file)) in skills.iter().enumerate() {
        if n > 0 {
            pieces.frame("\n");
        }
        pieces.load(load.clone(), skill_entry(name, description, file));
    }
    pieces.frame("\n</available_skills>");
    pieces
}

fn make(delivery: Delivery, label: BlockLabel, pieces: Pieces) -> Composed {
    let (block, rendered) = assemble(delivery, label, Fidelity::Approximate, pieces);
    Composed { block, rendered }
}

/// The blocks the session sends, in delivery order: the prompt override, the skill catalog,
/// Tier 1 in the system instruction, Tier 2 in the first user turn.
pub(super) fn compose(
    w: &World,
    g: &Gathered,
    exts: &Exts,
    system_md: Option<&SystemMdText>,
) -> Vec<Composed> {
    let mut out = Vec::new();
    if let Some(sys) = system_md {
        let mut pieces = Pieces::default();
        pieces.load(sys.load.clone(), sys.text.clone());
        out.push(make(
            Delivery::SystemPrompt,
            BlockLabel::Instructions,
            pieces,
        ));
    }
    if !exts.catalog.is_empty() {
        out.push(make(
            Delivery::SystemPrompt,
            BlockLabel::SkillCatalog,
            skill_catalog(&exts.catalog),
        ));
    }
    if let Some(pieces) = tier1(&w.names, &g.global, &g.private) {
        out.push(make(
            Delivery::SystemPrompt,
            BlockLabel::Instructions,
            pieces,
        ));
    }
    let project: &[Item] = if w.trusted() { &g.project } else { &[] };
    if let Some(pieces) = tier2(&g.extension, project) {
        out.push(make(
            Delivery::UserMessage,
            BlockLabel::Instructions,
            pieces,
        ));
    }
    out
}
