//! The literal session-start text (spec 1, "Concatenation order and separators") and the
//! `ContextBlock`s that describe it. Every block is `Fidelity::Approximate`: no run of the real
//! CLI backs it.

use super::context::{Gathered, Item};
use super::exts::Exts;
use super::util::{collapse_newlines, js_trim};
use super::world::World;
use crate::model::*;
use crate::tokens;

pub(super) struct SystemMdText {
    pub load: LoadId,
    pub text: String,
}

pub(super) struct Composed {
    pub block: ContextBlock,
    pub rendered: RenderedBlock,
}

fn blocks(items: &[Item]) -> String {
    items
        .iter()
        .filter_map(|item| item.block.as_deref())
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// `renderUserMemory`, with the newline cleanup of `promptProvider.ts` applied. The text is what
/// follows the base prompt and one blank line.
fn tier1(names: &[String], global: &str, private: &str) -> String {
    let global = js_trim(global);
    let private = js_trim(private);
    let text = if private.is_empty() {
        if global.is_empty() {
            return String::new();
        }
        format!(
            "\n# Contextual Instructions ({})\n\
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
<loaded_context>\n{global}\n</loaded_context>",
            names.join(", ")
        )
    } else {
        let mut sections = Vec::new();
        if !global.is_empty() {
            sections.push(format!("<global_context>\n{global}\n</global_context>"));
        }
        sections.push(format!(
            "<user_project_memory>\n--- Private Project Memory Index (private, not committed to repo) ---\n{private}\n--- End Private Project Memory Index ---\n</user_project_memory>"
        ));
        format!(
            "\n---\n\n<loaded_context>\n{}\n</loaded_context>",
            sections.join("\n")
        )
    };
    collapse_newlines(&text).trim_start_matches('\n').to_owned()
}

/// `getSessionMemory`.
fn tier2(extension: &str, project: &str) -> String {
    let mut sections = Vec::new();
    if !js_trim(extension).is_empty() {
        sections.push(format!(
            "<extension_context>\n{}\n</extension_context>",
            js_trim(extension)
        ));
    }
    if !js_trim(project).is_empty() {
        sections.push(format!(
            "<project_context>\n{}\n</project_context>",
            js_trim(project)
        ));
    }
    if sections.is_empty() {
        return String::new();
    }
    format!(
        "\n<loaded_context>\n{}\n</loaded_context>",
        sections.join("\n")
    )
}

fn skill_catalog(skills: &[(LoadId, String, String, AbsPath)]) -> String {
    let xml = skills
        .iter()
        .map(|(_, name, description, file)| {
            format!(
                "  <skill>\n    <name>{name}</name>\n    <description>{description}</description>\n    <location>{file}</location>\n  </skill>"
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "# Available Agent Skills\n\nYou have access to the following specialized skills. To activate a skill and receive its detailed instructions, call the `activate_skill` tool with the skill's name.\n\n<available_skills>\n{xml}\n</available_skills>"
    )
}

fn make(delivery: Delivery, label: BlockLabel, text: String, loads: &[(LoadId, u32)]) -> Composed {
    let total = tokens::count(&text);
    let counted: u32 = loads.iter().map(|(_, t)| *t).sum();
    let mut parts: Vec<Part> = loads
        .iter()
        .map(|(load, tokens)| Part::Load {
            load: load.clone(),
            tokens: *tokens,
        })
        .collect();
    parts.push(Part::Glue {
        tokens: total.saturating_sub(counted),
    });
    Composed {
        block: ContextBlock {
            delivery,
            label,
            parts,
            tokens: total,
            fidelity: Fidelity::Approximate,
        },
        rendered: RenderedBlock {
            delivery,
            label,
            text,
            tokens: total,
            fidelity: Fidelity::Approximate,
        },
    }
}

fn loads_of(items: &[Item]) -> Vec<(LoadId, u32)> {
    items
        .iter()
        .filter(|item| item.block.is_some())
        .map(|item| (item.load.clone(), item.tokens))
        .collect()
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
        let tokens = tokens::count(&sys.text);
        out.push(make(
            Delivery::SystemPrompt,
            BlockLabel::Instructions,
            sys.text.clone(),
            &[(sys.load.clone(), tokens)],
        ));
    }
    if !exts.catalog.is_empty() {
        let loads: Vec<_> = exts
            .catalog
            .iter()
            .map(|(load, name, description, file)| {
                let one = skill_catalog(&[(
                    load.clone(),
                    name.clone(),
                    description.clone(),
                    file.clone(),
                )]);
                (load.clone(), tokens::count(&one))
            })
            .collect();
        out.push(make(
            Delivery::SystemPrompt,
            BlockLabel::SkillCatalog,
            skill_catalog(&exts.catalog),
            &loads,
        ));
    }
    let system = tier1(&w.names, &blocks(&g.global), &blocks(&g.private));
    if !system.is_empty() {
        let mut loads = loads_of(&g.global);
        loads.extend(loads_of(&g.private));
        out.push(make(
            Delivery::SystemPrompt,
            BlockLabel::Instructions,
            system,
            &loads,
        ));
    }
    let project = if w.trusted() {
        blocks(&g.project)
    } else {
        String::new()
    };
    let session = tier2(&blocks(&g.extension), &project);
    if !session.is_empty() {
        let mut loads = loads_of(&g.extension);
        if w.trusted() {
            loads.extend(loads_of(&g.project));
        }
        out.push(make(
            Delivery::UserMessage,
            BlockLabel::Instructions,
            session,
            &loads,
        ));
    }
    out
}
