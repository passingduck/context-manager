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

use crate::agents::{Agent, PlanCx, find_in};
use crate::env::ScanEnv;
use crate::model::*;
use crate::probe_fs::{EntryKind, ProbeErrorKind, ProbeFs};
use crate::scan::{MAX_READ_BYTES, Scanned};

/// A directory trashed whole keeps its files as history blobs up to this many bytes. Over it,
/// the OS trash is the only copy and the plan warns [`Warning::NotRevertible`].
pub const MAX_TREE_JOURNAL_BYTES: u64 = 16 * 1024 * 1024;

/// The walk of a directory trashed whole stops at these limits. A tree it could not read to the
/// end is not revertible (design section 4, step 4).
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
    /// An agent's planner: also the parked-hook directory and the items of a directory slot.
    Agent,
    /// A revert restores paths a recorded change touched. They were checked when recorded, and a
    /// trashed file is no longer in the snapshot. A read-only claim still refuses.
    Revert,
}

enum Disk {
    Absent,
    Text { text: String, rev: Revision },
    NotUtf8,
    Unreadable(ApiError),
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
    /// False when the walk skipped something: a level below its depth limit, entries over its
    /// count limit, an entry it could not read, a symlink, or a name the lister cannot represent.
    complete: bool,
}

/// `<dir>/<name>` when `path` lies below it, deeper than `<dir>/<name>` itself.
fn item_dir(dir: &AbsPath, path: &AbsPath) -> Option<AbsPath> {
    let rest = path.as_str().strip_prefix(dir.as_str())?;
    let rest = if dir.as_str() == "/" {
        rest
    } else {
        rest.strip_prefix('/')?
    };
    let (name, below) = rest.split_once('/')?;
    (!name.is_empty() && !below.is_empty()).then(|| dir.join(name))
}

/// Whether the agent lists `entry` as an item of the kind `slot` creates. Only skills, commands
/// and subagents are items; a directory of instructions is not.
fn is_listed_item(view: &AgentView, slot: &Slot, entry: &AbsPath) -> bool {
    let kind = match slot.new_kind {
        NewItemKind::Skill => ExtKind::Skill,
        NewItemKind::Command => ExtKind::Command,
        NewItemKind::Subagent => ExtKind::Subagent,
        NewItemKind::Instructions | NewItemKind::Rule | NewItemKind::Memory => return false,
    };
    view.extensions
        .iter()
        .any(|ext| ext.ext_kind == kind && ext.file == *entry)
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

fn invalid(field: &str, why: &'static str) -> ApiError {
    ApiError::InvalidEdit {
        field: field.to_owned(),
        why: RuleRef::new(why),
    }
}

fn invalid_ref(field: &str, why: &RuleRef) -> ApiError {
    ApiError::InvalidEdit {
        field: field.to_owned(),
        why: why.clone(),
    }
}

fn read_only(why: &ReadOnlyWhy) -> ApiError {
    ApiError::ReadOnly {
        why: ReadOnlyReason::File { why: why.clone() },
    }
}

impl<'a> Planner<'a> {
    pub fn plan(&self, edit: &Edit) -> Result<Preview, ApiError> {
        match edit {
            Edit::Save { path, base, text } => self.save(path, base, text),
            // An agent may own the trash of a file it keeps an index of, such as a memory topic.
            Edit::Trash { path, base } => match self.by_owner(path, edit) {
                Some(result) => result,
                None => self.trash(path, base),
            },
            Edit::Revert { change } => self.revert(change),
            Edit::NewItem { agent, .. }
            | Edit::AddMemory { agent, .. }
            | Edit::AddHook { agent, .. } => self.by_agent(agent, edit),
            Edit::UpdateHook { hook, .. }
            | Edit::RemoveHook { hook }
            | Edit::SetHookEnabled { hook, .. } => self.by_hook(hook, edit),
        }
    }

    /// An edit on one hook: the snapshot names the hook and the agent that lists it, and only
    /// that agent plans the edit. A hook the snapshot does not list is `NotFound`: the entry
    /// changed since the scan, so the id names nothing now.
    fn by_hook(&self, id: &HookId, edit: &Edit) -> Result<Preview, ApiError> {
        let (view, hook) = self.scanned.snapshot.hook(id).ok_or(ApiError::NotFound)?;
        match edit {
            // A switch may write elsewhere, such as Gemini's `hooksConfig.disabled`. A switch
            // that rewrites a read-only hook file fails the boundary in `finish`.
            Edit::SetHookEnabled { .. } => {
                if let Toggle::Unavailable { why } = &hook.toggle {
                    return Err(invalid_ref("enabled", why));
                }
            }
            // Updating or removing a hook rewrites the file that defines it, so that file must
            // be an editable file of the snapshot.
            _ => self.check_hook_file(hook)?,
        }
        let agent = find_in(self.agents, &view.agent)
            .ok_or_else(|| invalid("agent", "edit.unknown_agent"))?;
        let cx = PlanCx {
            hook: Some(hook),
            ..self.plan_cx()
        };
        match agent.plan(&cx, edit) {
            Some(result) => self.agent_preview(result),
            None => Err(invalid("edit", "edit.unsupported")),
        }
    }

    fn check_hook_file(&self, hook: &Hook) -> Result<(), ApiError> {
        let file = &hook.source.file;
        match self
            .scanned
            .snapshot
            .files
            .get(file)
            .map(|entry| &entry.policy)
        {
            Some(EditPolicy::Editable) => Ok(()),
            Some(EditPolicy::ReadOnly { why }) => Err(read_only(why)),
            None => Err(ApiError::OutsideBoundary { path: file.clone() }),
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
            Err(err) => return Disk::Unreadable(err.into()),
            Ok(stat) if stat.kind != EntryKind::File => {
                return Disk::Unreadable(invalid("path", "edit.not_regular_file"));
            }
            Ok(stat) if stat.len > MAX_READ_BYTES => {
                return Disk::Unreadable(invalid("path", "edit.file_too_large"));
            }
            Ok(_) => {}
        }
        match self.fs().read(path) {
            Err(err) if err.is_not_found() => Disk::Absent,
            Err(err) => Disk::Unreadable(err.into()),
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
            Disk::Unreadable(err) => Err(err),
            ok => Ok(ok),
        }
    }

    fn tree(&self, dir: &AbsPath) -> Tree {
        let mut files = Vec::new();
        let mut seen = 0usize;
        let mut complete = true;
        self.walk(dir, 0, &mut seen, &mut files, &mut complete);
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
            complete,
        }
    }

    fn walk(
        &self,
        dir: &AbsPath,
        depth: usize,
        seen: &mut usize,
        out: &mut Vec<(AbsPath, Vec<u8>)>,
        complete: &mut bool,
    ) {
        if depth > TREE_DEPTH {
            *complete = false;
            return;
        }
        let Ok(listing) = self.fs().listing(dir) else {
            *complete = false;
            return;
        };
        // A name that is not UTF-8 has no entry, so the history holds no copy of its file.
        if listing.not_utf8 > 0 {
            *complete = false;
        }
        for entry in listing.entries.iter() {
            *seen += 1;
            if *seen > TREE_ENTRIES {
                *complete = false;
                return;
            }
            let path = dir.join(&entry.name);
            // History cannot hold a link, and the files behind one are not inside the tree.
            if self.fs().stat(&path).is_ok_and(|stat| stat.is_symlink) {
                *complete = false;
                continue;
            }
            match entry.kind {
                EntryKind::Dir => self.walk(&path, depth + 1, seen, out, complete),
                EntryKind::File => match self.fs().read(&path) {
                    Ok(bytes) => out.push((path, bytes.as_ref().clone())),
                    Err(_) => *complete = false,
                },
                // A broken symlink, a socket or a pipe: history cannot hold it.
                EntryKind::Other => *complete = false,
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
                Place::NamedDir { dir, file } | Place::InSubdir { dir, file } => {
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

    /// The agents with a directory slot that holds `path` inside one of its items: `path` is
    /// below `<dir>/<name>/`, and `<dir>/<name>/<entry file>` is an item of the slot's kind that
    /// the snapshot lists, or a file that `ops` creates. Any other path below the slot's
    /// directory belongs to no item.
    fn slot_dir_owners(&self, path: &AbsPath, ops: &[FileOp]) -> Vec<AgentId> {
        let created = |entry: &AbsPath| {
            ops.iter().any(|op| {
                op.path == *entry
                    && op.expect == Base::Absent
                    && matches!(op.action, Action::Write { .. })
            })
        };
        let mut owners = Vec::new();
        for view in &self.scanned.snapshot.agents {
            let hit = view.slots.iter().any(|slot| {
                let Place::NamedDir { dir, file } = &slot.place else {
                    return false;
                };
                let Some(item) = item_dir(dir, path) else {
                    return false;
                };
                let entry = item.join(file);
                is_listed_item(view, slot, &entry) || created(&entry)
            });
            if hit {
                owners.push(view.agent.clone());
            }
        }
        owners
    }

    /// `ops` is the change the path belongs to: an agent's plan may write inside a directory
    /// item that the same change creates.
    fn check_path(&self, path: &AbsPath, mode: Mode, ops: &[FileOp]) -> Result<(), ApiError> {
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
                    || !self.slot_dir_owners(path, ops).is_empty()
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
            .chain(self.slot_dir_owners(path, &[]))
        {
            if !owners.contains(&id) {
                owners.push(id);
            }
        }
        owners
    }

    /// The agents that use `path` today: a load of it that is delivered, a hook defined in it,
    /// or a memory entry at it. A slot above the path makes an agent its owner for planning,
    /// not a user of it.
    fn users(&self, path: &AbsPath) -> Vec<AgentId> {
        self.scanned
            .snapshot
            .agents
            .iter()
            .filter(|view| {
                view.loads
                    .iter()
                    .any(|l| l.file == *path && l.delivered.is_some())
                    || view.hooks.iter().any(|h| h.source.file == *path)
                    || view
                        .memory
                        .iter()
                        .any(|store| store.entries.iter().any(|e| e.file == *path))
            })
            .map(|view| view.agent.clone())
            .collect()
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
                _ => self.check_path(&op.path, mode, &ops)?,
            }
            let landing = self.landing(&op.path)?;
            self.verify(op, &mut before)?;
            self.guard_op(op, &before)?;
            self.add_write_warnings(op, &before, &mut warnings);
            if let Some(target) = landing {
                warnings.push(Warning::ThroughSymlink { target });
            }
            // Design section 4, step 4: a file with other names is written in place.
            if let Action::Write { .. } = op.action
                && let Ok(stat) = self.fs().stat(&op.path)
                && stat.links > 1
            {
                let links = u32::try_from(stat.links).unwrap_or(u32::MAX);
                warnings.push(Warning::HardLinked { links });
            }
            let users = self.users(&op.path);
            if users.len() > 1 {
                warnings.push(Warning::SharedFile { agents: users });
            }
        }
        Ok(self.preview(summary, ops, before, warnings, conflicts))
    }

    /// The root a path lies below: the deepest of the home directory, the project root and `cwd`
    /// that holds it. A symlink at or above a root, such as macOS `/tmp` or a home directory that
    /// is a link, is where the user keeps the tree, not a link a write goes through.
    fn root_of(&self, path: &AbsPath) -> Option<AbsPath> {
        [
            Some(&self.env.home),
            self.scanned.snapshot.git_root.as_ref(),
            Some(&self.env.cwd),
        ]
        .into_iter()
        .flatten()
        .filter(|root| path.starts_with(root) && path != *root)
        .max_by_key(|root| root.as_str().len())
        .cloned()
    }

    /// Where an operation on `path` lands when the path's own entry, or a directory above it
    /// below its root, is a symlink (design section 4, `Warning::ThroughSymlink`). `None` when no
    /// link is on the way. A symlink to nothing on the way is an `InvalidEdit`: the tool does not
    /// create a file through a link. A path below no root is checked at its own entry only.
    fn landing(&self, path: &AbsPath) -> Result<Option<AbsPath>, ApiError> {
        let mut on_the_way = vec![path.clone()];
        if let Some(root) = self.root_of(path) {
            while let Some(dir) = on_the_way
                .last()
                .and_then(AbsPath::parent)
                .filter(|dir| *dir != root)
            {
                on_the_way.push(dir);
            }
        }
        let mut linked = false;
        for entry in &on_the_way {
            match self.fs().stat(entry) {
                Ok(stat) => linked |= stat.is_symlink,
                Err(err) if err.kind == ProbeErrorKind::DanglingLink => {
                    return Err(invalid("path", "edit.dangling_symlink"));
                }
                Err(_) => {}
            }
        }
        if !linked {
            return Ok(None);
        }
        // The deepest entry that exists, resolved, with the rest of the path below it.
        Ok(on_the_way.iter().find_map(|entry| {
            let real = self.fs().real_path(entry).ok()?;
            Some(real.join(path.as_str()[entry.as_str().len()..].trim_start_matches('/')))
        }))
    }

    /// Each owner of the file adds its warnings for the write, once.
    fn add_write_warnings(
        &self,
        op: &FileOp,
        before: &BTreeMap<AbsPath, Option<String>>,
        warnings: &mut Vec<Warning>,
    ) {
        let after = match &op.action {
            Action::Write { text } => Some(text.as_str()),
            Action::Trash => None,
            Action::TrashDir => return,
        };
        let before = before.get(&op.path).and_then(|b| b.as_deref());
        let cx = self.plan_cx();
        let owners = self.owners(&op.path);
        for agent in self.agents.iter().filter(|a| owners.contains(&a.id())) {
            for warning in agent.write_warnings(&cx, &op.path, before, after) {
                if !warnings.contains(&warning) {
                    warnings.push(warning);
                }
            }
        }
    }

    fn plan_cx(&self) -> PlanCx<'_> {
        PlanCx {
            env: self.env,
            fs: self.fs(),
            snapshot: &self.scanned.snapshot,
            parked_dir: self.parked_dir,
            hook: None,
        }
    }

    /// The entry file of `dir` when `dir` is one item of a directory slot, such as
    /// `skills/<name>`, that the snapshot lists as an item of the slot's kind.
    fn item_entry(&self, dir: &AbsPath) -> Option<AbsPath> {
        self.scanned.snapshot.agents.iter().find_map(|view| {
            view.slots.iter().find_map(|slot| match &slot.place {
                Place::NamedDir { dir: parent, file } if dir.parent().as_ref() == Some(parent) => {
                    let entry = dir.join(file);
                    is_listed_item(view, slot, &entry).then_some(entry)
                }
                _ => None,
            })
        })
    }

    /// Only a listed item of a directory slot goes to the trash whole.
    fn check_dir(&self, dir: &AbsPath, mode: Mode) -> Result<(), ApiError> {
        if self.item_entry(dir).is_some() || mode == Mode::Revert {
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
        self.check_path(path, Mode::Client, &[])?;
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

    /// The entry file of a listed item of a directory slot (`skills/<name>/SKILL.md`) trashes the
    /// whole directory, with the hash of its tree as the base of the operation. Any other file,
    /// such as a `GEMINI.md` in a subdirectory, is trashed alone.
    fn trash(&self, path: &AbsPath, base: &Revision) -> Result<Preview, ApiError> {
        self.check_path(path, Mode::Client, &[])?;
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
        let item = path
            .parent()
            .filter(|dir| self.item_entry(dir).as_ref() == Some(path));
        let Some(dir) = item else {
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
        };
        let tree = self.tree(&dir);
        let total: u64 = tree.files.iter().map(|(_, bytes)| bytes.len() as u64).sum();
        let all_text = tree
            .files
            .iter()
            .all(|(_, bytes)| std::str::from_utf8(bytes).is_ok());
        let mut warnings = Vec::new();
        let mut before = BTreeMap::new();
        if total > MAX_TREE_JOURNAL_BYTES || !all_text || !tree.complete {
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
            self.check_path(&file.path, Mode::Revert, &[])?;
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

    /// The first agent plan for an edit that also has a generic plan. `None`: the generic one
    /// applies.
    fn by_agent(&self, agent: &AgentId, edit: &Edit) -> Result<Preview, ApiError> {
        let agent =
            find_in(self.agents, agent).ok_or_else(|| invalid("agent", "edit.unknown_agent"))?;
        match agent.plan(&self.plan_cx(), edit) {
            Some(result) => self.agent_preview(result),
            None => Err(invalid("edit", "edit.unsupported")),
        }
    }

    /// An edit on a file an agent owns, such as a trash that also fixes an index. The first owner
    /// that plans it wins. `None`: the generic plan applies.
    fn by_owner(&self, path: &AbsPath, edit: &Edit) -> Option<Result<Preview, ApiError>> {
        let owners = self.owners(path);
        let cx = self.plan_cx();
        self.agents
            .iter()
            .filter(|a| owners.contains(&a.id()))
            .find_map(|a| a.plan(&cx, edit))
            .map(|result| self.agent_preview(result))
    }

    fn agent_preview(&self, result: Result<Preview, ApiError>) -> Result<Preview, ApiError> {
        let preview = result?;
        self.finish(
            Mode::Agent,
            Draft {
                summary: preview.change_set.summary,
                ops: preview.change_set.ops,
                before: preview.before,
                warnings: preview.warnings,
                conflicts: preview.conflicts,
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scan::scan_with;
    use crate::testing::{Fixture, TestAgent};

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
            ..TestAgent::default()
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
    fn a_hook_the_snapshot_does_not_list_is_not_found_for_every_hook_edit() {
        let fx = Fixture::new();
        let hook = HookId("h".into());
        let draft = HookDraft {
            event: "Stop".into(),
            matcher: None,
            handler: serde_json::json!({"type": "command", "command": "x"}),
        };
        for edit in [
            Edit::RemoveHook { hook: hook.clone() },
            Edit::UpdateHook {
                hook: hook.clone(),
                draft,
            },
            Edit::SetHookEnabled {
                hook,
                enabled: false,
            },
        ] {
            let err = plan_with(&agent(&[]), &fx, &edit).unwrap_err();
            assert_eq!(err, ApiError::NotFound, "{edit:?}");
        }
    }

    /// An agent whose `NewItem` plan writes `writes`, with a skill slot over `skills/`.
    fn writer(fx: &Fixture, writes: &[&str], skills: &[&str]) -> TestAgent {
        let dir = fx.project_path(".claude/skills");
        TestAgent {
            slot: Some(Place::NamedDir {
                dir: dir.clone(),
                file: "SKILL.md".into(),
            }),
            writes: writes
                .iter()
                .map(|rel| (dir.join(rel), "x".to_owned()))
                .collect(),
            skills: skills.iter().map(|rel| dir.join(rel)).collect(),
            ..TestAgent::default()
        }
    }

    fn new_item() -> Edit {
        Edit::NewItem {
            agent: AgentId::parse("claude").unwrap(),
            slot: SlotId("s".into()),
            name: None,
        }
    }

    #[test]
    fn an_agent_plan_may_not_write_below_a_directory_slot_outside_its_items() {
        let fx = Fixture::new();
        fx.write(
            &fx.project_path(".claude/skills/foo/hooks/hooks.json"),
            "{}",
        );
        for rel in ["foo/hooks/hooks.json", "loose.md", "foo/SKILL.md/x"] {
            let path = fx.project_path(".claude/skills").join(rel);
            assert_eq!(
                plan_with(&writer(&fx, &[rel], &[]), &fx, &new_item()).unwrap_err(),
                ApiError::OutsideBoundary { path },
                "{rel}"
            );
        }
    }

    #[test]
    fn an_agent_plan_may_write_inside_a_listed_skill_or_one_the_change_creates() {
        let fx = Fixture::new();
        fx.write(&fx.project_path(".claude/skills/known/SKILL.md"), "skill");
        let listed = writer(&fx, &["known/refs/a.md"], &["known/SKILL.md"]);
        assert!(plan_with(&listed, &fx, &new_item()).is_ok());
        let created = writer(&fx, &["new/SKILL.md", "new/refs/a.md"], &[]);
        assert_eq!(
            plan_with(&created, &fx, &new_item())
                .unwrap()
                .change_set
                .ops
                .len(),
            2
        );
    }

    /// A file in a subdirectory is no item: a change that creates `src/GEMINI.md` may write that
    /// file and nothing else in `src/`.
    #[test]
    fn an_agent_plan_that_creates_a_subdirectory_file_may_not_write_beside_it() {
        let fx = Fixture::new();
        let src = fx.project_path("src");
        let agent = TestAgent {
            slot: Some(Place::InSubdir {
                dir: fx.project(),
                file: "GEMINI.md".into(),
            }),
            writes: vec![
                (src.join("GEMINI.md"), "context".to_owned()),
                (src.join("main.rs"), "fn main() {}".to_owned()),
            ],
            ..TestAgent::default()
        };
        assert_eq!(
            plan_with(&agent, &fx, &new_item()).unwrap_err(),
            ApiError::OutsideBoundary {
                path: src.join("main.rs")
            }
        );
    }

    /// Design section 4, step 4: a tree the plan could not capture completely is not revertible.
    #[test]
    fn a_skill_with_a_file_deeper_than_the_walk_is_not_revertible() {
        let fx = Fixture::new();
        let entry = fx.project_path(".claude/skills/deep/SKILL.md");
        fx.write(&entry, "skill");
        let trash = Edit::Trash {
            path: entry.clone(),
            base: Revision::of(b"skill"),
        };
        let a = writer(&fx, &[], &["deep/SKILL.md"]);
        let levels = |n: usize| (1..=n).map(|i| format!("d{i}/")).collect::<String>();

        fx.write(
            &fx.project_path(&format!(".claude/skills/deep/{}f.txt", levels(16))),
            "16",
        );
        let preview = plan_with(&a, &fx, &trash).unwrap();
        assert_eq!(preview.change_set.ops[0].action, Action::TrashDir);
        assert!(!preview.warnings.contains(&Warning::NotRevertible));
        assert_eq!(preview.before.len(), 2);

        let deep = fx.project_path(&format!(".claude/skills/deep/{}f.txt", levels(17)));
        fx.write(&deep, "17");
        let preview = plan_with(&a, &fx, &trash).unwrap();
        assert_eq!(preview.change_set.ops[0].action, Action::TrashDir);
        assert!(preview.warnings.contains(&Warning::NotRevertible));
        assert!(!preview.before.contains_key(&deep));
    }

    #[test]
    fn an_entry_file_name_outside_a_listed_item_trashes_only_that_file() {
        let fx = Fixture::new();
        let entry = fx.project_path(".claude/skills/src/SKILL.md");
        fx.write(&entry, "not a skill the agent lists");
        fx.write(
            &fx.project_path(".claude/skills/src/main.rs"),
            "fn main() {}",
        );
        let a = writer(&fx, &[], &[]);
        let preview = plan_with(
            &a,
            &fx,
            &Edit::Trash {
                path: entry.clone(),
                base: Revision::of(b"not a skill the agent lists"),
            },
        )
        .unwrap();
        assert_eq!(
            preview
                .change_set
                .ops
                .iter()
                .map(|op| (op.path.clone(), op.action.clone()))
                .collect::<Vec<_>>(),
            vec![(entry, Action::Trash)]
        );
    }

    #[test]
    fn trashing_an_entry_file_trashes_its_directory_and_its_tree_hash_moves_with_the_siblings() {
        let fx = Fixture::new();
        let dir = fx.project_path(".claude/skills");
        let entry = dir.join("deploy/SKILL.md");
        fx.write(&entry, "skill");
        fx.write(&dir.join("deploy/notes.md"), "n");
        let a = writer(&fx, &[], &["deploy/SKILL.md"]);
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
