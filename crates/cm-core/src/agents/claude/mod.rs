//! Claude Code. Specified in `docs/agents/claude-code.md`.

use std::sync::OnceLock;

use super::{Agent, AgentScan, ScanCx, home_only_scan, home_with_env, rules_from};
use crate::model::{AgentId, HookSchema, RuleSpec};

mod extensions;
mod hooks;
mod setup;

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

    /// docs/agents/claude-code.md section 1.1: `CLAUDE_CONFIG_DIR`, else `~/.claude`.
    fn scan(&self, cx: &ScanCx) -> AgentScan {
        let home = home_with_env(cx, "CLAUDE_CONFIG_DIR", ".claude", false);
        home_only_scan(cx, self.id(), "claude", "claude.home", home)
    }

    fn sample_input(&self, event: &str, cx: &ScanCx) -> serde_json::Value {
        hooks::sample_input(event, cx)
    }

    fn exec_spec(
        &self,
        hook: &crate::model::Hook,
        input: &serde_json::Value,
        cx: &ScanCx,
    ) -> Result<crate::model::ExecSpec, crate::model::RuleRef> {
        hooks::exec_spec(hook, input, cx)
    }
}
