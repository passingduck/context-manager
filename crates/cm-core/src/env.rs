//! The process environment, read once at the boundary. Adapters see only a [`ScanEnv`].

use std::collections::BTreeMap;

use crate::model::Os;
pub use crate::model::{AbsPath, PathError};

/// The environment variables that move things. `ScanEnv::from_process` copies exactly these.
pub const MOVING_VARS: &[&str] = &[
    "CLAUDE_CONFIG_DIR",
    "CODEX_HOME",
    "GEMINI_CLI_HOME",
    "PATH",
    "GEMINI_CLI_SYSTEM_SETTINGS_PATH",
    "GEMINI_CLI_SYSTEM_DEFAULTS_PATH",
    "GEMINI_CLI_TRUSTED_FOLDERS_PATH",
    "GEMINI_CLI_TRUST_WORKSPACE",
    "GEMINI_RESTRICTED_MODE",
    "GEMINI_SYSTEM_MD",
    "GEMINI_WRITE_SYSTEM_MD",
    "GEMINI_CLI_IDE_WORKSPACE_PATH",
    "CODEX_SQLITE_HOME",
];

#[derive(Debug, Clone, PartialEq)]
pub struct ScanEnv {
    pub cwd: AbsPath,
    pub home: AbsPath,
    pub os: Os,
    vars: BTreeMap<String, String>,
}

impl ScanEnv {
    /// The only code that reads the process environment.
    pub fn from_process(cwd: AbsPath) -> Result<ScanEnv, PathError> {
        let home_raw = std::env::var("HOME").unwrap_or_else(|_| "/".to_owned());
        let home = AbsPath::parse(&home_raw)?;
        let vars = MOVING_VARS
            .iter()
            .filter_map(|name| {
                std::env::var(name)
                    .ok()
                    .map(|value| ((*name).to_owned(), value))
            })
            .collect();
        Ok(ScanEnv {
            cwd,
            home,
            os: current_os(),
            vars,
        })
    }

    /// A test environment: no variable is set, so nothing moves and no program is on `PATH`.
    pub fn for_fixture(home: AbsPath, cwd: AbsPath) -> ScanEnv {
        ScanEnv {
            cwd,
            home,
            os: current_os(),
            vars: BTreeMap::new(),
        }
    }

    pub fn var(&self, name: &str) -> Option<&str> {
        self.vars
            .get(name)
            .map(String::as_str)
            .filter(|value| !value.is_empty())
    }

    pub fn with_var(mut self, name: &str, value: &str) -> ScanEnv {
        self.vars.insert(name.to_owned(), value.to_owned());
        self
    }

    pub fn with_cwd(&self, cwd: AbsPath) -> ScanEnv {
        ScanEnv {
            cwd,
            ..self.clone()
        }
    }
}

fn current_os() -> Os {
    match std::env::consts::OS {
        "macos" => Os::Macos,
        "linux" => Os::Linux,
        "windows" => Os::Windows,
        _ => Os::Other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_normalizes_and_rejects_relative_paths() {
        assert_eq!(AbsPath::parse("/a/b/../c//d/.").unwrap().as_str(), "/a/c/d");
        assert_eq!(AbsPath::parse("/../x").unwrap().as_str(), "/x");
        assert_eq!(AbsPath::parse("/").unwrap().as_str(), "/");
        assert!(AbsPath::parse("a/b").is_err());
        assert!(AbsPath::parse("").is_err());
    }

    #[test]
    fn join_and_parent() {
        let base = AbsPath::parse("/a/b").unwrap();
        assert_eq!(base.join("../c.md").as_str(), "/a/c.md");
        assert_eq!(base.parent().unwrap().as_str(), "/a");
        assert_eq!(
            AbsPath::parse("/a").unwrap().parent().unwrap().as_str(),
            "/"
        );
        assert!(AbsPath::root().parent().is_none());
        assert_eq!(base.file_name(), Some("b"));
    }

    #[test]
    fn an_empty_variable_counts_as_unset() {
        let home = AbsPath::parse("/h").unwrap();
        let env = ScanEnv::for_fixture(home.clone(), home).with_var("CODEX_HOME", "");
        assert_eq!(env.var("CODEX_HOME"), None);
    }
}
