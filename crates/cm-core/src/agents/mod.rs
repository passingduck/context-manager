//! Agent adapters (design section 5).
//!
//! An adapter reads the disk only through [`ProbeFs`] and the environment only through
//! [`ScanEnv`], both reached through [`ScanCx`]. This unit's adapters resolve their home
//! directory and their presence and report nothing else.

use std::sync::OnceLock;

use serde::Deserialize;

use crate::env::ScanEnv;
use crate::model::*;
use crate::probe_fs::{EntryKind, ProbeFs};
use crate::tokens;

pub mod claude;
pub mod codex;
pub mod gemini;

/// The shared parts every adapter reads through.
pub struct ScanCx<'a> {
    pub env: &'a ScanEnv,
    pub fs: &'a ProbeFs,
    /// Where the tool keeps parked hook records. `None` for a caller that has no data
    /// directory: no hook is then reported as parked.
    pub(crate) parked_dir: Option<&'a AbsPath>,
}

impl<'a> ScanCx<'a> {
    pub fn new(env: &'a ScanEnv, fs: &'a ProbeFs) -> ScanCx<'a> {
        ScanCx {
            env,
            fs,
            parked_dir: None,
        }
    }

    /// Reads the parked hook records in `dir`, which only the Claude hook reader uses.
    pub fn with_parked(self, dir: &'a AbsPath) -> ScanCx<'a> {
        ScanCx {
            parked_dir: Some(dir),
            ..self
        }
    }
}

/// What a planner reads through.
pub struct PlanCx<'a> {
    pub env: &'a ScanEnv,
    pub fs: &'a ProbeFs,
    /// The fresh scan the plan runs against.
    pub snapshot: &'a Snapshot,
    /// Where the tool keeps parked hook records (`parked/`). A planner may write inside it.
    pub parked_dir: &'a AbsPath,
    /// The hook a `UpdateHook`, `RemoveHook` or `SetHookEnabled` names, resolved in `snapshot`.
    /// The planner resolves it and calls only the agent that lists it. `None` for every other edit.
    pub hook: Option<&'a Hook>,
}

/// A file an adapter looked at, with the edit policy it claims for it.
#[derive(Debug, Clone, PartialEq)]
pub struct FileClaim {
    pub path: AbsPath,
    pub policy: EditPolicy,
}

/// What one adapter returns from a scan.
#[derive(Debug, Clone, PartialEq)]
pub struct AgentScan {
    pub view: AgentView,
    /// Files that the loads do not name but the agent uses, such as probed locations.
    pub files: Vec<FileClaim>,
    pub diagnostics: Vec<Diagnostic>,
}

/// A load the agent would add when it touches a file in a directory.
#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    pub file: AbsPath,
    pub kind: DocKind,
    pub scope: Scope,
    pub rule: RuleRef,
    pub access: AccessRule,
    /// Set when the agent finds the file and does not load it, such as in an untrusted folder.
    pub skip: Option<Skip>,
}

pub trait Agent: Sync {
    fn id(&self) -> AgentId;
    fn display_name(&self) -> &'static str;
    /// Loaded from `rules.toml` next to the adapter.
    fn rules(&self) -> &'static [RuleSpec];
    /// Events, matcher kind per event, handler kinds and their fields.
    fn hook_schema(&self) -> &'static HookSchema;
    /// Every discovery rule, applied to one environment.
    fn scan(&self, cx: &ScanCx) -> AgentScan;

    /// What loads when the agent touches a file in `dir`.
    fn on_access(&self, _cx: &ScanCx, _dir: &AbsPath) -> Vec<Candidate> {
        Vec::new()
    }
    /// The literal session-start text, piece by piece.
    fn render(&self, _cx: &ScanCx, _view: &AgentView) -> Vec<RenderedBlock> {
        Vec::new()
    }
    /// The text the model receives for a delivered load that has no piece of its own in a
    /// session-start block: a body on invoke, a file on access and what it imports, a file the
    /// model may read by itself. The default is the file as it is.
    fn load_text(&self, cx: &ScanCx, _view: &AgentView, load: &Load) -> Option<String> {
        file_text(cx, &load.file)
    }
    fn known_projects(&self, _cx: &ScanCx) -> Vec<AbsPath> {
        Vec::new()
    }
    /// `None`: the generic plan applies.
    fn plan(&self, _cx: &PlanCx, _edit: &Edit) -> Option<Result<Preview, ApiError>> {
        None
    }
    /// Warnings for one planned write or trash of a file this agent uses, whoever planned it. A
    /// plain save gets them too. `before` and `after` are the file's text, `None` when absent.
    fn write_warnings(
        &self,
        _cx: &PlanCx,
        _path: &AbsPath,
        _before: Option<&str>,
        _after: Option<&str>,
    ) -> Vec<Warning> {
        Vec::new()
    }
    /// Runs on every planned write.
    fn guard(
        &self,
        _path: &AbsPath,
        _before: Option<&str>,
        _after: Option<&str>,
    ) -> Result<(), ApiError> {
        Ok(())
    }
    fn exec_spec(
        &self,
        _hook: &Hook,
        _input: &serde_json::Value,
        _cx: &ScanCx,
    ) -> Result<ExecSpec, RuleRef> {
        Err(RuleRef::new("hook.run_unsupported"))
    }
    fn sample_input(&self, _event: &str, _cx: &ScanCx) -> serde_json::Value {
        serde_json::Value::Null
    }
}

/// A file as it is on disk, decoded lossily.
pub(crate) fn file_text(cx: &ScanCx, path: &AbsPath) -> Option<String> {
    let bytes = cx.fs.read(path).ok()?;
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

/// The pieces of one session-start block, in delivery order. Adjacent frames merge into one, and
/// an empty frame is dropped.
#[derive(Debug, Default)]
pub(crate) struct Pieces(Vec<Piece>);

impl Pieces {
    pub fn frame(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        match self.0.last_mut() {
            Some(Piece::Frame { text: last }) => last.push_str(text),
            _ => self.0.push(Piece::Frame {
                text: text.to_owned(),
            }),
        }
    }

    pub fn load(&mut self, load: LoadId, text: impl Into<String>) {
        self.0.push(Piece::Load {
            load,
            text: text.into(),
        });
    }
}

/// The scan's description of a block and its literal text, both from one list of pieces. Each
/// piece is counted on its own, and the block costs the sum, so `parts` add up to `tokens`.
pub(crate) fn assemble(
    delivery: Delivery,
    label: BlockLabel,
    fidelity: Fidelity,
    pieces: Pieces,
) -> (ContextBlock, RenderedBlock) {
    let parts: Vec<Part> = pieces
        .0
        .iter()
        .map(|piece| match piece {
            Piece::Frame { text } => Part::Glue {
                tokens: tokens::count(text),
            },
            Piece::Load { load, text } => Part::Load {
                load: load.clone(),
                tokens: tokens::count(text),
            },
        })
        .collect();
    let total = parts
        .iter()
        .map(|p| match p {
            Part::Glue { tokens } | Part::Load { tokens, .. } => *tokens,
        })
        .sum();
    (
        ContextBlock {
            delivery,
            label,
            parts,
            tokens: total,
            fidelity,
            preceded_by: Vec::new(),
        },
        RenderedBlock {
            delivery,
            label,
            pieces: pieces.0,
            tokens: total,
            fidelity,
        },
    )
}

/// Registry order is the order agents appear in a snapshot. A fourth agent adds one line here.
pub fn registry() -> &'static [&'static dyn Agent] {
    &[&claude::Claude, &codex::Codex, &gemini::Gemini]
}

pub fn find(id: &AgentId) -> Option<&'static dyn Agent> {
    find_in(registry(), id)
}

/// The agent with `id` among `agents`. Every pairing of an agent with its view goes through this
/// lookup, never through a shared position.
pub fn find_in<'a>(agents: &[&'a dyn Agent], id: &AgentId) -> Option<&'a dyn Agent> {
    agents.iter().copied().find(|agent| agent.id() == *id)
}

// ---------------------------------------------------------------------------------------------
// Shared adapter helpers
// ---------------------------------------------------------------------------------------------

#[derive(Deserialize)]
struct RulesFile {
    rule: Vec<RuleSpec>,
}

/// Parses an adapter's `rules.toml`. The text is embedded, so a parse error is a build mistake.
pub(crate) fn rules_from(
    cell: &'static OnceLock<Vec<RuleSpec>>,
    source: &'static str,
) -> &'static [RuleSpec] {
    cell.get_or_init(|| {
        toml::from_str::<RulesFile>(source)
            .expect("rules.toml parses")
            .rule
    })
}

/// The rules that belong to no agent, from `core_rules.toml`.
pub fn core_rules() -> &'static [RuleSpec] {
    static CELL: OnceLock<Vec<RuleSpec>> = OnceLock::new();
    rules_from(&CELL, include_str!("../core_rules.toml"))
}

/// Where an agent's home directory is, and the variable that moved it, if one did.
pub(crate) struct HomeChoice {
    pub path: AbsPath,
    pub from_env: Option<&'static str>,
}

/// Resolves a directory that an environment variable can replace.
pub(crate) fn home_with_env(
    cx: &ScanCx,
    var: &'static str,
    default_under_home: &str,
    var_is_parent: bool,
) -> HomeChoice {
    match cx.env.var(var).and_then(|raw| AbsPath::parse(raw).ok()) {
        Some(set) => HomeChoice {
            path: if var_is_parent {
                set.join(default_under_home)
            } else {
                set
            },
            from_env: Some(var),
        },
        None => HomeChoice {
            path: cx.env.home.join(default_under_home),
            from_env: None,
        },
    }
}

/// `Installed` when `bin` is on `PATH`, `ConfigOnly` when only the home directory exists.
pub(crate) fn presence(cx: &ScanCx, bin: &str, home_exists: bool) -> Presence {
    let on_path = cx
        .env
        .var("PATH")
        .unwrap_or("")
        .split(':')
        .filter(|dir| dir.starts_with('/'))
        .find_map(|dir| {
            let candidate = AbsPath::parse(dir).ok()?.join(bin);
            let stat = cx.fs.stat(&candidate).ok()?;
            (stat.kind == EntryKind::File).then_some(candidate)
        });
    match on_path {
        Some(bin) => Presence::Installed { bin },
        None if home_exists => Presence::ConfigOnly,
        None => Presence::Absent,
    }
}

pub(crate) fn home_dir(cx: &ScanCx, role: &'static str, choice: &HomeChoice) -> HomeDir {
    let exists = cx
        .fs
        .stat(&choice.path)
        .map(|stat| stat.kind == EntryKind::Dir)
        .unwrap_or(false);
    HomeDir {
        role: RuleRef::new(role),
        path: choice.path.clone(),
        from_env: choice.from_env.map(str::to_owned),
        exists,
    }
}

/// The budget an adapter reports. `passes::budget` computes the real one after every adapter
/// has run, from the view's session blocks and loads.
pub(crate) fn unbudgeted() -> Budget {
    Budget {
        session_start: 0,
        by_label: Vec::new(),
        on_access_max: 0,
        method: TokenMethod::O200kEstimate,
    }
}

pub(crate) fn empty_view(agent: AgentId, presence: Presence, homes: Vec<HomeDir>) -> AgentView {
    AgentView {
        agent,
        presence,
        homes,
        trust: Trust {
            level: TrustLevel::Unknown,
            recorded_in: None,
        },
        assumptions: Vec::new(),
        loads: Vec::new(),
        session: Vec::new(),
        budget: unbudgeted(),
        hooks: Vec::new(),
        hook_files: Vec::new(),
        memory: Vec::new(),
        extensions: Vec::new(),
        slots: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scan::scan;
    use crate::testing::Fixture;

    #[test]
    fn registry_lists_three_agents_in_order() {
        let ids: Vec<_> = registry()
            .iter()
            .map(|a| a.id().as_str().to_owned())
            .collect();
        assert_eq!(ids, ["claude", "codex", "gemini"]);
        assert!(AgentId::parse("claude").is_ok());
        assert!(AgentId::parse("emacs").is_err());
    }

    #[test]
    fn every_rule_has_both_languages_and_evidence() {
        for agent in registry() {
            assert!(!agent.rules().is_empty(), "{} has no rules", agent.id());
            for rule in agent.rules() {
                assert!(
                    rule.id.0.starts_with(agent.id().as_str()),
                    "{} is in the wrong file",
                    rule.id.0
                );
                assert!(
                    !rule.en.trim().is_empty() && !rule.ko.trim().is_empty(),
                    "{} lacks a language",
                    rule.id.0
                );
                assert!(
                    !rule.evidence.trim().is_empty(),
                    "{} lacks evidence",
                    rule.id.0
                );
            }
        }
    }

    #[test]
    fn every_rule_ref_a_scan_uses_resolves_in_its_rules_file() {
        let fx = Fixture::new();
        fx.mkdir(&fx.home_path(".claude"));
        let snapshot = scan(&fx.env());
        for view in &snapshot.agents {
            let agent = find(&view.agent).unwrap();
            let known: Vec<_> = agent.rules().iter().map(|r| &r.id).collect();
            let used = view.homes.iter().map(|h| &h.role);
            for rule in used {
                assert!(
                    known.contains(&rule),
                    "{} is not in {}'s rules.toml",
                    rule.0,
                    view.agent
                );
            }
        }
    }
}
