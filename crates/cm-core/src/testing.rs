//! A temporary home and project for tests. Tests never read a real agent home.

use std::collections::BTreeMap;
use std::path::Path;

use crate::agents::{
    Agent, AgentScan, FileClaim, HomeChoice, PlanCx, ScanCx, empty_view, home_dir, home_with_env,
    presence,
};
use crate::env::ScanEnv;
use crate::model::*;

pub struct Fixture {
    _dir: tempfile::TempDir,
    root: AbsPath,
}

impl Default for Fixture {
    fn default() -> Self {
        Fixture::new()
    }
}

impl Fixture {
    /// Creates `<tmp>/home` and `<tmp>/project`. The root is canonical, so macOS `/var` and
    /// `/private/var` do not both appear.
    pub fn new() -> Fixture {
        let dir = tempfile::tempdir().expect("create fixture directory");
        let real = dir
            .path()
            .canonicalize()
            .expect("canonicalize fixture directory");
        let root =
            AbsPath::parse(real.to_str().expect("UTF-8 temp path")).expect("absolute temp path");
        let fixture = Fixture { _dir: dir, root };
        fixture.mkdir(&fixture.home());
        fixture.mkdir(&fixture.project());
        fixture
    }

    pub fn root(&self) -> AbsPath {
        self.root.clone()
    }

    pub fn home(&self) -> AbsPath {
        self.root.join("home")
    }

    pub fn project(&self) -> AbsPath {
        self.root.join("project")
    }

    pub fn home_path(&self, rel: &str) -> AbsPath {
        self.home().join(rel)
    }

    pub fn project_path(&self, rel: &str) -> AbsPath {
        self.project().join(rel)
    }

    pub fn mkdir(&self, path: &AbsPath) {
        std::fs::create_dir_all(path.as_path()).expect("create directory");
    }

    /// Writes a file, creating parent directories.
    pub fn write(&self, path: &AbsPath, text: &str) {
        if let Some(parent) = path.parent() {
            self.mkdir(&parent);
        }
        std::fs::write(path.as_path(), text).expect("write file");
    }

    /// Creates `link` pointing at `target`, creating parent directories.
    pub fn symlink(&self, link: &AbsPath, target: &AbsPath) {
        if let Some(parent) = link.parent() {
            self.mkdir(&parent);
        }
        std::os::unix::fs::symlink(Path::new(target.as_str()), link.as_path())
            .expect("create symlink");
    }

    /// An environment with the fixture's home and project and no variable set.
    pub fn env(&self) -> ScanEnv {
        ScanEnv::for_fixture(self.home(), self.project())
    }
}

pub(crate) static NO_HOOKS: HookSchema = HookSchema {
    events: Vec::new(),
    handler_kinds: Vec::new(),
};

/// Resolves the home of an adapter whose view is otherwise empty.
pub(crate) fn home_only_scan(
    cx: &ScanCx,
    agent: AgentId,
    bin: &str,
    role: &'static str,
    choice: HomeChoice,
) -> AgentScan {
    let home = home_dir(cx, role, &choice);
    let presence = presence(cx, bin, home.exists);
    AgentScan {
        view: empty_view(agent, presence, vec![home]),
        files: Vec::new(),
        diagnostics: Vec::new(),
    }
}

/// The `PATH` for a real agent CLI that a test starts under a fixture home.
///
/// Claude Code asks the macOS `security` tool for its credentials and finds that tool on `PATH`.
/// Under a fixture home the tool has no keychain, and macOS shows a dialog on every start. This
/// writes a `security` into `dir` that answers every question with "not found" and puts `dir`
/// first, so the CLI reaches no keychain at all, the operator's included.
pub fn cli_path(dir: &Path, rest: &str) -> String {
    std::fs::create_dir_all(dir).expect("create the stub directory");
    let stub = dir.join("security");
    // 44 is what `security` exits with for an item that is not in the keychain. With `-i` the
    // CLI sends its commands on standard input, so read them before answering.
    std::fs::write(
        &stub,
        "#!/bin/sh\n[ \"$1\" = \"-i\" ] && cat >/dev/null\nexit 44\n",
    )
    .expect("write the security stub");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755))
            .expect("mark the security stub executable");
    }
    format!("{}:{rest}", dir.display())
}

/// Claims the files it is given and refuses any text that contains `refuse`.
#[derive(Default)]
pub struct TestAgent {
    pub files: Vec<AbsPath>,
    pub readonly: Vec<AbsPath>,
    pub slot: Option<Place>,
    pub refuse: Option<&'static str>,
    /// `Edit::NewItem` plans a write of each of these, so a change can span several files.
    pub writes: Vec<(AbsPath, String)>,
    /// Entry files it lists as skills, such as `skills/<name>/SKILL.md`.
    pub skills: Vec<AbsPath>,
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
        &NO_HOOKS
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
        for file in &self.skills {
            scan.view.extensions.push(Extension {
                id: ExtId(file.to_string()),
                ext_kind: ExtKind::Skill,
                name: file
                    .parent()
                    .and_then(|dir| dir.file_name().map(str::to_owned))
                    .unwrap_or_default(),
                description: None,
                file: file.clone(),
                scope: Scope::User,
                listing: None,
                body: LoadId(0),
                status: ExtStatus::Active,
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
    fn plan(&self, cx: &PlanCx, edit: &Edit) -> Option<Result<Preview, ApiError>> {
        if self.writes.is_empty() || !matches!(edit, Edit::NewItem { .. }) {
            return None;
        }
        let ops = self
            .writes
            .iter()
            .map(|(path, text)| FileOp {
                path: path.clone(),
                expect: match cx.fs.read(path) {
                    Ok(bytes) => Base::Rev {
                        rev: Revision::of(&bytes),
                    },
                    Err(_) => Base::Absent,
                },
                action: Action::Write { text: text.clone() },
            })
            .collect();
        Some(Ok(Preview {
            digest: String::new(),
            change_set: ChangeSet {
                summary: ChangeSummary {
                    edit: ChangeEdit::NewItem,
                    agent: Some(self.id()),
                    targets: self.writes.iter().map(|(path, _)| path.clone()).collect(),
                },
                ops,
            },
            before: BTreeMap::new(),
            warnings: Vec::new(),
            conflicts: Vec::new(),
        }))
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

#[cfg(all(test, unix))]
mod tests {
    use std::io::Write;
    use std::process::{Command, Stdio};

    use super::cli_path;

    /// The real tool exits 0 for `help`. A process that gets 44 reached the stub.
    #[test]
    fn a_process_started_with_cli_path_reaches_the_stub_and_not_the_real_tool() {
        let dir = tempfile::tempdir().expect("create a temp directory");
        let path = cli_path(&dir.path().join("bin"), "/usr/bin:/bin");

        let with_arguments = Command::new("security")
            .arg("help")
            .env_clear()
            .env("PATH", &path)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .expect("start security");
        assert_eq!(with_arguments.code(), Some(44));

        let mut interactive = Command::new("security")
            .arg("-i")
            .env_clear()
            .env("PATH", &path)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("start security -i");
        interactive
            .stdin
            .take()
            .expect("a pipe to the stub")
            .write_all(b"help\n")
            .expect("send a command");
        assert_eq!(interactive.wait().expect("wait").code(), Some(44));
    }
}
