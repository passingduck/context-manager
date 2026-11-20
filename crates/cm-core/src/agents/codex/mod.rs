//! Codex CLI. Specified in `docs/agents/codex.md`, checked against `codex-cli 0.165.0`.
//!
//! One pass builds everything: [`Setup`] reads the homes, the config layers, the project root
//! and the trust level, and then each module adds its loads, extensions and hooks to one
//! [`Out`]. `scan` and `render` both run that pass, so the literal text and the loads agree.

mod catalog;
mod extras;
mod hooks;
mod instructions;
mod memory;
mod out;
mod plan;
mod plugins;
mod setup;
mod skills;
mod subagents;
#[cfg(test)]
mod tests;

use std::sync::OnceLock;

use super::{Agent, AgentScan, PlanCx, ScanCx, home_dir, presence, rules_from, unbudgeted};
use crate::model::*;
use crate::tokens;
use out::{Block, BlockPart, Out, me};
use setup::Setup;

pub use hooks::{HookInspection, TrustStatus, current_hash};

pub struct Codex;

static RULES: OnceLock<Vec<RuleSpec>> = OnceLock::new();

/// Every hook Codex lists for the scan's cwd, with its key, enabled flag, trust status and
/// current hash, as `hooks/list` reports them.
pub fn inspect_hooks(cx: &ScanCx) -> Vec<HookInspection> {
    hooks::inspect(cx)
}

struct Computed {
    scan: AgentScan,
    blocks: Vec<Block>,
}

fn rank(delivery: Delivery) -> u8 {
    match delivery {
        Delivery::SystemPrompt => 0,
        Delivery::SystemMessage | Delivery::DeveloperMessage => 1,
        Delivery::UserMessage => 2,
        Delivery::ToolSpec => 3,
    }
}

fn compute(cx: &ScanCx) -> Computed {
    let setup = Setup::load(cx);
    let plugins = plugins::installed(cx, &setup);
    let mut out = Out::default();
    out.diagnostics.extend(setup.diagnostics.iter().cloned());

    instructions::run(cx, &setup, &mut out);
    skills::run(cx, &setup, &plugins, &mut out);
    subagents::run(cx, &setup, &mut out);
    memory::run(cx, &setup, &mut out);
    hooks::run(cx, &setup, &plugins, &mut out);
    extras::slots(&setup, &mut out);
    assumptions(cx, &setup, &plugins, &mut out);

    let home = HomeDir {
        role: RuleRef::new("codex.home"),
        path: setup.codex_home.clone(),
        from_env: setup.home_from_env.map(str::to_owned),
        exists: cx
            .fs
            .stat(&setup.codex_home)
            .map(|s| s.kind == crate::probe_fs::EntryKind::Dir)
            .unwrap_or(false),
    };
    let _ = home_dir;
    let presence = presence(cx, "codex", home.exists);

    out.blocks.sort_by_key(|b| rank(b.delivery));
    let session: Vec<ContextBlock> = out.blocks.iter().map(context_block).collect();
    // Subagents after skills, as the extension list reads.
    let view = AgentView {
        agent: me(),
        presence,
        homes: vec![home],
        trust: setup.trust.clone(),
        assumptions: std::mem::take(&mut out.assumptions),
        loads: std::mem::take(&mut out.loads),
        session,
        budget: unbudgeted(),
        hooks: std::mem::take(&mut out.hooks),
        hook_files: std::mem::take(&mut out.hook_files),
        memory: std::mem::take(&mut out.memory),
        extensions: std::mem::take(&mut out.extensions),
        slots: std::mem::take(&mut out.slots),
    };
    Computed {
        scan: AgentScan {
            view,
            files: std::mem::take(&mut out.files),
            diagnostics: std::mem::take(&mut out.diagnostics),
        },
        blocks: std::mem::take(&mut out.blocks),
    }
}

fn context_block(block: &Block) -> ContextBlock {
    let parts: Vec<Part> = block
        .parts
        .iter()
        .map(|p| match p {
            BlockPart::Text(t) => Part::Glue {
                tokens: tokens::count(t),
            },
            BlockPart::Load(id, t) => Part::Load {
                load: id.clone(),
                tokens: tokens::count(t),
            },
        })
        .collect();
    let total = parts
        .iter()
        .map(|p| match p {
            Part::Glue { tokens } | Part::Load { tokens, .. } => *tokens,
        })
        .sum();
    ContextBlock {
        delivery: block.delivery,
        label: block.label,
        parts,
        tokens: total,
        // Spec 3.4: the memory template is not reproduced.
        fidelity: if block.label == BlockLabel::Memory {
            Fidelity::Approximate
        } else {
            Fidelity::Exact
        },
        preceded_by: Vec::new(),
    }
}

/// What a static scan had to assume (design section 3, `Assumption`).
fn assumptions(cx: &ScanCx, setup: &Setup, plugins: &[plugins::Plugin], out: &mut Out) {
    for (name, on) in [
        ("hooks", setup.features.hooks),
        ("memories", setup.features.memories),
        ("plugins", setup.features.plugins),
    ] {
        let set_in = setup
            .layer_setting(&["features", name])
            .map(|(layer, _)| layer.config_path.clone());
        out.assumptions.push(Assumption::Feature {
            name: name.to_owned(),
            on,
            set_in,
        });
    }
    out.assumptions.push(Assumption::Mode {
        name: "project_doc_max_bytes".to_owned(),
        value: setup.project_doc_max_bytes.to_string(),
        set_in: setup
            .layer_setting(&["project_doc_max_bytes"])
            .map(|(l, _)| l.config_path.clone()),
    });
    if !setup.fallback_names.is_empty() {
        out.assumptions.push(Assumption::Mode {
            name: "project_doc_fallback_filenames".to_owned(),
            value: setup.fallback_names.join(","),
            set_in: setup
                .layer_setting(&["project_doc_fallback_filenames"])
                .map(|(l, _)| l.config_path.clone()),
        });
    }
    out.assumptions.push(Assumption::Mode {
        name: "project_root_markers".to_owned(),
        value: setup.markers.join(","),
        set_in: None,
    });
    let pinned = setup.merged.contains_key("model_context_window");
    if let Some(window) = skills::context_window(cx, setup).filter(|w| *w > 0) {
        out.assumptions.push(Assumption::ContextWindow {
            tokens: u32::try_from(window).unwrap_or(u32::MAX),
        });
    }
    if !pinned {
        // The skill catalog budget follows the model's window, which a scan reads only from the
        // model cache, when it has one.
        out.assumptions.push(Assumption::Unknown {
            what: RuleRef::new("codex.assumption.context_window"),
        });
    }
    out.assumptions.push(Assumption::Unknown {
        what: RuleRef::new("codex.assumption.managed_sources"),
    });
    if !plugins.is_empty() {
        out.assumptions.push(Assumption::Unknown {
            what: RuleRef::new("codex.assumption.plugin_marketplaces"),
        });
    }
}

impl Agent for Codex {
    fn id(&self) -> AgentId {
        me()
    }

    fn display_name(&self) -> &'static str {
        "Codex CLI"
    }

    fn rules(&self) -> &'static [RuleSpec] {
        rules_from(&RULES, include_str!("rules.toml"))
    }

    fn hook_schema(&self) -> &'static HookSchema {
        hooks::schema()
    }

    fn scan(&self, cx: &ScanCx) -> AgentScan {
        compute(cx).scan
    }

    /// Spec 1.4: the literal blocks, in delivery order.
    fn render(&self, cx: &ScanCx, _view: &AgentView) -> Vec<RenderedBlock> {
        compute(cx)
            .blocks
            .iter()
            .map(|block| {
                let text = block.text();
                RenderedBlock {
                    delivery: block.delivery,
                    label: block.label,
                    tokens: tokens::count(&text),
                    fidelity: context_block(block).fidelity,
                    text,
                }
            })
            .collect()
    }

    fn known_projects(&self, cx: &ScanCx) -> Vec<AbsPath> {
        extras::known_projects(cx, &Setup::load(cx))
    }

    fn plan(&self, cx: &PlanCx, edit: &Edit) -> Option<Result<Preview, ApiError>> {
        plan::plan(cx, edit)
    }

    fn write_warnings(
        &self,
        cx: &PlanCx,
        path: &AbsPath,
        _before: Option<&str>,
        _after: Option<&str>,
    ) -> Vec<Warning> {
        plan::write_warnings(cx, path)
    }

    fn guard(
        &self,
        path: &AbsPath,
        before: Option<&str>,
        after: Option<&str>,
    ) -> Result<(), ApiError> {
        extras::guard(path, before, after)
    }

    fn exec_spec(
        &self,
        hook: &Hook,
        input: &serde_json::Value,
        cx: &ScanCx,
    ) -> Result<ExecSpec, RuleRef> {
        extras::exec_spec(cx, &Setup::load(cx), hook, input)
    }

    fn sample_input(&self, event: &str, cx: &ScanCx) -> serde_json::Value {
        extras::sample_input(event, &cx.env.cwd)
    }
}
