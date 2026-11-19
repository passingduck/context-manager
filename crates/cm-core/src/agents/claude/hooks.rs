//! Claude Code hooks (spec 3). Owned by the hooks and extensions half of unit 6.

use serde_json::Value;

use super::extensions::ExtOut;
use super::setup::Setup;
use crate::agents::{FileClaim, ScanCx};
use crate::model::*;

pub(super) struct HooksOut {
    pub hooks: Vec<Hook>,
    pub hook_files: Vec<HookFile>,
    pub diagnostics: Vec<Diagnostic>,
    /// Files this half looked at that no load names.
    pub files: Vec<FileClaim>,
}

/// Every hook of every definition location that is a file (spec 3, "Definition locations").
pub(super) fn scan(_cx: &ScanCx, _setup: &Setup, _ext: &ExtOut) -> HooksOut {
    HooksOut {
        hooks: Vec::new(),
        hook_files: Vec::new(),
        diagnostics: Vec::new(),
        files: Vec::new(),
    }
}

/// The 33 events, the matcher kind of each, the handler kinds and their fields.
pub(super) fn schema() -> &'static HookSchema {
    &crate::agents::NO_HOOKS
}

pub(super) fn sample_input(_event: &str, _cx: &ScanCx) -> Value {
    Value::Null
}

pub(super) fn exec_spec(_hook: &Hook, _input: &Value, _cx: &ScanCx) -> Result<ExecSpec, RuleRef> {
    Err(RuleRef::new("hook.run_unsupported"))
}
