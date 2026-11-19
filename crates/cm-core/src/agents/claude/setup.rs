//! What a Claude Code scan reads before it looks at any instruction file: the homes, the
//! settings files in precedence order (spec 5.2, 5.3), the keys of `~/.claude.json` (spec 5.2),
//! the project and its worktree (spec 1.1, 2.3) and the switches that gate loading.

use std::collections::BTreeMap;

use serde_json::Value;

use crate::agents::ScanCx;
use crate::model::*;
use crate::probe_fs::EntryKind;

/// Test-only override of the managed directory. It is not in `MOVING_VARS`, so a real process
/// never sees it. Spec 5.1: this build of Claude Code has no override.
pub(super) const TEST_MANAGED_DIR_VAR: &str = "CONTEXT_MANAGER_MANAGED_DIR";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SettingsKind {
    Remote,
    Managed,
    Local,
    Project,
    User,
}

/// One settings file, present or not. Spec 5.2.
#[derive(Debug, Clone)]
pub(super) struct SettingsFile {
    pub path: AbsPath,
    pub kind: SettingsKind,
    pub scope: Scope,
    pub exists: bool,
    /// The parsed object. `None` when the file is absent, unreadable or not a JSON object.
    pub value: Option<Value>,
    pub error: Option<String>,
}

impl SettingsFile {
    pub fn policy(&self) -> EditPolicy {
        match self.kind {
            SettingsKind::Managed | SettingsKind::Remote => EditPolicy::ReadOnly {
                why: ReadOnlyWhy::Managed,
            },
            _ => EditPolicy::Editable,
        }
    }

    pub fn get(&self, key: &str) -> Option<&Value> {
        self.value.as_ref()?.get(key)
    }
}

/// The git facts spec 1.1 and 2.3 need.
#[derive(Debug, Clone)]
pub(super) struct Project {
    /// The nearest ancestor of cwd (cwd included) that holds `.git`.
    pub git_root: Option<AbsPath>,
    /// The main repository root. Differs from `git_root` inside a linked worktree.
    pub main_root: Option<AbsPath>,
}

pub(super) struct Setup {
    pub home: AbsPath,
    pub cwd: AbsPath,
    pub os: Os,
    pub config: AbsPath,
    pub config_from_env: Option<&'static str>,
    pub managed: AbsPath,
    pub plugins: AbsPath,
    /// Highest precedence first: remote cache, managed files (last drop-in first), local,
    /// project, user. Spec 5.3.
    pub settings: Vec<SettingsFile>,
    pub global_path: AbsPath,
    /// `~/.claude.json`, keys only. `None` when absent or not a JSON object.
    pub global: Option<Value>,
    pub project: Project,
    /// `projects[<dir>]` of the nearest recorded directory at or above cwd.
    pub project_record: Option<(String, Value)>,
    pub diagnostics: Vec<Diagnostic>,
}

impl Setup {
    pub fn load(cx: &ScanCx) -> Setup {
        let env = cx.env;
        let home = env.home.clone();
        let (config, config_from_env) = match env
            .var("CLAUDE_CONFIG_DIR")
            .and_then(|raw| AbsPath::parse(raw).ok())
        {
            Some(set) => (set, Some("CLAUDE_CONFIG_DIR")),
            None => (home.join(".claude"), None),
        };
        let managed = match env
            .var(TEST_MANAGED_DIR_VAR)
            .and_then(|r| AbsPath::parse(r).ok())
        {
            Some(dir) => dir,
            None => AbsPath::parse(match env.os {
                Os::Macos => "/Library/Application Support/ClaudeCode",
                Os::Windows => "/C:/Program Files/ClaudeCode",
                _ => "/etc/claude-code",
            })
            .expect("constant path"),
        };
        let plugins = match env
            .var("CLAUDE_CODE_PLUGIN_CACHE_DIR")
            .and_then(|raw| AbsPath::parse(raw).ok())
        {
            Some(dir) => dir,
            None => config.join("plugins"),
        };
        let global_path = if config_from_env.is_some() {
            config.join(".claude.json")
        } else {
            home.join(".claude.json")
        };

        let mut diagnostics = Vec::new();
        let project = find_project(cx);
        let mut settings = Vec::new();
        let mut read = |path: AbsPath, kind: SettingsKind, scope: Scope| {
            let file = read_settings(cx, path, kind, scope);
            if let Some(detail) = &file.error {
                diagnostics.push(Diagnostic {
                    severity: Severity::Error,
                    agent: Some(AgentId::builtin("claude")),
                    at: Subject::File {
                        path: file.path.clone(),
                        line: None,
                    },
                    problem: Problem::ParseError {
                        format: Format::Json,
                        detail: detail.clone(),
                        fatal_for_agent: kind == SettingsKind::Managed,
                    },
                    fix: None,
                });
            }
            file
        };

        settings.push(read(
            config.join("remote-settings.json"),
            SettingsKind::Remote,
            Scope::Managed,
        ));
        let mut managed_files = vec![managed.join("managed-settings.json")];
        if let Ok(entries) = cx.fs.list(&managed.join("managed-settings.d")) {
            for entry in entries.iter() {
                if entry.kind != EntryKind::Dir
                    && entry.name.ends_with(".json")
                    && !entry.name.starts_with('.')
                {
                    managed_files.push(managed.join("managed-settings.d").join_name(&entry.name));
                }
            }
        }
        for path in managed_files.into_iter().rev() {
            settings.push(read(path, SettingsKind::Managed, Scope::Managed));
        }
        // Spec 5.2: the local file sits at the main checkout root, with the start directory's
        // copy as an older location. Outside git it sits beside the start directory.
        let local_root = project.main_root.clone().unwrap_or_else(|| env.cwd.clone());
        let local_root = if local_root == home {
            env.cwd.clone()
        } else {
            local_root
        };
        settings.push(read(
            local_root.join(".claude/settings.local.json"),
            SettingsKind::Local,
            Scope::Local,
        ));
        let cwd_local = env.cwd.join(".claude/settings.local.json");
        if cwd_local != settings.last().expect("just pushed").path {
            settings.push(read(cwd_local, SettingsKind::Local, Scope::Local));
        }
        settings.push(read(
            env.cwd.join(".claude/settings.json"),
            SettingsKind::Project,
            Scope::Project {
                dir: env.cwd.clone(),
            },
        ));
        settings.push(read(
            config.join("settings.json"),
            SettingsKind::User,
            Scope::User,
        ));

        let global = read_json_object(cx, &global_path).ok().flatten();
        let project_record = global.as_ref().and_then(|value| {
            let projects = value.get("projects")?.as_object()?;
            let mut dir = Some(env.cwd.clone());
            while let Some(current) = dir {
                if let Some(record) = projects.get(current.as_str()) {
                    return Some((current.as_str().to_owned(), record.clone()));
                }
                dir = current.parent();
            }
            None
        });

        Setup {
            home,
            cwd: env.cwd.clone(),
            os: env.os,
            config,
            config_from_env,
            managed,
            plugins,
            settings,
            global_path,
            global,
            project,
            project_record,
            diagnostics,
        }
    }

    /// The first value of `key` in precedence order, with the file that holds it.
    pub fn first(&self, key: &str) -> Option<(&Value, &SettingsFile)> {
        self.settings
            .iter()
            .find_map(|file| file.get(key).map(|value| (value, file)))
    }

    /// Like [`Setup::first`], restricted to files of these kinds.
    pub fn first_in(&self, key: &str, kinds: &[SettingsKind]) -> Option<(&Value, &SettingsFile)> {
        self.settings
            .iter()
            .filter(|file| kinds.contains(&file.kind))
            .find_map(|file| file.get(key).map(|value| (value, file)))
    }

    /// The union of a string-array key over every settings file, first occurrence first.
    pub fn union_strings(&self, key: &str) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for file in &self.settings {
            if let Some(Value::Array(items)) = file.get(key) {
                for item in items.iter().filter_map(Value::as_str) {
                    if !out.iter().any(|seen| seen == item) {
                        out.push(item.to_owned());
                    }
                }
            }
        }
        out
    }

    /// A process variable, else the `env` block of the settings (spec 1.7: the env var may also
    /// come from a settings `env` block).
    pub fn var(&self, cx: &ScanCx, name: &str) -> Option<String> {
        if let Some(value) = cx.env.var(name) {
            return Some(value.to_owned());
        }
        self.settings.iter().find_map(|file| {
            let value = file.get("env")?.get(name)?;
            Some(
                value
                    .as_str()
                    .map_or_else(|| value.to_string(), str::to_owned),
            )
        })
    }

    /// A boolean switch from the environment. `1`, `true`, `yes`, `on` are true.
    pub fn flag(&self, cx: &ScanCx, name: &str) -> Option<bool> {
        let raw = self.var(cx, name)?;
        Some(matches!(
            raw.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        ))
    }

    /// The recorded folder trust (`hasTrustDialogAccepted`) for cwd or its nearest recorded
    /// ancestor, with the file it came from.
    pub fn trust(&self) -> Trust {
        let accepted = self.global.as_ref().and_then(|value| {
            let projects = value.get("projects")?.as_object()?;
            let mut dir = Some(self.cwd.clone());
            let mut any = None;
            while let Some(current) = dir {
                if let Some(flag) = projects
                    .get(current.as_str())
                    .and_then(|r| r.get("hasTrustDialogAccepted"))
                    .and_then(Value::as_bool)
                {
                    if flag {
                        return Some(true);
                    }
                    any = Some(false);
                }
                dir = current.parent();
            }
            any
        });
        match accepted {
            Some(true) => Trust {
                level: TrustLevel::Trusted,
                recorded_in: Some(self.global_path.clone()),
            },
            Some(false) => Trust {
                level: TrustLevel::Untrusted,
                recorded_in: Some(self.global_path.clone()),
            },
            None => Trust {
                level: TrustLevel::Unknown,
                recorded_in: None,
            },
        }
    }

    pub fn enabled_plugins(&self) -> BTreeMap<String, bool> {
        let mut out = BTreeMap::new();
        // Lowest precedence first, so a higher file overwrites.
        for file in self.settings.iter().rev() {
            if let Some(Value::Object(map)) = file.get("enabledPlugins") {
                for (id, on) in map {
                    if let Some(on) = on.as_bool() {
                        out.insert(id.clone(), on);
                    }
                }
            }
        }
        out
    }
}

trait JoinName {
    fn join_name(&self, name: &str) -> AbsPath;
}

impl JoinName for AbsPath {
    fn join_name(&self, name: &str) -> AbsPath {
        AbsPath::parse(&format!("{}/{}", self.as_str().trim_end_matches('/'), name))
            .unwrap_or_else(|_| self.clone())
    }
}

fn read_json_object(cx: &ScanCx, path: &AbsPath) -> Result<Option<Value>, String> {
    let bytes = match cx.fs.read(path) {
        Ok(bytes) => bytes,
        Err(err) if err.is_not_found() => return Ok(None),
        Err(err) => return Err(err.detail),
    };
    match serde_json::from_slice::<Value>(&bytes) {
        Ok(value @ Value::Object(_)) => Ok(Some(value)),
        Ok(_) => Err("not a JSON object".to_owned()),
        Err(err) => Err(err.to_string()),
    }
}

fn read_settings(cx: &ScanCx, path: AbsPath, kind: SettingsKind, scope: Scope) -> SettingsFile {
    let (value, error, exists) = match read_json_object(cx, &path) {
        Ok(Some(value)) => (Some(value), None, true),
        Ok(None) => (None, None, false),
        Err(detail) => (None, Some(detail), true),
    };
    SettingsFile {
        path,
        kind,
        scope,
        exists,
        value,
        error,
    }
}

/// Spec 1.1 and 2.3: the git root of cwd and the main repository root.
fn find_project(cx: &ScanCx) -> Project {
    let mut dir = Some(cx.env.cwd.clone());
    while let Some(current) = dir {
        let dot_git = current.join(".git");
        if let Ok(stat) = cx.fs.stat(&dot_git) {
            let main_root = if stat.kind == EntryKind::File {
                linked_main_root(cx, &current, &dot_git)
            } else {
                None
            };
            return Project {
                main_root: Some(main_root.unwrap_or_else(|| current.clone())),
                git_root: Some(current),
            };
        }
        dir = current.parent();
    }
    Project {
        git_root: None,
        main_root: None,
    }
}

/// `.git` file of a linked worktree: `gitdir: <main>/.git/worktrees/<name>`. The common
/// directory is named by `commondir`, relative to the gitdir. The main root is its parent.
fn linked_main_root(cx: &ScanCx, root: &AbsPath, dot_git: &AbsPath) -> Option<AbsPath> {
    let text = String::from_utf8(cx.fs.read(dot_git).ok()?.as_ref().clone()).ok()?;
    let raw = text.lines().find_map(|line| line.strip_prefix("gitdir:"))?;
    let gitdir = root.join(raw.trim());
    let common_raw = cx.fs.read(&gitdir.join("commondir")).ok()?;
    let common_raw = String::from_utf8(common_raw.as_ref().clone()).ok()?;
    let common = gitdir.join(common_raw.trim());
    if common.file_name() == Some(".git") {
        common.parent()
    } else {
        None
    }
}
