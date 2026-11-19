//! Claude Code. Specified in `docs/agents/claude-code.md`.

use std::sync::OnceLock;

use super::{Agent, AgentScan, ScanCx, home_only_scan, home_with_env, rules_from};
use crate::model::{AgentId, HookSchema, RuleSpec};

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
        rules_from(&RULES, include_str!("rules.toml"))
    }

    fn hook_schema(&self) -> &'static HookSchema {
        &super::NO_HOOKS
    }

    /// docs/agents/claude-code.md section 1.1: `CLAUDE_CONFIG_DIR`, else `~/.claude`.
    fn scan(&self, cx: &ScanCx) -> AgentScan {
        let home = home_with_env(cx, "CLAUDE_CONFIG_DIR", ".claude", false);
        home_only_scan(cx, self.id(), "claude", "claude.home", home)
    }
}
