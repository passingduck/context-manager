//! Everything a scan or a render needs to know before it looks at a context file: the homes,
//! the settings layers, folder trust, the context file names and the include directories.

use super::extension::{self, ExtInfo};
use super::settings::{self, LayerKind, Settings};
use super::trust::{self, TrustInfo};
use super::util::{gemini_home, is_dir, normalize_name, real, within};
use crate::agents::ScanCx;
use crate::model::{AbsPath, Diagnostic, Format, Problem, Severity, Subject};

pub(super) const DEFAULT_NAME: &str = "GEMINI.md";

pub(super) struct World<'a> {
    pub cx: &'a ScanCx<'a>,
    /// `~` as Gemini CLI resolves it: `GEMINI_CLI_HOME`, else the OS home (spec 7).
    pub home: AbsPath,
    /// `<home>/.gemini`.
    pub gdir: AbsPath,
    pub cwd: AbsPath,
    pub cwd_is_home: bool,
    pub settings: Settings,
    pub trust: TrustInfo,
    pub names: Vec<String>,
    pub markers: Vec<String>,
    /// `cwd`, then each existing include directory.
    pub roots: Vec<AbsPath>,
    pub exts: Vec<ExtInfo>,
    /// Problems that stop the CLI before it loads anything.
    pub fatal: Vec<(AbsPath, Format, String)>,
    pub diagnostics: Vec<Diagnostic>,
}

impl<'a> World<'a> {
    pub fn new(cx: &'a ScanCx<'a>) -> World<'a> {
        let home = gemini_home(cx);
        let gdir = home.join(".gemini");
        let cwd = cx.env.cwd.clone();
        let cwd_is_home = real(cx, &cwd) == real(cx, &home);

        let mut layers = Vec::new();
        if let Some(path) = settings::system_defaults_path(cx) {
            layers.push(settings::read_layer(
                cx,
                LayerKind::SystemDefaults,
                path,
                true,
            ));
        }
        layers.push(settings::read_layer(
            cx,
            LayerKind::User,
            gdir.join("settings.json"),
            true,
        ));
        let system = settings::system_settings_path(cx)
            .map(|path| settings::read_layer(cx, LayerKind::System, path, true));

        // Spec 2: the trust check can read only system defaults, user and system settings.
        let early = settings::merge_layers(layers.iter().chain(system.iter()));
        let early = Settings {
            layers: Vec::new(),
            merged: early,
        };
        let trust = trust::check(
            cx,
            &gdir,
            early.bool("security.folderTrust.enabled", true),
            &cwd,
        );

        if !cwd_is_home {
            layers.push(settings::read_layer(
                cx,
                LayerKind::Workspace,
                cwd.join(".gemini/settings.json"),
                trust.trusted,
            ));
        }
        layers.extend(system);
        let merged = settings::merge_layers(layers.iter().filter(|layer| layer.active));
        let settings = Settings { layers, merged };

        let mut fatal: Vec<(AbsPath, Format, String)> = settings
            .errors()
            .map(|(path, detail)| (path.clone(), Format::Jsonc, detail.to_owned()))
            .collect();
        if let Some(detail) = &trust.error {
            fatal.push((trust.file.clone(), Format::Jsonc, detail.clone()));
        }

        let loaded = extension::load(cx, &gdir, &cwd, &settings);
        fatal.extend(
            loaded
                .fatal
                .into_iter()
                .map(|(path, detail)| (path, Format::Json, detail)),
        );
        let mut diagnostics = loaded.diagnostics;
        for (path, _, detail) in &fatal {
            diagnostics.push(Diagnostic {
                severity: Severity::Error,
                agent: None,
                at: Subject::File {
                    path: path.clone(),
                    line: None,
                },
                problem: Problem::ParseError {
                    format: Format::Jsonc,
                    detail: detail.clone(),
                    fatal_for_agent: true,
                },
                fix: None,
            });
        }

        let names = context_names(&settings);
        let markers = boundary_markers(&settings);
        let roots = workspace_roots(cx, &home, &cwd, &settings);
        World {
            cx,
            home,
            gdir,
            cwd,
            cwd_is_home,
            settings,
            trust,
            names,
            markers,
            roots,
            exts: loaded.exts,
            fatal,
            diagnostics,
        }
    }

    pub fn trusted(&self) -> bool {
        self.trust.trusted
    }

    /// `findProjectRoot`: the nearest ancestor of `start` (itself included) holding any boundary
    /// marker. A file or a directory counts. Markers that are absolute or contain `..` are
    /// skipped (spec 1, project row).
    pub fn boundary(&self, start: &AbsPath) -> Option<AbsPath> {
        if self.markers.is_empty() {
            return None;
        }
        let mut dir = Some(start.clone());
        while let Some(current) = dir {
            let hit = self
                .markers
                .iter()
                .filter(|m| !m.starts_with('/') && !m.contains(".."))
                .any(|marker| self.cx.fs.stat(&current.join(marker)).is_ok());
            if hit {
                return Some(current);
            }
            dir = current.parent();
        }
        None
    }

    /// The directory an import in a file under `dir` may not leave (spec 3, "Allowed root").
    pub fn import_root(&self, dir: &AbsPath) -> AbsPath {
        self.boundary(dir).unwrap_or_else(|| dir.clone())
    }

    /// The deepest workspace root that holds `path` (`loadJitSubdirectoryMemory`).
    pub fn root_of(&self, path: &AbsPath) -> Option<&AbsPath> {
        self.roots
            .iter()
            .filter(|root| within(self.cx.env.os, root, path))
            .max_by_key(|root| root.as_str().len())
    }
}

/// Spec 2, `context.fileName`: the user's names come first and `GEMINI.md` stays.
fn context_names(settings: &Settings) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for raw in settings.strings("context.fileName") {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            continue;
        }
        let name = normalize_name(trimmed);
        if !name.is_empty() && !names.contains(&name) {
            names.push(name);
        }
    }
    if !names.iter().any(|n| n == DEFAULT_NAME) {
        names.push(DEFAULT_NAME.to_owned());
    }
    names
}

/// Spec 1, project row: default `[".git"]`; an empty array checks only the workspace root.
fn boundary_markers(settings: &Settings) -> Vec<String> {
    match settings.get("context.memoryBoundaryMarkers") {
        Some(serde_json::Value::Array(items)) => items
            .iter()
            .filter_map(|item| item.as_str().map(str::to_owned))
            .collect(),
        _ => vec![".git".to_owned()],
    }
}

/// Spec 1, "Include directories": settings, then the VS Code folders. Missing directories are
/// skipped.
fn workspace_roots(
    cx: &ScanCx,
    home: &AbsPath,
    cwd: &AbsPath,
    settings: &Settings,
) -> Vec<AbsPath> {
    let mut roots = vec![cwd.clone()];
    let mut candidates: Vec<AbsPath> = Vec::new();
    for raw in settings.strings("context.includeDirectories") {
        let expanded = if raw.to_lowercase().starts_with("%userprofile%") {
            format!("{}{}", home, &raw["%userprofile%".len()..])
        } else if raw == "~" || raw.starts_with("~/") {
            format!("{}{}", home, &raw[1..])
        } else {
            raw.clone()
        };
        if expanded.is_empty() {
            continue;
        }
        candidates.push(cwd.join(&expanded));
    }
    if let Some(ide) = cx.env.var("GEMINI_CLI_IDE_WORKSPACE_PATH") {
        for folder in ide.split(':').map(str::trim).filter(|f| !f.is_empty()) {
            if let Ok(path) = AbsPath::parse(folder)
                && real(cx, &path) != real(cx, cwd)
            {
                candidates.push(path);
            }
        }
    }
    for dir in candidates {
        if is_dir(cx, &dir) && !roots.contains(&dir) {
            roots.push(dir);
        }
    }
    roots
}
