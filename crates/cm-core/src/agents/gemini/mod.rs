//! Gemini CLI. Specified in `docs/agents/gemini-cli.md`.

use std::sync::OnceLock;

use super::{Agent, AgentScan, ScanCx, home_only_scan, home_with_env, rules_from};
use crate::model::{AgentId, HookSchema, RuleSpec};

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
        &super::NO_HOOKS
    }

    /// docs/agents/gemini-cli.md section 7: `GEMINI_CLI_HOME` replaces the home directory, so config is `<it>/.gemini`.
    fn scan(&self, cx: &ScanCx) -> AgentScan {
        let home = home_with_env(cx, "GEMINI_CLI_HOME", ".gemini", true);
        home_only_scan(cx, self.id(), "gemini", "gemini.home", home)
    }
}
