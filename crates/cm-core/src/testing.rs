//! A temporary home and project for tests. Tests never read a real agent home.

use std::path::Path;

use crate::env::ScanEnv;
use crate::model::AbsPath;

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
