//! Claude Code. Specified in `docs/agents/claude-code.md`.

use std::sync::OnceLock;

use serde_json::Value;

use super::{
    Agent, AgentScan, Candidate, FileClaim, PlanCx, ScanCx, home_dir, home_with_env, presence,
    rules_from, unbudgeted,
};
use crate::model::*;
use crate::walk;

mod catalog;
mod extensions;
mod hooks;
mod instructions;
mod memory;
mod plan;
mod render;
mod setup;
mod slots;
#[cfg(test)]
mod tests;

use instructions::ASSUMED_WINDOW_TOKENS;
use setup::{SettingsKind, Setup};

pub struct Claude;

static RULES: OnceLock<Vec<RuleSpec>> = OnceLock::new();

impl Agent for Claude {
    fn id(&self) -> AgentId {
        AgentId::builtin("claude")
    }

    fn display_name(&self) -> &'static str {
        "Claude Code"
    }

    fn rules(&self) -> &'static [RuleSpec] {
        rules_from(
            &RULES,
            concat!(
                include_str!("rules.toml"),
                "\n",
                include_str!("rules_ext.toml")
            ),
        )
    }

    fn hook_schema(&self) -> &'static HookSchema {
        hooks::schema()
    }

    /// Spec 6.1: the whole scanner.
    fn scan(&self, cx: &ScanCx) -> AgentScan {
        let setup = Setup::load(cx);
        let home_choice = home_with_env(cx, "CLAUDE_CONFIG_DIR", ".claude", false);
        let home = home_dir(cx, "claude.home", &home_choice);
        let presence = presence(cx, "claude", home.exists);

        let tree = walk::walk(&setup.cwd, cx.fs);
        let dirs: Vec<AbsPath> = tree.dirs().cloned().collect();
        let mut memory_out = None;
        let ins = instructions::collect(cx, &setup, &dirs, ASSUMED_WINDOW_TOKENS, &mut |c| {
            memory_out = Some(memory::collect_main(c, cx, &setup));
        });
        let mem = memory_out.expect("collect calls the memory step once");

        let mut loads = ins.loads;
        let ext =
            extensions::scan_with_window(cx, &setup, loads.len() as u32, ASSUMED_WINDOW_TOKENS);
        loads.extend(ext.loads.iter().cloned());
        let (agent_stores, agent_diags, agent_files) = {
            let base = loads.len() as u32;
            let mut extra: Vec<Load> = Vec::new();
            let mut push = |file: &AbsPath, kind, scope, via, when, status, delivered| {
                let id = LoadId(base + extra.len() as u32);
                extra.push(Load {
                    id: id.clone(),
                    file: file.clone(),
                    kind,
                    scope,
                    via,
                    when,
                    status,
                    delivered,
                });
                id
            };
            let out = memory::agent_stores(&mut push, cx, &setup, &ext, &mem.off);
            loads.extend(extra);
            out
        };
        let hook_out = hooks::scan(cx, &setup, &ext);

        let blocks = render::session(cx, &setup, &loads, &ext.extensions);

        let mut stores = mem.stores;
        stores.extend(agent_stores);
        let mut diagnostics = setup.diagnostics.clone();
        diagnostics.extend(ins.diagnostics);
        diagnostics.extend(mem.diagnostics);
        diagnostics.extend(agent_diags);
        diagnostics.extend(ext.diagnostics);
        diagnostics.extend(hook_out.diagnostics);

        let mut files = ins.files;
        files.extend(mem.files);
        files.extend(agent_files);
        files.extend(ext.files);
        files.extend(hook_out.files);
        files.extend(settings_claims(&setup));

        let view = AgentView {
            agent: self.id(),
            presence,
            homes: vec![home],
            trust: setup.trust(),
            assumptions: assumptions(
                cx,
                &setup,
                &ins.mode,
                ins.mode_set_in,
                ins.off,
                &mem.off,
                &mem.dir,
            ),
            loads,
            session: blocks.into_iter().map(|b| b.block).collect(),
            budget: unbudgeted(),
            hooks: hook_out.hooks,
            hook_files: hook_out.hook_files,
            memory: stores,
            extensions: ext.extensions,
            slots: slots::slots(cx, &setup, &mem.dir),
        };
        AgentScan {
            view,
            files,
            diagnostics,
        }
    }

    /// Spec 1.3: what loads when the agent touches a file in `dir`.
    fn on_access(&self, cx: &ScanCx, dir: &AbsPath) -> Vec<Candidate> {
        let setup = Setup::load(cx);
        if instructions::instructions_off(&setup, cx).is_some() {
            return Vec::new();
        }
        let mut out = Vec::new();
        if dir.starts_with(&setup.cwd) && dir != &setup.cwd {
            // Top-down from cwd to `dir`, each directory strictly below cwd.
            let rest = &dir.as_str()[setup.cwd.as_str().trim_end_matches('/').len()..];
            let mut at = setup.cwd.clone();
            for part in rest.split('/').filter(|p| !p.is_empty()) {
                at = at.join(part);
                instructions::own_candidates(cx, &at, &mut out);
            }
        }
        instructions::global_candidates(cx, &setup, &mut out);
        out
    }

    fn render(&self, cx: &ScanCx, view: &AgentView) -> Vec<RenderedBlock> {
        let setup = Setup::load(cx);
        render::session(cx, &setup, &view.loads, &view.extensions)
            .into_iter()
            .map(|b| b.rendered)
            .collect()
    }

    /// The same text `render` would send, for a load that fires later.
    fn load_text(&self, cx: &ScanCx, _view: &AgentView, load: &Load) -> Option<String> {
        render::text_of(cx, load)
    }

    /// Spec 5.2: the keys of `projects` in `~/.claude.json`.
    fn known_projects(&self, cx: &ScanCx) -> Vec<AbsPath> {
        let setup = Setup::load(cx);
        let Some(Value::Object(projects)) = setup.global.as_ref().and_then(|g| g.get("projects"))
        else {
            return Vec::new();
        };
        let mut out: Vec<AbsPath> = projects
            .keys()
            .filter_map(|key| AbsPath::parse(key).ok())
            .collect();
        out.sort();
        out
    }

    /// Spec 2.6, 3, 6.2: the edits that mean something to Claude Code.
    fn plan(&self, cx: &PlanCx, edit: &Edit) -> Option<Result<Preview, ApiError>> {
        plan::plan(cx, edit)
    }

    fn write_warnings(
        &self,
        _cx: &PlanCx,
        path: &AbsPath,
        _before: Option<&str>,
        _after: Option<&str>,
    ) -> Vec<Warning> {
        plan::write_warnings(path)
    }

    /// Spec 6.2: what a write may not touch, and what it must keep valid.
    fn guard(
        &self,
        path: &AbsPath,
        before: Option<&str>,
        after: Option<&str>,
    ) -> Result<(), ApiError> {
        guard(path, before, after)
    }

    fn sample_input(&self, event: &str, cx: &ScanCx) -> serde_json::Value {
        hooks::sample_input(event, cx)
    }

    fn exec_spec(
        &self,
        hook: &Hook,
        input: &serde_json::Value,
        cx: &ScanCx,
    ) -> Result<ExecSpec, RuleRef> {
        hooks::exec_spec(hook, input, cx)
    }
}

/// Spec 6.1, last paragraph, and spec 1.2 to 2.7: what the scan could not read.
fn assumptions(
    cx: &ScanCx,
    setup: &Setup,
    mode: &instructions::AgentsMode,
    mode_set_in: Option<AbsPath>,
    off: Option<&'static str>,
    memory_off: &Option<String>,
    memdir: &memory::MemDir,
) -> Vec<Assumption> {
    let mut out = vec![Assumption::ContextWindow {
        tokens: ASSUMED_WINDOW_TOKENS,
    }];
    out.push(Assumption::Mode {
        name: "instructionFiles".to_owned(),
        value: mode.name().to_owned(),
        set_in: mode_set_in,
    });
    out.push(Assumption::Feature {
        name: "instructions".to_owned(),
        on: off.is_none(),
        set_in: None,
    });
    out.push(Assumption::Feature {
        name: "autoMemory".to_owned(),
        on: memory_off.is_none(),
        set_in: setup
            .first("autoMemoryEnabled")
            .map(|(_, file)| file.path.clone()),
    });
    if memdir.set_in.is_some() {
        out.push(Assumption::Mode {
            name: "autoMemoryDirectory".to_owned(),
            value: memdir.dir.to_string(),
            set_in: memdir.set_in.clone(),
        });
    }
    let _ = cx;
    for what in [
        "claude.assumption.server_managed_policy",
        "claude.assumption.mdm_policy",
        "claude.assumption.feature_flags",
        "claude.assumption.mcp_instructions",
        "claude.assumption.synced_skills",
    ] {
        out.push(Assumption::Unknown {
            what: RuleRef::new(what),
        });
    }
    out
}

fn settings_claims(setup: &Setup) -> Vec<FileClaim> {
    let mut out: Vec<FileClaim> = setup
        .settings
        .iter()
        .map(|file| FileClaim {
            path: file.path.clone(),
            policy: file.policy(),
        })
        .collect();
    out.push(FileClaim {
        path: setup.global_path.clone(),
        policy: EditPolicy::ReadOnly {
            why: ReadOnlyWhy::AgentState,
        },
    });
    let _ = SettingsKind::User;
    out
}

/// Spec 6.2: managed files, the server-managed cache, `~/.claude.json` and the plugin cache are
/// read-only. Strict JSON for settings files. A rule whose frontmatter newly stops parsing may
/// never load (spec 1.4).
fn guard(path: &AbsPath, before: Option<&str>, after: Option<&str>) -> Result<(), ApiError> {
    let p = path.as_str();
    let name = path.file_name().unwrap_or("");
    let refuse = |why: &'static str| {
        Err(ApiError::ReadOnly {
            why: ReadOnlyReason::Rule {
                rule: RuleRef::new(why),
            },
        })
    };
    if [
        "/Library/Application Support/ClaudeCode",
        "/etc/claude-code",
    ]
    .iter()
    .any(|m| p == *m || p.starts_with(&format!("{m}/")))
    {
        return refuse("claude.guard.managed");
    }
    if name == ".claude.json" {
        return refuse("claude.guard.claude_json");
    }
    if name == "remote-settings.json" {
        return refuse("claude.guard.remote_settings");
    }
    if p.contains("/plugins/cache/") || p.contains("/plugins/marketplaces/") {
        return refuse("claude.guard.plugin_cache");
    }
    if p.contains("/skills/synced/") || p.contains("/plugins/synced/") {
        return refuse("claude.guard.synced");
    }
    let Some(after) = after else { return Ok(()) };
    let settings_file = name == "settings.json" || name == "settings.local.json";
    if settings_file && serde_json::from_str::<Value>(after).is_err() {
        return Err(ApiError::InvalidEdit {
            field: "json".to_owned(),
            why: RuleRef::new("claude.guard.settings_not_json"),
        });
    }
    if p.contains("/.claude/rules/") && name.ends_with(".md") {
        let broke = crate::md::frontmatter::parse(after).error.is_some()
            && before.is_none_or(|b| crate::md::frontmatter::parse(b).error.is_none());
        if broke {
            return Err(ApiError::InvalidEdit {
                field: "frontmatter".to_owned(),
                why: RuleRef::new("claude.guard.rule_frontmatter"),
            });
        }
    }
    Ok(())
}
