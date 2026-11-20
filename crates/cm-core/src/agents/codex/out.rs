//! The accumulator the adapter's passes write into.

use std::collections::BTreeMap;

use crate::agents::{FileClaim, Pieces};
use crate::model::*;

pub(super) fn me() -> AgentId {
    AgentId::builtin("codex")
}

/// What one pass wants to record about a load. Defaults describe a session-start load.
pub(super) struct NewLoad {
    pub file: AbsPath,
    pub kind: DocKind,
    pub scope: Scope,
    pub via: Via,
    pub when: Activation,
    pub status: LoadStatus,
    pub delivered: Option<Delivered>,
}

#[derive(Default)]
pub(super) struct Out {
    pub loads: Vec<Load>,
    pub files: Vec<FileClaim>,
    pub diagnostics: Vec<Diagnostic>,
    pub assumptions: Vec<Assumption>,
    pub extensions: Vec<Extension>,
    pub hooks: Vec<Hook>,
    pub hook_files: Vec<HookFile>,
    pub memory: Vec<MemoryStore>,
    pub slots: Vec<Slot>,
    /// Literal session-start blocks in delivery order, with the loads each is made of.
    pub blocks: Vec<Block>,
    /// The text of each delivered load that fires after the session starts, as the scan counted
    /// it. `load_texts` returns it.
    pub texts: BTreeMap<LoadId, String>,
}

/// One session-start block: the literal text and the loads inside it, as pieces.
pub(super) struct Block {
    pub delivery: Delivery,
    pub label: BlockLabel,
    pub pieces: Pieces,
}

impl Block {
    /// A block that is one load's text and nothing else.
    pub fn one(delivery: Delivery, label: BlockLabel, load: LoadId, text: String) -> Block {
        let mut pieces = Pieces::default();
        pieces.load(load, text);
        Block {
            delivery,
            label,
            pieces,
        }
    }
}

impl Out {
    pub fn push_load(&mut self, new: NewLoad) -> LoadId {
        let id = LoadId(self.loads.len() as u32);
        self.loads.push(Load {
            id: id.clone(),
            file: new.file,
            kind: new.kind,
            scope: new.scope,
            via: new.via,
            when: new.when,
            status: new.status,
            delivered: new.delivered,
        });
        id
    }

    pub fn claim(&mut self, path: &AbsPath, policy: EditPolicy) {
        if !self.files.iter().any(|f| &f.path == path) {
            self.files.push(FileClaim {
                path: path.clone(),
                policy,
            });
        }
    }

    pub fn diagnose(&mut self, severity: Severity, at: Subject, problem: Problem) {
        self.diagnostics.push(Diagnostic {
            severity,
            agent: Some(me()),
            at,
            problem,
            fix: None,
        });
    }

    pub fn warn_file(&mut self, path: &AbsPath, problem: Problem) {
        self.diagnose(
            Severity::Warning,
            Subject::File {
                path: path.clone(),
                line: None,
            },
            problem,
        );
    }
}

pub(super) fn discovery(rule: &'static str) -> Via {
    Via::Discovery {
        rule: RuleRef::new(rule),
    }
}

pub(super) fn delivered(text: &str, bytes: u64) -> Delivered {
    Delivered {
        bytes,
        tokens: crate::tokens::count(text),
        transformed: false,
    }
}
