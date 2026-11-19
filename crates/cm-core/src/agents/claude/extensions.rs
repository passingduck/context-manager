//! Claude Code skills, commands, subagents and plugins (spec 4). Owned by the hooks and
//! extensions half of unit 6.

use super::setup::Setup;
use crate::agents::{FileClaim, ScanCx};
use crate::model::*;

pub(super) struct ExtOut {
    pub extensions: Vec<Extension>,
    /// Listing and body loads of the extensions. `Load.id` and every `LoadId` inside
    /// `extensions` already count from the `first_load_id` the caller passed.
    pub loads: Vec<Load>,
    pub diagnostics: Vec<Diagnostic>,
    pub files: Vec<FileClaim>,
}

/// Every discovery location of spec 4. `first_load_id` is the index the first returned load
/// will have in `AgentView.loads`.
pub(super) fn scan(_cx: &ScanCx, _setup: &Setup, _first_load_id: u32) -> ExtOut {
    ExtOut {
        extensions: Vec::new(),
        loads: Vec::new(),
        diagnostics: Vec::new(),
        files: Vec::new(),
    }
}
