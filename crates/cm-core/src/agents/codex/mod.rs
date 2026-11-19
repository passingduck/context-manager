//! Codex CLI. Specified in `docs/agents/codex.md`.

use std::sync::OnceLock;

use super::{Agent, AgentScan, ScanCx, home_only_scan, home_with_env, rules_from};
use crate::model::{AgentId, HookSchema, RuleSpec};

pub struct Codex;

static RULES: OnceLock<Vec<RuleSpec>> = OnceLock::new();

impl Agent for Codex {
    fn id(&self) -> AgentId {
        AgentId::builtin("codex")
    }

    fn display_name(&self) -> &'static str {
        "Codex CLI"
    }

    fn rules(&self) -> &'static [RuleSpec] {
        rules_from(&RULES, include_str!("rules.toml"))
    }

    fn hook_schema(&self) -> &'static HookSchema {
        &super::NO_HOOKS
    }

    /// docs/agents/codex.md section 6.1: `CODEX_HOME`, else `~/.codex`.
    fn scan(&self, cx: &ScanCx) -> AgentScan {
        let home = home_with_env(cx, "CODEX_HOME", ".codex", false);
        home_only_scan(cx, self.id(), "codex", "codex.home", home)
    }
}
