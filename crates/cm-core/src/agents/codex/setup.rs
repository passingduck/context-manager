//! Homes, config layers, project roots and trust (docs/agents/codex.md sections 1.2, 6).
//!
//! One [`Setup`] is built per scan. Every other module of the adapter reads it.

use std::sync::Arc;

use toml::{Table, Value};

use crate::agents::ScanCx;
use crate::model::*;
use crate::probe_fs::EntryKind;

/// The system config directory. Absent on most machines (spec 6.2).
pub(super) const SYSTEM_DIR: &str = "/etc/codex";
/// Spec 1.3: `DEFAULT_PROJECT_DOC_MAX_BYTES`.
pub(super) const DEFAULT_PROJECT_DOC_MAX_BYTES: u64 = 32 * 1024;
/// Spec 1.2: `DEFAULT_PROJECT_ROOT_MARKERS`.
const DEFAULT_ROOT_MARKERS: [&str; 1] = [".git"];
/// Spec 6.2: `PROJECT_LOCAL_CONFIG_DENYLIST`.
const PROJECT_DENYLIST: [&str; 12] = [
    "openai_base_url",
    "chatgpt_base_url",
    "apps_mcp_product_sku",
    "responses_api_metadata",
    "model_provider",
    "model_providers",
    "notify",
    "profile",
    "profiles",
    "experimental_realtime_webrtc_call_base_url",
    "experimental_realtime_ws_base_url",
    "otel",
];

#[derive(Debug, Clone, PartialEq)]
pub(super) enum LayerKind {
    System,
    User,
    Project { dir: AbsPath },
    LegacyManaged,
}

/// One config layer: a `config.toml` and the folder that holds its sibling files.
#[derive(Debug, Clone)]
pub(super) struct Layer {
    pub kind: LayerKind,
    /// `/etc/codex`, `$CODEX_HOME` or `<dir>/.codex`.
    pub folder: AbsPath,
    pub config_path: AbsPath,
    pub config: Option<Table>,
    /// Disabled project layers (spec 6.3): parsed, then ignored.
    pub enabled: bool,
    /// Where hook declarations are read from. Differs from `folder` in a linked worktree.
    pub hooks_folder: AbsPath,
}

impl Layer {
    pub fn scope(&self) -> Scope {
        match &self.kind {
            LayerKind::System | LayerKind::LegacyManaged => Scope::Managed,
            LayerKind::User => Scope::User,
            LayerKind::Project { dir } => Scope::Project { dir: dir.clone() },
        }
    }

    pub fn is_managed(&self) -> bool {
        matches!(self.kind, LayerKind::System | LayerKind::LegacyManaged)
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Features {
    pub hooks: bool,
    pub memories: bool,
    pub plugins: bool,
}

pub(super) struct Setup {
    pub cwd: AbsPath,
    pub home: AbsPath,
    pub codex_home: AbsPath,
    pub home_from_env: Option<&'static str>,
    pub user_config: AbsPath,
    /// Low to high precedence: system, user, project layers (root first), legacy managed.
    pub layers: Vec<Layer>,
    /// Enabled layers merged, higher over lower.
    pub merged: Table,
    /// The root `AGENTS.md` discovery walks down from. `cwd` when there is no marker.
    pub md_root: AbsPath,
    pub md_root_found: bool,
    pub markers: Vec<String>,
    /// Spec 1.5: only an explicit `untrusted` entry for cwd or the repo root skips `AGENTS.md`.
    pub md_untrusted: bool,
    pub trust: Trust,
    pub features: Features,
    pub project_doc_max_bytes: u64,
    pub fallback_names: Vec<String>,
    pub diagnostics: Vec<Diagnostic>,
}

impl Setup {
    pub fn load(cx: &ScanCx) -> Setup {
        let choice = crate::agents::home_with_env(cx, "CODEX_HOME", ".codex", false);
        let codex_home = choice.path;
        let user_config = codex_home.join("config.toml");
        let mut diagnostics = Vec::new();

        let system_dir = AbsPath::parse(SYSTEM_DIR).expect("absolute");
        let mut layers = Vec::new();
        layers.push(read_layer(
            cx,
            LayerKind::System,
            system_dir.clone(),
            system_dir.join("config.toml"),
            true,
            &mut diagnostics,
        ));
        layers.push(read_layer(
            cx,
            LayerKind::User,
            codex_home.clone(),
            user_config.clone(),
            true,
            &mut diagnostics,
        ));

        // Spec 1.2: markers come from the merged config without project layers.
        let mut base = Table::new();
        for layer in &layers {
            if let Some(config) = &layer.config {
                merge_tables(&mut base, config);
            }
        }
        let markers = root_markers(&base);

        // Spec 1.2: for AGENTS.md any file type counts, a `.git` directory needs no `HEAD`.
        let md_root_found_at = find_root(cx, &markers, false);
        let md_root_found = md_root_found_at.is_some();
        let md_root = md_root_found_at.unwrap_or_else(|| cx.env.cwd.clone());

        // Spec 6.3: a `.git` directory counts only with a `HEAD`.
        let cfg_root = find_root(cx, &markers, true).unwrap_or_else(|| cx.env.cwd.clone());
        let checkout = find_git_checkout(cx);
        let repo_root = checkout.as_ref().map(|c| repo_root_of(cx, c));

        let trusts = project_trusts(&base);
        let lookup = TrustLookup {
            cx,
            trusts: &trusts,
            cfg_root: &cfg_root,
            repo_root: repo_root.as_ref(),
        };

        // Spec 6.3: one project layer per `.codex` directory from the config root down to cwd.
        let mut dirs = Vec::new();
        let mut cursor = Some(cx.env.cwd.clone());
        while let Some(dir) = cursor {
            let done = dir == cfg_root;
            cursor = if done { None } else { dir.parent() };
            dirs.push(dir);
        }
        dirs.reverse();
        let codex_home_real = cx.fs.real_path(&codex_home).ok();
        for dir in dirs {
            let dot = dir.join(".codex");
            let is_dir = cx
                .fs
                .stat(&dot)
                .map(|s| s.kind == EntryKind::Dir)
                .unwrap_or(false);
            if !is_dir {
                continue;
            }
            // Spec 6.1: a `.codex` that is the Codex home is not a project layer.
            if dot == codex_home || cx.fs.real_path(&dot).ok() == codex_home_real {
                continue;
            }
            let decision = lookup.decide(&dir);
            let enabled = decision.level == TrustLevel::Trusted;
            let hooks_folder = match (&checkout, &repo_root) {
                (Some(co), Some(repo)) if co != repo && dir.starts_with(co) => {
                    let rel = &dir.as_str()[co.as_str().trim_end_matches('/').len()..];
                    repo.join(&format!(".{rel}")).join(".codex")
                }
                _ => dot.clone(),
            };
            let mut layer = read_layer(
                cx,
                LayerKind::Project { dir: dir.clone() },
                dot.clone(),
                dot.join("config.toml"),
                enabled,
                &mut diagnostics,
            );
            layer.hooks_folder = hooks_folder;
            if let Some(config) = layer.config.as_mut() {
                for key in PROJECT_DENYLIST {
                    config.remove(key);
                }
            }
            layers.push(layer);
        }
        layers.push(read_layer(
            cx,
            LayerKind::LegacyManaged,
            system_dir.clone(),
            system_dir.join("managed_config.toml"),
            true,
            &mut diagnostics,
        ));

        let mut merged = Table::new();
        for layer in layers.iter().filter(|l| l.enabled) {
            if let Some(config) = &layer.config {
                merge_tables(&mut merged, config);
            }
        }

        // Spec 6.3 trust matrix, AGENTS.md row: the cwd entry, else the repo root entry.
        let md_untrusted = [Some(&cx.env.cwd), repo_root.as_ref()]
            .into_iter()
            .flatten()
            .find_map(|path| lookup.entry(path))
            .is_some_and(|(level, _)| level == TrustLevel::Untrusted);
        let decision = lookup.decide(&cx.env.cwd);
        let trust = Trust {
            level: decision.level,
            recorded_in: decision.recorded.then(|| user_config.clone()),
        };

        let features = features_of(&merged);
        let project_doc_max_bytes =
            get_u64(&merged, &["project_doc_max_bytes"]).unwrap_or(DEFAULT_PROJECT_DOC_MAX_BYTES);
        let fallback_names = get_path(&merged, &["project_doc_fallback_filenames"])
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(|v| v.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();

        Setup {
            cwd: cx.env.cwd.clone(),
            home: cx.env.home.clone(),
            codex_home,
            home_from_env: choice.from_env,
            user_config,
            layers,
            merged,
            md_root,
            md_root_found,
            markers,
            md_untrusted,
            trust,
            features,
            project_doc_max_bytes,
            fallback_names,
            diagnostics,
        }
    }

    pub fn user_layer(&self) -> &Layer {
        self.layers
            .iter()
            .find(|l| l.kind == LayerKind::User)
            .expect("the user layer is always built")
    }

    /// The enabled layer that sets `path`, highest precedence first.
    pub fn layer_setting(&self, path: &[&str]) -> Option<(&Layer, &Value)> {
        self.layers
            .iter()
            .rev()
            .filter(|l| l.enabled)
            .find_map(|l| Some((l, get_path(l.config.as_ref()?, path)?)))
    }

    pub fn path_in_layer(&self, layer: &Layer, raw: &str) -> AbsPath {
        // Spec 6.2: a relative path resolves against the layer's folder.
        layer.folder.join(raw)
    }
}

fn read_layer(
    cx: &ScanCx,
    kind: LayerKind,
    folder: AbsPath,
    config_path: AbsPath,
    enabled: bool,
    diagnostics: &mut Vec<Diagnostic>,
) -> Layer {
    let mut config = None;
    if let Ok(bytes) = cx.fs.read(&config_path) {
        match toml::from_str::<Table>(&String::from_utf8_lossy(&bytes)) {
            Ok(table) => config = Some(table),
            Err(err) => {
                // Spec 6.3 edge case: a bad file is fatal only when its layer is active.
                if enabled {
                    diagnostics.push(Diagnostic {
                        severity: Severity::Error,
                        agent: Some(AgentId::builtin("codex")),
                        at: Subject::File {
                            path: config_path.clone(),
                            line: None,
                        },
                        problem: Problem::ParseError {
                            format: Format::Toml,
                            detail: err.message().to_owned(),
                            fatal_for_agent: matches!(
                                kind,
                                LayerKind::User | LayerKind::Project { .. }
                            ),
                        },
                        fix: None,
                    });
                }
            }
        }
    }
    Layer {
        hooks_folder: folder.clone(),
        kind,
        folder,
        config_path,
        config,
        enabled,
    }
}

// ---------------------------------------------------------------------------------------------
// Roots
// ---------------------------------------------------------------------------------------------

/// Spec 1.2: a non-array value falls back to the defaults. An empty array disables the walk.
fn root_markers(config: &Table) -> Vec<String> {
    match config.get("project_root_markers") {
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|v| v.as_str().map(str::to_owned))
            .collect(),
        _ => DEFAULT_ROOT_MARKERS
            .iter()
            .map(|m| (*m).to_owned())
            .collect(),
    }
}

fn find_root(cx: &ScanCx, markers: &[String], git_needs_head: bool) -> Option<AbsPath> {
    if markers.is_empty() {
        return None;
    }
    let mut cursor = Some(cx.env.cwd.clone());
    while let Some(dir) = cursor {
        for marker in markers {
            let path = dir.join(marker);
            let Ok(stat) = cx.fs.stat(&path) else {
                continue;
            };
            if git_needs_head
                && marker == ".git"
                && stat.kind == EntryKind::Dir
                && cx.fs.stat(&path.join("HEAD")).is_err()
            {
                continue;
            }
            return Some(dir);
        }
        cursor = dir.parent();
    }
    None
}

/// The nearest ancestor with a usable `.git` (a directory with `HEAD`, or a file).
fn find_git_checkout(cx: &ScanCx) -> Option<AbsPath> {
    find_root(cx, &[".git".to_owned()], true)
}

/// Spec 6.3: for a linked worktree the repo root is the main checkout.
fn repo_root_of(cx: &ScanCx, checkout: &AbsPath) -> AbsPath {
    let dot_git = checkout.join(".git");
    let is_file = cx
        .fs
        .stat(&dot_git)
        .map(|s| s.kind == EntryKind::File)
        .unwrap_or(false);
    if !is_file {
        return checkout.clone();
    }
    let Ok(bytes) = cx.fs.read(&dot_git) else {
        return checkout.clone();
    };
    let text = String::from_utf8_lossy(&bytes);
    let Some(gitdir) = text.trim().strip_prefix("gitdir:").map(str::trim) else {
        return checkout.clone();
    };
    let gitdir = checkout.join(gitdir);
    // `<main>/.git/worktrees/<name>`
    let in_worktrees = gitdir
        .parent()
        .and_then(|p| p.file_name().map(|n| n == "worktrees"))
        .unwrap_or(false);
    if !in_worktrees {
        return checkout.clone();
    }
    gitdir
        .parent()
        .and_then(|p| p.parent())
        .and_then(|git| git.parent())
        .unwrap_or_else(|| checkout.clone())
}

// ---------------------------------------------------------------------------------------------
// Trust
// ---------------------------------------------------------------------------------------------

fn project_trusts(config: &Table) -> Vec<(String, TrustLevel)> {
    let Some(Value::Table(projects)) = config.get("projects") else {
        return Vec::new();
    };
    projects
        .iter()
        .filter_map(|(key, entry)| {
            let level = match entry.get("trust_level")?.as_str()? {
                "trusted" => TrustLevel::Trusted,
                "untrusted" => TrustLevel::Untrusted,
                _ => return None,
            };
            Some((key.clone(), level))
        })
        .collect()
}

struct TrustLookup<'a, 'b> {
    cx: &'a ScanCx<'b>,
    trusts: &'a [(String, TrustLevel)],
    cfg_root: &'a AbsPath,
    repo_root: Option<&'a AbsPath>,
}

struct Decision {
    level: TrustLevel,
    recorded: bool,
}

impl TrustLookup<'_, '_> {
    /// The recorded level for one path, trying the canonical and the literal spelling.
    fn entry(&self, path: &AbsPath) -> Option<(TrustLevel, String)> {
        let canonical = self.cx.fs.real_path(path).ok();
        let spellings = canonical
            .iter()
            .map(|p| p.as_str())
            .chain(std::iter::once(path.as_str()));
        for spelling in spellings {
            if let Some((key, level)) = self.trusts.iter().find(|(key, _)| key == spelling) {
                return Some((*level, key.clone()));
            }
        }
        None
    }

    /// Spec 6.3: the directory itself, then the config root, then the repo root.
    fn decide(&self, dir: &AbsPath) -> Decision {
        let order = [Some(dir), Some(self.cfg_root), self.repo_root];
        for path in order.into_iter().flatten() {
            if let Some((level, _)) = self.entry(path) {
                return Decision {
                    level,
                    recorded: true,
                };
            }
        }
        Decision {
            level: TrustLevel::Unknown,
            recorded: false,
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Values
// ---------------------------------------------------------------------------------------------

pub(super) fn merge_tables(into: &mut Table, from: &Table) {
    for (key, value) in from {
        match (into.get_mut(key), value) {
            (Some(Value::Table(a)), Value::Table(b)) => merge_tables(a, b),
            _ => {
                into.insert(key.clone(), value.clone());
            }
        }
    }
}

pub(super) fn get_path<'a>(table: &'a Table, path: &[&str]) -> Option<&'a Value> {
    let (first, rest) = path.split_first()?;
    let mut value = table.get(*first)?;
    for key in rest {
        value = value.as_table()?.get(*key)?;
    }
    Some(value)
}

pub(super) fn get_u64(table: &Table, path: &[&str]) -> Option<u64> {
    get_path(table, path)?
        .as_integer()
        .and_then(|n| u64::try_from(n).ok())
}

pub(super) fn get_bool(table: &Table, path: &[&str]) -> Option<bool> {
    get_path(table, path)?.as_bool()
}

pub(super) fn get_str<'a>(table: &'a Table, path: &[&str]) -> Option<&'a str> {
    get_path(table, path)?.as_str()
}

/// Spec 2.3, 3.1, 5.4: `hooks` (alias `codex_hooks`) and `plugins` default on, `memories`
/// (alias `memory_tool`) defaults off.
fn features_of(config: &Table) -> Features {
    let flag = |names: &[&str], default: bool| {
        names
            .iter()
            .find_map(|name| get_bool(config, &["features", name]))
            .unwrap_or(default)
    };
    Features {
        hooks: flag(&["hooks", "codex_hooks"], true),
        memories: flag(&["memories", "memory_tool"], false),
        plugins: flag(&["plugins"], true),
    }
}

/// `toml::Value` to JSON, for the handler `raw` and the hook hash.
pub(super) fn to_json(value: &Value) -> serde_json::Value {
    serde_json::to_value(value).unwrap_or(serde_json::Value::Null)
}

/// A shared read of a text file through the probe layer. Lossy, like Codex.
pub(super) fn read_lossy(cx: &ScanCx, path: &AbsPath) -> Option<(Arc<Vec<u8>>, String)> {
    let bytes = cx.fs.read(path).ok()?;
    let text = String::from_utf8_lossy(&bytes).into_owned();
    Some((bytes, text))
}
