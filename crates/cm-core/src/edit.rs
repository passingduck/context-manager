//! Planning (design section 4): an [`Edit`] becomes a [`Preview`]. Reads only. Nothing here
//! writes a file; `cm-host::apply` executes the change set a plan returns.
//!
//! Every plan, generic or from an agent, passes the same checks before it is returned:
//!
//! - each path is an `Editable` file of the snapshot, a slot, or (for an agent's plan) inside the
//!   parked-hook directory, else `OutsideBoundary` or `ReadOnly`;
//! - each operation's `expect` still matches the disk, else `Conflict` with the current text;
//! - each agent that uses the file accepts the before and after text through `Agent::guard`.
//!
//! A plan that the disk already satisfies has no operations. The digest is the SHA-256 of the
//! change set serialized as JSON. Field order is the struct's declaration order, so the bytes are
//! the same on every run.

use std::collections::BTreeMap;

use sha2::{Digest, Sha256};

use crate::agents::{Agent, PlanCx};
use crate::env::ScanEnv;
use crate::model::*;
use crate::probe_fs::{EntryKind, ProbeFs};
use crate::scan::{MAX_READ_BYTES, Scanned};

/// A directory trashed whole keeps its files as history blobs up to this many bytes. Over it,
/// the OS trash is the only copy and the plan warns [`Warning::NotRevertible`].
pub const MAX_TREE_JOURNAL_BYTES: u64 = 16 * 1024 * 1024;

const TREE_DEPTH: usize = 16;
const TREE_ENTRIES: usize = 10_000;

/// Where `Revert` reads a recorded change from. The host implements it over its history.
pub trait ChangeSource {
    /// `ApiError::NotFound` when no such change exists.
    fn detail(&self, id: &ChangeId) -> Result<ChangeDetail, ApiError>;
}

/// Everything a plan reads: one fresh scan, the agents, and the history.
pub struct Planner<'a> {
    pub agents: &'a [&'a dyn Agent],
    pub scanned: &'a Scanned,
    pub env: &'a ScanEnv,
    pub parked_dir: &'a AbsPath,
    pub changes: &'a dyn ChangeSource,
}

/// Who asked for a path, which decides what the boundary lets through.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// An editor save or a trash from the client: snapshot files and slots only.
    Client,
    /// An agent's planner: also the parked-hook directory and directories of a slot.
    Agent,
    /// A revert restores paths a recorded change touched. They were checked when recorded, and a
    /// trashed file is no longer in the snapshot. A read-only claim still refuses.
    Revert,
}

enum Disk {
    Absent,
    Text { text: String, rev: Revision },
    NotUtf8,
    Unreadable(String),
}

impl Disk {
    fn text(&self) -> Option<&str> {
        match self {
            Disk::Text { text, .. } => Some(text),
            _ => None,
        }
    }

    fn base(&self) -> Base {
        match self {
            Disk::Text { rev, .. } => Base::Rev { rev: rev.clone() },
            _ => Base::Absent,
        }
    }
}

/// What a plan has so far. `finish` validates it and turns it into a `Preview`.
struct Draft {
    summary: ChangeSummary,
    ops: Vec<FileOp>,
    before: BTreeMap<AbsPath, Option<String>>,
    warnings: Vec<Warning>,
    conflicts: Vec<Conflict>,
}

struct Tree {
    files: Vec<(AbsPath, Vec<u8>)>,
    hash: String,
}

pub fn digest_of(change_set: &ChangeSet) -> String {
    let bytes = serde_json::to_vec(change_set).expect("a change set serializes");
    sha256_hex(&bytes)
}

pub fn change_edit_of(edit: &Edit) -> ChangeEdit {
    match edit {
        Edit::Save { .. } => ChangeEdit::Save,
        Edit::Trash { .. } => ChangeEdit::Trash,
        Edit::NewItem { .. } => ChangeEdit::NewItem,
        Edit::AddMemory { .. } => ChangeEdit::AddMemory,
        Edit::AddHook { .. } => ChangeEdit::AddHook,
        Edit::UpdateHook { .. } => ChangeEdit::UpdateHook,
        Edit::RemoveHook { .. } => ChangeEdit::RemoveHook,
        Edit::SetHookEnabled { .. } => ChangeEdit::SetHookEnabled,
        Edit::Revert { .. } => ChangeEdit::Revert,
    }
}

fn invalid(field: &str, why: &str) -> ApiError {
    ApiError::InvalidEdit {
        field: field.to_owned(),
        why: RuleRef::new(why),
    }
}

fn read_only(why: &ReadOnlyWhy) -> ApiError {
    let name = match why {
        ReadOnlyWhy::Managed => "managed",
        ReadOnlyWhy::PluginCache { .. } => "plugin_cache",
        ReadOnlyWhy::Generated => "generated",
        ReadOnlyWhy::AgentState => "agent_state",
        ReadOnlyWhy::Synced => "synced",
        ReadOnlyWhy::NotUtf8 => "not_utf8",
    };
    ApiError::ReadOnly {
        why: RuleRef::new(&format!("edit.read_only.{name}")),
    }
}

impl<'a> Planner<'a> {
    pub fn plan(&self, edit: &Edit) -> Result<Preview, ApiError> {
        match edit {
            Edit::Save { path, base, text } => self.save(path, base, text),
            Edit::Trash { path, base } => self.trash(path, base),
            Edit::Revert { change } => self.revert(change),
            Edit::NewItem { agent, .. }
            | Edit::AddMemory { agent, .. }
            | Edit::AddHook { agent, .. } => self.by_agent(Some(agent), edit),
            Edit::UpdateHook { .. } | Edit::RemoveHook { .. } | Edit::SetHookEnabled { .. } => {
                self.by_agent(None, edit)
            }
        }
    }

    // -----------------------------------------------------------------------------------------
    // Reading the disk
    // -----------------------------------------------------------------------------------------

    fn fs(&self) -> &ProbeFs {
        self.scanned.fs()
    }

    fn disk(&self, path: &AbsPath) -> Disk {
        match self.fs().stat(path) {
            Err(err) if err.is_not_found() => return Disk::Absent,
            Err(err) => return Disk::Unreadable(err.detail),
            Ok(stat) if stat.kind != EntryKind::File => {
                return Disk::Unreadable("not a regular file".to_owned());
            }
            Ok(stat) if stat.len > MAX_READ_BYTES => {
                return Disk::Unreadable("file is too big".to_owned());
            }
            Ok(_) => {}
        }
        match self.fs().read(path) {
            Err(err) if err.is_not_found() => Disk::Absent,
            Err(err) => Disk::Unreadable(err.detail),
            Ok(bytes) => match String::from_utf8(bytes.as_ref().clone()) {
                Ok(text) => Disk::Text {
                    rev: Revision::of(text.as_bytes()),
                    text,
                },
                Err(_) => Disk::NotUtf8,
            },
        }
    }

    /// A disk the plan can work with: absent or text. Anything else is an error.
    fn editable_disk(&self, path: &AbsPath) -> Result<Disk, ApiError> {
        match self.disk(path) {
            Disk::NotUtf8 => Err(read_only(&ReadOnlyWhy::NotUtf8)),
            Disk::Unreadable(detail) => Err(ApiError::Io { detail }),
            ok => Ok(ok),
        }
    }

    fn tree(&self, dir: &AbsPath) -> Tree {
        let mut files = Vec::new();
        let mut seen = 0usize;
        self.walk(dir, 0, &mut seen, &mut files);
        files.sort_by(|a, b| a.0.cmp(&b.0));
        let mut hasher = Sha256::new();
        for (path, bytes) in &files {
            let rel = &path.as_str()[dir.as_str().len()..];
            hasher.update(rel.as_bytes());
            hasher.update([0]);
            hasher.update(Revision::of(bytes).0.as_bytes());
            hasher.update([b'\n']);
        }
        Tree {
            files,
            hash: format!("tree:{}", crate::model::hex(&hasher.finalize())),
        }
    }

    fn walk(
        &self,
        dir: &AbsPath,
        depth: usize,
        seen: &mut usize,
        out: &mut Vec<(AbsPath, Vec<u8>)>,
    ) {
        if depth > TREE_DEPTH {
            return;
        }
        let Ok(entries) = self.fs().list(dir) else {
            return;
        };
        for entry in entries.iter() {
            *seen += 1;
            if *seen > TREE_ENTRIES {
                return;
            }
            let path = dir.join(&entry.name);
            match entry.kind {
                EntryKind::Dir => self.walk(&path, depth + 1, seen, out),
                EntryKind::File => {
                    if let Ok(bytes) = self.fs().read(&path) {
                        out.push((path, bytes.as_ref().clone()));
                    }
                }
                EntryKind::Other => {}
            }
        }
    }

    // -----------------------------------------------------------------------------------------
    // The boundary
    // -----------------------------------------------------------------------------------------

    fn slot_owners(&self, path: &AbsPath) -> Vec<AgentId> {
        let mut owners = Vec::new();
        for view in &self.scanned.snapshot.agents {
            let hit = view.slots.iter().any(|slot| match &slot.place {
                Place::File { path: p } => p == path,
                Place::NamedFile { dir, ext } => {
                    path.parent().as_ref() == Some(dir)
                        && path.file_name().is_some_and(|name| {
                            name.ends_with(&format!(".{}", ext.trim_start_matches('.')))
                        })
                }
                Place::NamedDir { dir, file } => {
                    path.file_name() == Some(file.as_str())
                        && path.parent().and_then(|d| d.parent()).as_ref() == Some(dir)
                }
            });
            if hit {
                owners.push(view.agent.clone());
            }
        }
        owners
    }

    /// The agents whose slot directory contains `path` at any depth.
    fn slot_dir_owners(&self, path: &AbsPath) -> Vec<AgentId> {
        let mut owners = Vec::new();
        for view in &self.scanned.snapshot.agents {
            let hit = view.slots.iter().any(|slot| match &slot.place {
                Place::NamedDir { dir, .. } => path != dir && path.starts_with(dir),
                _ => false,
            });
            if hit {
                owners.push(view.agent.clone());
            }
        }
        owners
    }

    fn check_path(&self, path: &AbsPath, mode: Mode) -> Result<(), ApiError> {
        if let Some(entry) = self.scanned.snapshot.files.get(path) {
            return match &entry.policy {
                EditPolicy::Editable => Ok(()),
                EditPolicy::ReadOnly { why } => Err(read_only(why)),
            };
        }
        let allowed = match mode {
            Mode::Revert => true,
            Mode::Agent => {
                !self.slot_owners(path).is_empty()
                    || !self.slot_dir_owners(path).is_empty()
                    || path.starts_with(self.parked_dir)
            }
            Mode::Client => !self.slot_owners(path).is_empty(),
        };
        if allowed {
            Ok(())
        } else {
            Err(ApiError::OutsideBoundary { path: path.clone() })
        }
    }

    /// Every agent that uses `path`, in registry order.
    fn owners(&self, path: &AbsPath) -> Vec<AgentId> {
        let mut owners: Vec<AgentId> = self
            .scanned
            .snapshot
            .files
            .get(path)
            .map(|entry| entry.agents.clone())
            .unwrap_or_default();
        for id in self
            .slot_owners(path)
            .into_iter()
            .chain(self.slot_dir_owners(path))
        {
            if !owners.contains(&id) {
                owners.push(id);
            }
        }
        owners
    }

    fn guard(
        &self,
        path: &AbsPath,
        before: Option<&str>,
        after: Option<&str>,
    ) -> Result<(), ApiError> {
        let owners = self.owners(path);
        for agent in self.agents.iter().filter(|a| owners.contains(&a.id())) {
            agent.guard(path, before, after)?;
        }
        Ok(())
    }

    // -----------------------------------------------------------------------------------------
    // Finishing a plan
    // -----------------------------------------------------------------------------------------

    fn summary(&self, edit: ChangeEdit, targets: Vec<AbsPath>) -> ChangeSummary {
        let mut agents: Vec<AgentId> = Vec::new();
        for target in &targets {
            for id in self.owners(target) {
                if !agents.contains(&id) {
                    agents.push(id);
                }
            }
        }
        ChangeSummary {
            edit,
            agent: (agents.len() == 1).then(|| agents.remove(0)),
            targets,
        }
    }

    fn empty(&self, summary: ChangeSummary) -> Preview {
        self.preview(summary, Vec::new(), BTreeMap::new(), Vec::new(), Vec::new())
    }

    fn preview(
        &self,
        summary: ChangeSummary,
        ops: Vec<FileOp>,
        before: BTreeMap<AbsPath, Option<String>>,
        warnings: Vec<Warning>,
        conflicts: Vec<Conflict>,
    ) -> Preview {
        let change_set = ChangeSet { summary, ops };
        Preview {
            digest: digest_of(&change_set),
            change_set,
            before,
            warnings,
            conflicts,
        }
    }

    fn finish(&self, mode: Mode, draft: Draft) -> Result<Preview, ApiError> {
        let Draft {
            summary,
            ops,
            mut before,
            mut warnings,
            conflicts,
        } = draft;
        for op in &ops {
            match op.action {
                Action::TrashDir => self.check_dir(&op.path, mode)?,
                _ => self.check_path(&op.path, mode)?,
            }
            self.verify(op, &mut before)?;
            self.guard_op(op, &before)?;
            let through = self.fs().stat(&op.path).ok().filter(|s| s.is_symlink);
            if through.is_some()
                && let Ok(target) = self.fs().real_path(&op.path)
            {
                warnings.push(Warning::ThroughSymlink { target });
            }
            let owners = self.owners(&op.path);
            if owners.len() > 1 {
                warnings.push(Warning::SharedFile { agents: owners });
            }
        }
        Ok(self.preview(summary, ops, before, warnings, conflicts))
    }

    fn check_dir(&self, dir: &AbsPath, mode: Mode) -> Result<(), ApiError> {
        let in_slot = self
            .scanned
            .snapshot
            .agents
            .iter()
            .flat_map(|view| &view.slots)
            .any(|slot| match &slot.place {
                Place::NamedDir { dir: parent, .. } => dir.parent().as_ref() == Some(parent),
                _ => false,
            });
        if in_slot || mode == Mode::Revert {
            Ok(())
        } else {
            Err(ApiError::OutsideBoundary { path: dir.clone() })
        }
    }

    /// Checks that the disk still matches `op.expect` and records the current text.
    fn verify(
        &self,
        op: &FileOp,
        before: &mut BTreeMap<AbsPath, Option<String>>,
    ) -> Result<(), ApiError> {
        if op.action == Action::TrashDir {
            let tree = self.tree(&op.path);
            if op.expect
                != (Base::Tree {
                    hash: tree.hash.clone(),
                })
            {
                return Err(ApiError::Conflict {
                    path: op.path.clone(),
                    expected: op.expect.clone(),
                    found: Base::Tree { hash: tree.hash },
                    current: None,
                });
            }
            return Ok(());
        }
        let disk = self.editable_disk(&op.path)?;
        let matches = match (&op.expect, &disk) {
            (Base::Absent, Disk::Absent) => true,
            (Base::Rev { rev }, Disk::Text { rev: now, .. }) => rev == now,
            _ => false,
        };
        if !matches {
            return Err(ApiError::Conflict {
                path: op.path.clone(),
                expected: op.expect.clone(),
                found: disk.base(),
                current: disk.text().map(str::to_owned),
            });
        }
        before.insert(op.path.clone(), disk.text().map(str::to_owned));
        Ok(())
    }

    fn guard_op(
        &self,
        op: &FileOp,
        before: &BTreeMap<AbsPath, Option<String>>,
    ) -> Result<(), ApiError> {
        match &op.action {
            Action::Write { text } => self.guard(
                &op.path,
                before.get(&op.path).and_then(|b| b.as_deref()),
                Some(text),
            ),
            Action::Trash => self.guard(
                &op.path,
                before.get(&op.path).and_then(|b| b.as_deref()),
                None,
            ),
            Action::TrashDir => {
                for (path, text) in before.iter().filter(|(p, _)| p.starts_with(&op.path)) {
                    self.guard(path, text.as_deref(), None)?;
                }
                Ok(())
            }
        }
    }

    // -----------------------------------------------------------------------------------------
    // The generic plans
    // -----------------------------------------------------------------------------------------

    fn save(&self, path: &AbsPath, base: &Base, text: &str) -> Result<Preview, ApiError> {
        self.check_path(path, Mode::Client)?;
        let summary = self.summary(ChangeEdit::Save, vec![path.clone()]);
        let disk = self.editable_disk(path)?;
        if disk.text() == Some(text) {
            return Ok(self.empty(summary));
        }
        let expect = match (base, &disk) {
            (Base::Tree { .. }, _) => return Err(invalid("base", "edit.base_not_a_file")),
            (Base::Absent, Disk::Absent) => Base::Absent,
            (Base::Rev { rev }, Disk::Text { rev: now, .. }) if rev == now => base.clone(),
            _ => {
                return Err(ApiError::Conflict {
                    path: path.clone(),
                    expected: base.clone(),
                    found: disk.base(),
                    current: disk.text().map(str::to_owned),
                });
            }
        };
        self.finish(
            Mode::Client,
            Draft {
                summary,
                ops: vec![FileOp {
                    path: path.clone(),
                    expect,
                    action: Action::Write {
                        text: text.to_owned(),
                    },
                }],
                before: BTreeMap::new(),
                warnings: Vec::new(),
                conflicts: Vec::new(),
            },
        )
    }

    /// A file that is the entry file of a directory slot (`skills/<name>/SKILL.md`) trashes the
    /// whole directory, with the hash of its tree as the base of the operation.
    fn trash(&self, path: &AbsPath, base: &Revision) -> Result<Preview, ApiError> {
        self.check_path(path, Mode::Client)?;
        let summary = self.summary(ChangeEdit::Trash, vec![path.clone()]);
        let disk = self.editable_disk(path)?;
        let text = match &disk {
            Disk::Absent => return Ok(self.empty(summary)),
            Disk::Text { rev, text } if rev == base => text.clone(),
            _ => {
                return Err(ApiError::Conflict {
                    path: path.clone(),
                    expected: Base::Rev { rev: base.clone() },
                    found: disk.base(),
                    current: disk.text().map(str::to_owned),
                });
            }
        };
        let is_dir_entry = self
            .scanned
            .snapshot
            .agents
            .iter()
            .flat_map(|view| &view.slots)
            .any(|slot| match &slot.place {
                Place::NamedDir { dir, file } => {
                    path.file_name() == Some(file.as_str())
                        && path.parent().and_then(|d| d.parent()).as_ref() == Some(dir)
                }
                _ => false,
            });
        if !is_dir_entry {
            return self.finish(
                Mode::Client,
                Draft {
                    summary,
                    ops: vec![FileOp {
                        path: path.clone(),
                        expect: Base::Rev { rev: base.clone() },
                        action: Action::Trash,
                    }],
                    before: BTreeMap::from([(path.clone(), Some(text))]),
                    warnings: Vec::new(),
                    conflicts: Vec::new(),
                },
            );
        }
        let dir = path.parent().expect("an entry file has a directory");
        let tree = self.tree(&dir);
        let total: u64 = tree.files.iter().map(|(_, bytes)| bytes.len() as u64).sum();
        let all_text = tree
            .files
            .iter()
            .all(|(_, bytes)| std::str::from_utf8(bytes).is_ok());
        let mut warnings = Vec::new();
        let mut before = BTreeMap::new();
        if total > MAX_TREE_JOURNAL_BYTES || !all_text {
            warnings.push(Warning::NotRevertible);
        }
        if total <= MAX_TREE_JOURNAL_BYTES {
            for (file, bytes) in &tree.files {
                if let Ok(text) = std::str::from_utf8(bytes) {
                    before.insert(file.clone(), Some(text.to_owned()));
                }
            }
        }
        let summary = self.summary(ChangeEdit::Trash, vec![dir.clone()]);
        self.finish(
            Mode::Client,
            Draft {
                summary,
                ops: vec![FileOp {
                    path: dir,
                    expect: Base::Tree { hash: tree.hash },
                    action: Action::TrashDir,
                }],
                before,
                warnings,
                conflicts: Vec::new(),
            },
        )
    }

    /// Restores each file's before text. A file already at before is skipped. A file at neither
    /// the before nor the after text is a conflict in the preview.
    fn revert(&self, id: &ChangeId) -> Result<Preview, ApiError> {
        let detail = self.changes.detail(id)?;
        let targets: Vec<AbsPath> = detail.files.iter().map(|f| f.path.clone()).collect();
        let summary = self.summary(ChangeEdit::Revert, targets);
        let mut ops = Vec::new();
        let mut before = BTreeMap::new();
        let mut conflicts = Vec::new();
        for file in &detail.files {
            self.check_path(&file.path, Mode::Revert)?;
            let disk = self.editable_disk(&file.path)?;
            if disk.text() == file.before.as_deref() {
                continue;
            }
            if disk.text() != file.after.as_deref() {
                conflicts.push(Conflict {
                    path: file.path.clone(),
                    expected: match &file.after {
                        Some(text) => Base::Rev {
                            rev: Revision::of(text.as_bytes()),
                        },
                        None => Base::Absent,
                    },
                    found: disk.base(),
                    current: disk.text().map(str::to_owned),
                });
                continue;
            }
            let action = match &file.before {
                Some(text) => Action::Write { text: text.clone() },
                None => Action::Trash,
            };
            ops.push(FileOp {
                path: file.path.clone(),
                expect: disk.base(),
                action,
            });
            before.insert(file.path.clone(), disk.text().map(str::to_owned));
        }
        self.finish(
            Mode::Revert,
            Draft {
                summary,
                ops,
                before,
                warnings: Vec::new(),
                conflicts,
            },
        )
    }

    // -----------------------------------------------------------------------------------------
    // Agent-specific edits
    // -----------------------------------------------------------------------------------------

    fn by_agent(&self, agent: Option<&AgentId>, edit: &Edit) -> Result<Preview, ApiError> {
        let candidates: Vec<&&dyn Agent> = self
            .agents
            .iter()
            .filter(|a| agent.is_none_or(|id| a.id() == *id))
            .collect();
        if agent.is_some() && candidates.is_empty() {
            return Err(invalid("agent", "edit.unknown_agent"));
        }
        let cx = PlanCx {
            env: self.env,
            fs: self.fs(),
            snapshot: &self.scanned.snapshot,
            parked_dir: self.parked_dir,
        };
        for candidate in candidates {
            if let Some(result) = candidate.plan(&cx, edit) {
                let preview = result?;
                return self.finish(
                    Mode::Agent,
                    Draft {
                        summary: preview.change_set.summary,
                        ops: preview.change_set.ops,
                        before: preview.before,
                        warnings: preview.warnings,
                        conflicts: preview.conflicts,
                    },
                );
            }
        }
        Err(invalid("edit", "edit.unsupported"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::{AgentScan, FileClaim, ScanCx, home_only_scan, home_with_env};
    use crate::scan::scan_with;
    use crate::testing::Fixture;

    /// Claims the files it is given and refuses any text that contains `refuse`.
    pub struct TestAgent {
        pub files: Vec<AbsPath>,
        pub readonly: Vec<AbsPath>,
        pub slot: Option<Place>,
        pub refuse: Option<&'static str>,
    }

    impl Agent for TestAgent {
        fn id(&self) -> AgentId {
            AgentId::parse("claude").unwrap()
        }
        fn display_name(&self) -> &'static str {
            "test"
        }
        fn rules(&self) -> &'static [RuleSpec] {
            &[]
        }
        fn hook_schema(&self) -> &'static HookSchema {
            &crate::agents::NO_HOOKS
        }
        fn scan(&self, cx: &ScanCx) -> AgentScan {
            let mut scan = home_only_scan(
                cx,
                self.id(),
                "claude",
                "claude.home",
                home_with_env(cx, "CLAUDE_CONFIG_DIR", ".claude", false),
            );
            for file in &self.files {
                scan.files.push(FileClaim {
                    path: file.clone(),
                    policy: EditPolicy::Editable,
                });
            }
            for file in &self.readonly {
                scan.files.push(FileClaim {
                    path: file.clone(),
                    policy: EditPolicy::ReadOnly {
                        why: ReadOnlyWhy::Managed,
                    },
                });
            }
            if let Some(place) = &self.slot {
                scan.view.slots.push(Slot {
                    id: SlotId("s".into()),
                    place: place.clone(),
                    new_kind: NewItemKind::Skill,
                    scope: Scope::User,
                    rule: RuleRef::new("claude.slot"),
                });
            }
            scan
        }
        fn guard(
            &self,
            _path: &AbsPath,
            _before: Option<&str>,
            after: Option<&str>,
        ) -> Result<(), ApiError> {
            match (self.refuse, after) {
                (Some(word), Some(text)) if text.contains(word) => Err(ApiError::Forbidden {
                    why: RuleRef::new("test.refused"),
                }),
                _ => Ok(()),
            }
        }
    }

    struct NoHistory;
    impl ChangeSource for NoHistory {
        fn detail(&self, _: &ChangeId) -> Result<ChangeDetail, ApiError> {
            Err(ApiError::NotFound)
        }
    }

    fn plan_with(agent: &TestAgent, fx: &Fixture, edit: &Edit) -> Result<Preview, ApiError> {
        let scanned = scan_with(&[agent as &dyn Agent], &fx.env());
        let env = fx.env();
        let parked = fx.root().join("data/parked");
        Planner {
            agents: &[agent as &dyn Agent],
            scanned: &scanned,
            env: &env,
            parked_dir: &parked,
            changes: &NoHistory,
        }
        .plan(edit)
    }

    fn agent(files: &[&AbsPath]) -> TestAgent {
        TestAgent {
            files: files.iter().map(|p| (*p).clone()).collect(),
            readonly: Vec::new(),
            slot: None,
            refuse: None,
        }
    }

    fn rev(text: &str) -> Base {
        Base::Rev {
            rev: Revision::of(text.as_bytes()),
        }
    }

    #[test]
    fn a_save_plans_one_write_with_the_current_text_as_before() {
        let fx = Fixture::new();
        let file = fx.project_path("A.md");
        fx.write(&file, "old");
        let preview = plan_with(
            &agent(&[&file]),
            &fx,
            &Edit::Save {
                path: file.clone(),
                base: rev("old"),
                text: "new".into(),
            },
        )
        .unwrap();
        assert_eq!(preview.change_set.ops.len(), 1);
        assert_eq!(preview.before[&file].as_deref(), Some("old"));
        assert_eq!(preview.digest, digest_of(&preview.change_set));
    }

    #[test]
    fn a_save_whose_text_is_on_disk_plans_to_nothing() {
        let fx = Fixture::new();
        let file = fx.project_path("A.md");
        fx.write(&file, "same");
        let preview = plan_with(
            &agent(&[&file]),
            &fx,
            &Edit::Save {
                path: file.clone(),
                base: rev("older"),
                text: "same".into(),
            },
        )
        .unwrap();
        assert!(preview.change_set.ops.is_empty());
    }

    #[test]
    fn a_stale_base_is_a_conflict_with_the_current_text() {
        let fx = Fixture::new();
        let file = fx.project_path("A.md");
        fx.write(&file, "outside");
        let err = plan_with(
            &agent(&[&file]),
            &fx,
            &Edit::Save {
                path: file.clone(),
                base: rev("loaded"),
                text: "mine".into(),
            },
        )
        .unwrap_err();
        assert_eq!(
            err,
            ApiError::Conflict {
                path: file,
                expected: rev("loaded"),
                found: rev("outside"),
                current: Some("outside".into()),
            }
        );
    }

    #[test]
    fn paths_outside_the_snapshot_and_read_only_files_are_refused() {
        let fx = Fixture::new();
        let stray = fx.project_path("stray.md");
        let managed = fx.project_path("managed.md");
        fx.write(&managed, "m");
        let mut a = agent(&[]);
        a.readonly.push(managed.clone());
        let save = |path: &AbsPath| Edit::Save {
            path: path.clone(),
            base: Base::Absent,
            text: "x".into(),
        };
        assert_eq!(
            plan_with(&a, &fx, &save(&stray)).unwrap_err(),
            ApiError::OutsideBoundary { path: stray }
        );
        assert!(matches!(
            plan_with(&a, &fx, &save(&managed)).unwrap_err(),
            ApiError::ReadOnly { .. }
        ));
    }

    #[test]
    fn a_slot_path_may_be_created() {
        let fx = Fixture::new();
        let dir = fx.project_path(".claude/commands");
        let mut a = agent(&[]);
        a.slot = Some(Place::NamedFile {
            dir: dir.clone(),
            ext: "md".into(),
        });
        let path = dir.join("deploy.md");
        let preview = plan_with(
            &a,
            &fx,
            &Edit::Save {
                path: path.clone(),
                base: Base::Absent,
                text: "go".into(),
            },
        )
        .unwrap();
        assert_eq!(preview.change_set.summary.targets, vec![path]);
        assert_eq!(preview.change_set.ops[0].expect, Base::Absent);
    }

    #[test]
    fn a_guard_that_refuses_the_text_is_forbidden() {
        let fx = Fixture::new();
        let file = fx.project_path("A.md");
        fx.write(&file, "old");
        let mut a = agent(&[&file]);
        a.refuse = Some("trusted_hash");
        let err = plan_with(
            &a,
            &fx,
            &Edit::Save {
                path: file,
                base: rev("old"),
                text: "trusted_hash = 1".into(),
            },
        )
        .unwrap_err();
        assert!(matches!(err, ApiError::Forbidden { .. }));
    }

    #[test]
    fn an_edit_no_agent_plans_is_invalid() {
        let fx = Fixture::new();
        let err = plan_with(
            &agent(&[]),
            &fx,
            &Edit::RemoveHook {
                hook: HookId("h".into()),
            },
        )
        .unwrap_err();
        assert!(matches!(err, ApiError::InvalidEdit { .. }));
    }

    #[test]
    fn trashing_an_entry_file_trashes_its_directory_and_its_tree_hash_moves_with_the_siblings() {
        let fx = Fixture::new();
        let dir = fx.project_path(".claude/skills");
        let entry = dir.join("deploy/SKILL.md");
        fx.write(&entry, "skill");
        fx.write(&dir.join("deploy/notes.md"), "n");
        let mut a = agent(&[&entry]);
        a.slot = Some(Place::NamedDir {
            dir: dir.clone(),
            file: "SKILL.md".into(),
        });
        let edit = Edit::Trash {
            path: entry.clone(),
            base: Revision::of(b"skill"),
        };
        let first = plan_with(&a, &fx, &edit).unwrap();
        let op = &first.change_set.ops[0];
        assert_eq!(op.action, Action::TrashDir);
        assert_eq!(op.path, dir.join("deploy"));
        assert_eq!(first.before.len(), 2);

        fx.write(&dir.join("deploy/extra.md"), "e");
        let second = plan_with(&a, &fx, &edit).unwrap();
        assert_ne!(first.digest, second.digest);
    }
}
