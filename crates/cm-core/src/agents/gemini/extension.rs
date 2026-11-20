//! Installed extensions (spec 6): manifest, install metadata, enablement and context files.

use regex::Regex;
use serde_json::Value;

use super::settings::Settings;
use super::util::{accessible, normalize_name, read_lossy, real, rule, within};
use crate::agents::ScanCx;
use crate::model::{AbsPath, Diagnostic, Problem, Severity, Subject};
use crate::probe_fs::EntryKind;
use crate::walk;

#[derive(Debug, Clone)]
pub(super) struct ExtInfo {
    pub name: String,
    /// `~/.gemini/extensions/<dir>`.
    pub dir: AbsPath,
    /// Where every file is read from: the install directory, or the `source` of a linked one.
    pub root: AbsPath,
    pub linked: bool,
    /// `None` when active. Otherwise the enablement rule that turned it off.
    pub off_by: Option<String>,
    pub context_files: Vec<AbsPath>,
    /// The manifest declares MCP servers, whose instructions the CLI may add to the session.
    pub has_mcp: bool,
}

impl ExtInfo {
    pub fn active(&self) -> bool {
        self.off_by.is_none()
    }
}

pub(super) struct Loaded {
    pub exts: Vec<ExtInfo>,
    pub diagnostics: Vec<Diagnostic>,
    /// A duplicate name is a fatal error for the whole CLI.
    pub fatal: Vec<(AbsPath, String)>,
}

pub(super) fn load(cx: &ScanCx, gdir: &AbsPath, cwd: &AbsPath, settings: &Settings) -> Loaded {
    let mut out = Loaded {
        exts: Vec::new(),
        diagnostics: Vec::new(),
        fatal: Vec::new(),
    };
    let base = gdir.join("extensions");
    if !cx.fs.stat(&base).is_ok_and(|s| s.kind == EntryKind::Dir) {
        return out;
    }
    let entries = walk::list_dir(cx.fs, &base);
    let admin_off = settings
        .get("admin.extensions.enabled")
        .and_then(Value::as_bool)
        == Some(false);
    let enablement = read_enablement(cx, &base);
    for entry in entries.iter().filter(|e| e.kind == EntryKind::Dir) {
        if entry.name().ends_with(".lock") {
            continue;
        }
        let dir = entry.path.clone();
        let manifest_path = dir.join("gemini-extension.json");
        let reject = |why: &'static str, at: &AbsPath| Diagnostic {
            severity: Severity::Warning,
            agent: None,
            at: Subject::File {
                path: at.clone(),
                line: None,
            },
            problem: Problem::ExtensionRejected { why: rule(why) },
            fix: None,
        };
        let meta = read_install_metadata(cx, &dir);
        if admin_off {
            out.diagnostics
                .push(reject("gemini.extension.admin_disabled", &dir));
            continue;
        }
        if let Some(why) = blocked_by_security(cx, settings, meta.as_ref()) {
            out.diagnostics.push(reject(why, &dir));
            continue;
        }
        let linked = meta
            .as_ref()
            .is_some_and(|m| m.get("type").and_then(Value::as_str) == Some("link"));
        let root = if linked {
            meta.as_ref()
                .and_then(|m| m.get("source")?.as_str())
                .and_then(|raw| AbsPath::parse(raw).ok())
                .unwrap_or_else(|| dir.clone())
        } else {
            dir.clone()
        };
        let manifest_path = if linked {
            root.join("gemini-extension.json")
        } else {
            manifest_path
        };
        let Some(text) = read_lossy(cx, &manifest_path) else {
            out.diagnostics
                .push(reject("gemini.extension.invalid_manifest", &manifest_path));
            continue;
        };
        let manifest: Value = match serde_json::from_str(&text) {
            Ok(value) => value,
            Err(_) => {
                out.diagnostics
                    .push(reject("gemini.extension.invalid_manifest", &manifest_path));
                continue;
            }
        };
        let (Some(name), Some(_)) = (
            manifest.get("name").and_then(Value::as_str),
            manifest.get("version").and_then(Value::as_str),
        ) else {
            out.diagnostics
                .push(reject("gemini.extension.invalid_manifest", &manifest_path));
            continue;
        };
        if out.exts.iter().any(|e| e.name == name) {
            out.fatal.push((
                manifest_path.clone(),
                format!("duplicate extension name {name}"),
            ));
            continue;
        }
        let off_by = disabled_by(&enablement, name, cwd);
        let context_files = context_files(cx, &manifest, &root);
        let has_mcp = manifest
            .get("mcpServers")
            .and_then(Value::as_object)
            .is_some_and(|servers| !servers.is_empty());
        out.exts.push(ExtInfo {
            name: name.to_owned(),
            dir,
            root,
            linked,
            off_by,
            context_files,
            has_mcp,
        });
    }
    out
}

fn read_install_metadata(cx: &ScanCx, dir: &AbsPath) -> Option<Value> {
    serde_json::from_str(&read_lossy(
        cx,
        &dir.join(".gemini-extension-install.json"),
    )?)
    .ok()
}

/// `security.allowedExtensions` (a regex list on the install source) and
/// `security.blockGitExtensions` (spec 6, extension row).
fn blocked_by_security(
    cx: &ScanCx,
    settings: &Settings,
    meta: Option<&Value>,
) -> Option<&'static str> {
    let allowed = settings.strings("security.allowedExtensions");
    if !allowed.is_empty() {
        let source = meta
            .and_then(|m| m.get("source")?.as_str())
            .and_then(|raw| AbsPath::parse(raw).ok());
        let Some(source) = source else {
            return Some("gemini.extension.not_allowed");
        };
        let source = real(cx, &source);
        let ok = allowed
            .iter()
            .filter_map(|pattern| Regex::new(pattern).ok())
            .any(|re| re.is_match(source.as_str()));
        return (!ok).then_some("gemini.extension.not_allowed");
    }
    let remote = meta
        .and_then(|m| m.get("type")?.as_str())
        .is_some_and(|kind| kind == "git" || kind == "github-release");
    (remote && settings.bool("security.blockGitExtensions", false))
        .then_some("gemini.extension.git_blocked")
}

fn read_enablement(cx: &ScanCx, base: &AbsPath) -> Value {
    read_lossy(cx, &base.join("extension-enablement.json"))
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or(Value::Null)
}

/// Spec 6, "Enablement": the last matching rule wins, and the default is enabled. Returns the
/// rule that disabled the extension.
fn disabled_by(enablement: &Value, name: &str, cwd: &AbsPath) -> Option<String> {
    let rules = enablement.get(name)?.get("overrides")?.as_array()?;
    let target = format!("{}/", cwd.as_str().trim_end_matches('/'));
    let mut verdict: Option<String> = None;
    for raw in rules.iter().filter_map(Value::as_str) {
        let disable = raw.starts_with('!');
        let body = raw.trim_start_matches('!');
        let subdirs = body.ends_with('*');
        let base = body.trim_end_matches('*');
        let hit = if subdirs {
            let stem = base.trim_end_matches('/');
            target == format!("{stem}/") || target.starts_with(&format!("{stem}/"))
        } else {
            target == base
        };
        if hit {
            verdict = disable.then(|| raw.to_owned());
        }
    }
    verdict
}

/// Spec 6, "Extension context files".
fn context_files(cx: &ScanCx, manifest: &Value, root: &AbsPath) -> Vec<AbsPath> {
    let names: Vec<Value> = match manifest.get("contextFileName") {
        None | Some(Value::Null) => vec![Value::String("GEMINI.md".to_owned())],
        Some(Value::Array(items)) => items.clone(),
        Some(one) => vec![one.clone()],
    };
    names
        .iter()
        .filter_map(|name| {
            let name = name.as_str().filter(|s| !s.trim().is_empty())?;
            if name.contains('\0') || name.starts_with('/') || name.contains("..") {
                return None;
            }
            let path = root.join(&normalize_name(name));
            (within(crate::model::Os::Linux, root, &path) && accessible(cx, &path)).then_some(path)
        })
        .collect()
}
