//! A temporary home and project for tests. Tests never read a real agent home.

use std::collections::BTreeMap;
use std::path::Path;

use crate::agents::{Agent, AgentScan, FileClaim, PlanCx, ScanCx, home_only_scan, home_with_env};
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

/// Claims the files it is given and refuses any text that contains `refuse`.
#[derive(Default)]
pub struct TestAgent {
    pub files: Vec<AbsPath>,
    pub readonly: Vec<AbsPath>,
    pub slot: Option<Place>,
    pub refuse: Option<&'static str>,
    /// `Edit::NewItem` plans a write of each of these, so a change can span several files.
    pub writes: Vec<(AbsPath, String)>,
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
