//! Gemini CLI. Specified in `docs/agents/gemini-cli.md` (version 0.69.0). No run of the real CLI
//! backs this adapter, so everything it renders is `Fidelity::Approximate`.

use std::sync::OnceLock;

use super::{
    Agent, AgentScan, Candidate, FileClaim, ScanCx, empty_view, home_dir, home_with_env, presence,
    rules_from,
};
use crate::model::*;
use crate::tokens;

mod compose;
mod context;
mod extension;
mod exts;
mod guard;
mod hooks;
mod memory;
mod prompt;
mod settings;
mod slots;
mod trust;
mod util;
mod world;

#[cfg(test)]
mod tests;

use context::Builder;
use util::rule;
use world::World;

pub struct Gemini;

static RULES: OnceLock<Vec<RuleSpec>> = OnceLock::new();

impl Agent for Gemini {
    fn id(&self) -> AgentId {
        AgentId::builtin("gemini")
    }

    fn display_name(&self) -> &'static str {
        "Gemini CLI"
    }

    fn rules(&self) -> &'static [RuleSpec] {
        rules_from(&RULES, include_str!("rules.toml"))
    }

    fn hook_schema(&self) -> &'static HookSchema {
        hooks::schema()
    }

    fn scan(&self, cx: &ScanCx) -> AgentScan {
        let choice = home_with_env(cx, "GEMINI_CLI_HOME", ".gemini", true);
        let home = home_dir(cx, "gemini.home", &choice);
        let presence = presence(cx, "gemini", home.exists);
        let w = World::new(cx);
        let mut view = empty_view(self.id(), presence, vec![home]);
        view.trust = Trust {
            level: if w.trusted() {
                TrustLevel::Trusted
            } else {
                TrustLevel::Untrusted
            },
            recorded_in: (w.trust.source == trust::TrustSource::Rule).then(|| w.trust.file.clone()),
        };
        view.assumptions = assumptions(&w);
        view.slots = slots::slots(&w);
        let mut diagnostics = w.diagnostics.clone();
        let mut files = state_claims(&w);

        // A settings file that does not parse stops the CLI before it loads anything (spec 2).
        if !w.fatal.is_empty() {
            return finish(view, files, diagnostics, self.id());
        }

        let built = build(&w);
        let hooks = hooks::scan(&w);
        files.extend(built.stores.claims.iter().cloned());
        files.extend(built.exts.claims.iter().cloned());
        diagnostics.extend(built.diagnostics);
        diagnostics.extend(built.exts.diagnostics.iter().cloned());
        diagnostics.extend(hooks.diagnostics);
        if let Some(skipped) = untrusted_skips(&w) {
            diagnostics.push(Diagnostic {
                severity: Severity::Warning,
                agent: None,
                at: Subject::Agent,
                problem: Problem::ProjectUntrusted { skipped },
                fix: None,
            });
        }

        let loads = built.loads;
        view.session = built.composed.iter().map(|c| c.block.clone()).collect();
        view.budget = budget(&view.session, &loads);
        view.loads = loads;
        view.hooks = hooks.hooks;
        view.hook_files = hooks.files;
        view.memory = built.stores.stores;
        view.extensions = built.exts.items;
        finish(view, files, diagnostics, self.id())
    }

    /// Spec 1, subdirectory row.
    fn on_access(&self, cx: &ScanCx, dir: &AbsPath) -> Vec<Candidate> {
        let w = World::new(cx);
        if !w.fatal.is_empty() {
            return Vec::new();
        }
        context::jit_files(&w, dir)
            .into_iter()
            .map(|(file, at)| Candidate {
                file,
                kind: DocKind::Instructions,
                scope: Scope::Project { dir: at.clone() },
                rule: rule("gemini.context.jit"),
                access: AccessRule::UnderDir { dir: at },
            })
            .collect()
    }

    fn render(&self, cx: &ScanCx, _view: &AgentView) -> Vec<RenderedBlock> {
        let w = World::new(cx);
        if !w.fatal.is_empty() {
            return Vec::new();
        }
        build(&w).composed.into_iter().map(|c| c.rendered).collect()
    }

    /// The keys of `projects.json`, in both runtime directories (spec 4, spec 7).
    fn known_projects(&self, cx: &ScanCx) -> Vec<AbsPath> {
        let w = World::new(cx);
        let mut projects: Vec<AbsPath> = Vec::new();
        for runtime in [w.gdir.clone(), memory::sandbox_runtime(&w)] {
            let Some(text) = util::read_lossy(cx, &runtime.join("projects.json")) else {
                continue;
            };
            let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
                continue;
            };
            let Some(map) = value.get("projects").and_then(|p| p.as_object()) else {
                continue;
            };
            for key in map.keys() {
                if let Ok(path) = AbsPath::parse(key)
                    && !projects.contains(&path)
                {
                    projects.push(path);
                }
            }
        }
        projects
    }

    fn guard(
        &self,
        path: &AbsPath,
        before: Option<&str>,
        after: Option<&str>,
    ) -> Result<(), ApiError> {
        guard::guard(path, before, after)
    }

    fn exec_spec(
        &self,
        hook: &Hook,
        input: &serde_json::Value,
        cx: &ScanCx,
    ) -> Result<ExecSpec, RuleRef> {
        hooks::exec_spec(hook, input, cx)
    }

    fn sample_input(&self, event: &str, cx: &ScanCx) -> serde_json::Value {
        hooks::sample_input(event, cx)
    }
}

struct Built {
    loads: Vec<Load>,
    diagnostics: Vec<Diagnostic>,
    exts: exts::Exts,
    stores: memory::Stores,
    composed: Vec<compose::Composed>,
}

/// Every load of one session, and the blocks the session sends.
fn build(w: &World) -> Built {
    let mut b = Builder::new(w);
    let system = system_md(&mut b);
    let gathered = context::gather(&mut b);
    let exts = exts::scan(&mut b);
    let stores = memory::stores(w);
    memory::note_loads(&mut b, &stores);
    let composed = compose::compose(w, &gathered, &exts, system.as_ref());
    Built {
        loads: std::mem::take(&mut b.loads),
        diagnostics: std::mem::take(&mut b.diagnostics),
        exts,
        stores,
        composed,
    }
}

/// Spec 6, `GEMINI_SYSTEM_MD`: the file replaces the base prompt and Tier 1 still follows it.
fn system_md(b: &mut Builder) -> Option<compose::SystemMdText> {
    let w = b.w;
    let selected = prompt::system_md(w)?;
    let id = b.next_id();
    let scope = if selected.from_switch {
        Scope::Project { dir: w.cwd.clone() }
    } else {
        Scope::User
    };
    let text = util::read_lossy(w.cx, &selected.path);
    let (status, delivered) = match &text {
        Some(text) => (
            LoadStatus::Loaded,
            Some(Delivered {
                bytes: text.len() as u64,
                tokens: tokens::count(text),
                transformed: false,
            }),
        ),
        // The CLI throws `missing system prompt file` and does not start.
        None => (
            LoadStatus::Skipped {
                skip: Skip::Missing,
            },
            None,
        ),
    };
    let load = b.push(Load {
        id,
        file: selected.path,
        kind: DocKind::Instructions,
        scope,
        via: Via::Discovery {
            rule: rule("gemini.system_md"),
        },
        when: Activation::SessionStart,
        status,
        delivered,
    });
    text.map(|text| compose::SystemMdText { load, text })
}

fn budget(session: &[ContextBlock], loads: &[Load]) -> Budget {
    let mut by_label: Vec<(BlockLabel, u32)> = Vec::new();
    for block in session {
        match by_label.iter_mut().find(|(label, _)| *label == block.label) {
            Some((_, total)) => *total += block.tokens,
            None => by_label.push((block.label, block.tokens)),
        }
    }
    let on_access_max = loads
        .iter()
        .filter(|l| matches!(l.when, Activation::OnAccess { .. }))
        .filter_map(|l| l.delivered.as_ref().map(|d| d.tokens))
        .sum();
    Budget {
        session_start: session.iter().map(|b| b.tokens).sum(),
        by_label,
        on_access_max,
        method: TokenMethod::O200kEstimate,
    }
}

fn finish(
    view: AgentView,
    files: Vec<FileClaim>,
    mut diagnostics: Vec<Diagnostic>,
    id: AgentId,
) -> AgentScan {
    for diagnostic in &mut diagnostics {
        diagnostic.agent = Some(id.clone());
    }
    AgentScan {
        view,
        files,
        diagnostics,
    }
}

fn assumptions(w: &World) -> Vec<Assumption> {
    let s = &w.settings;
    let set_in = |key: &str| s.set_in(key).map(|layer| layer.path.clone());
    let feature = |name: &str, key: &str, default: bool| Assumption::Feature {
        name: name.to_owned(),
        on: s.bool(key, default),
        set_in: set_in(key),
    };
    let mut out = vec![
        feature("folderTrust", "security.folderTrust.enabled", true),
        feature("hooks", "hooksConfig.enabled", true),
        feature("skills", "skills.enabled", true),
        feature("agents", "experimental.enableAgents", true),
        feature("autoMemory", "experimental.autoMemory", false),
        Assumption::Mode {
            name: "context.fileName".to_owned(),
            value: w.names.join(","),
            set_in: set_in("context.fileName"),
        },
        Assumption::Mode {
            name: "context.memoryBoundaryMarkers".to_owned(),
            value: w.markers.join(","),
            set_in: set_in("context.memoryBoundaryMarkers"),
        },
    ];
    if w.roots.len() > 1 {
        out.push(Assumption::Mode {
            name: "context.includeDirectories".to_owned(),
            value: w.roots[1..]
                .iter()
                .map(AbsPath::as_str)
                .collect::<Vec<_>>()
                .join(","),
            set_in: set_in("context.includeDirectories"),
        });
    }
    if s.get("context.importFormat").and_then(|v| v.as_str()) == Some("flat") {
        out.push(Assumption::Unknown {
            what: rule("gemini.import_format_flat"),
        });
    }
    let mcp = s
        .get("mcpServers")
        .and_then(|v| v.as_object())
        .is_some_and(|m| !m.is_empty())
        || w.exts.iter().any(|e| e.active() && e.has_mcp);
    if mcp {
        out.push(Assumption::Unknown {
            what: rule("gemini.mcp_instructions"),
        });
    }
    if s.layers.iter().any(|l| {
        matches!(
            l.kind,
            settings::LayerKind::System | settings::LayerKind::SystemDefaults
        ) && l.exists
    }) {
        out.push(Assumption::Unknown {
            what: rule("gemini.system_file_ownership"),
        });
    }
    out
}

/// Files the loads do not name but the CLI uses (spec 8).
fn state_claims(w: &World) -> Vec<FileClaim> {
    let state = |path: AbsPath| FileClaim {
        path,
        policy: EditPolicy::ReadOnly {
            why: ReadOnlyWhy::AgentState,
        },
    };
    let mut claims = Vec::new();
    for layer in &w.settings.layers {
        let editable = matches!(
            layer.kind,
            settings::LayerKind::User | settings::LayerKind::Workspace
        );
        if editable || layer.exists {
            claims.push(FileClaim {
                path: layer.path.clone(),
                policy: if editable {
                    EditPolicy::Editable
                } else {
                    EditPolicy::ReadOnly {
                        why: ReadOnlyWhy::Managed,
                    }
                },
            });
        }
    }
    for path in [w.gdir.join("trusted_hooks.json"), w.trust.file.clone()] {
        if util::is_file(w.cx, &path) {
            claims.push(state(path));
        }
    }
    let enablement = w.gdir.join("extensions/extension-enablement.json");
    if util::is_file(w.cx, &enablement) {
        claims.push(FileClaim {
            path: enablement,
            policy: EditPolicy::Editable,
        });
    }
    for ext in w.exts.iter().filter(|e| !e.linked) {
        let plugin = || EditPolicy::ReadOnly {
            why: ReadOnlyWhy::PluginCache {
                plugin: ext.name.clone(),
            },
        };
        claims.push(FileClaim {
            path: ext.dir.join("gemini-extension.json"),
            policy: plugin(),
        });
        claims.extend(ext.context_files.iter().map(|path| FileClaim {
            path: path.clone(),
            policy: plugin(),
        }));
    }
    if let Some(target) = prompt::write_target(w) {
        // `GEMINI_WRITE_SYSTEM_MD` makes the CLI overwrite the file on every start (spec 6).
        claims.push(FileClaim {
            path: target,
            policy: EditPolicy::ReadOnly {
                why: ReadOnlyWhy::Generated,
            },
        });
    }
    claims
}

/// What an untrusted folder kept out, for the files that exist (spec 1, 5, 6).
fn untrusted_skips(w: &World) -> Option<Vec<RuleRef>> {
    if w.trusted() {
        return None;
    }
    let mut skipped = Vec::new();
    if !context::project_files(w).files.is_empty() {
        skipped.push(rule("gemini.context.project"));
    }
    let present = |rel: &str| util::is_dir(w.cx, &w.cwd.join(rel));
    if w.settings
        .layer(settings::LayerKind::Workspace)
        .is_some_and(|l| l.exists)
    {
        skipped.push(rule("gemini.settings.workspace"));
    }
    if present(".gemini/commands") {
        skipped.push(rule("gemini.command"));
    }
    if present(".gemini/skills") || present(".agents/skills") {
        skipped.push(rule("gemini.skill"));
    }
    if present(".gemini/agents") {
        skipped.push(rule("gemini.agent"));
    }
    (!skipped.is_empty()).then_some(skipped)
}
