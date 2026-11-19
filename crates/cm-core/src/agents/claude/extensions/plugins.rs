//! Which plugins are enabled for the project and where their files are (spec 4, "Plugins").

use std::collections::BTreeSet;

use serde_json::Value;

use crate::agents::ScanCx;
use crate::agents::claude::setup::{SettingsKind, Setup};
use crate::model::*;
use crate::probe_fs::EntryKind;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::agents::claude) enum Origin {
    /// An install record and a cache directory.
    Marketplace,
    /// `--plugin-dir`, `CLAUDE_CODE_PLUGIN_DIRS`.
    Inline,
    /// `<dir>/.claude/skills/<d>/.claude-plugin/plugin.json`.
    SkillsDir,
}

#[derive(Debug, Clone)]
pub(in crate::agents::claude) struct Plugin {
    /// `name@origin`.
    pub id: String,
    /// The manifest `name`, else the part of the id before `@`.
    pub name: String,
    pub root: AbsPath,
    pub origin: Origin,
    pub enabled: bool,
    /// Where `enabledPlugins` said so, when a settings file did.
    /// The settings file that decided enablement. Kept for the UI to show.
    #[allow(dead_code)]
    pub enabled_by: Option<AbsPath>,
    /// A managed `enabledPlugins` `true` force-enables the plugin (spec 3, `allowManagedHooksOnly`).
    pub forced_by_managed: bool,
    pub manifest: Option<Value>,
    pub manifest_file: AbsPath,
}

impl Plugin {
    pub fn scope(&self) -> Scope {
        Scope::Plugin {
            id: self.id.clone(),
        }
    }

    /// Spec 4, "Editor handling per file type": the cache is overwritten on update. Plugins
    /// loaded in place belong to the user.
    pub fn policy(&self) -> EditPolicy {
        match self.origin {
            Origin::Marketplace => EditPolicy::ReadOnly {
                why: ReadOnlyWhy::PluginCache {
                    plugin: self.id.clone(),
                },
            },
            _ => EditPolicy::Editable,
        }
    }

    pub fn data_dir(&self, setup: &Setup) -> AbsPath {
        let key: String = self
            .id
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                    c
                } else {
                    '-'
                }
            })
            .collect();
        setup.plugins.join("data").join(&key)
    }
}

fn read_json(cx: &ScanCx, path: &AbsPath) -> Option<Value> {
    serde_json::from_slice(cx.fs.read(path).ok()?.as_slice()).ok()
}

fn is_dir(cx: &ScanCx, path: &AbsPath) -> bool {
    cx.fs
        .stat(path)
        .map(|s| s.kind == EntryKind::Dir)
        .unwrap_or(false)
}

/// Spec 4, "Which plugins are enabled for project P" and "Plugin origins". Not read: synced
/// plugins and built-in plugins (no spec for their layout), marketplace `strict` entries.
pub(in crate::agents::claude) fn discover(cx: &ScanCx, setup: &Setup) -> Vec<Plugin> {
    let merged = setup.enabled_plugins();
    let forced: BTreeSet<String> = setup
        .settings
        .iter()
        .filter(|f| matches!(f.kind, SettingsKind::Managed | SettingsKind::Remote))
        .filter_map(|f| f.get("enabledPlugins")?.as_object())
        .flat_map(|map| {
            map.iter()
                .filter(|(_, v)| v.as_bool() == Some(true))
                .map(|(k, _)| k.clone())
        })
        .collect();
    let enabled_by = |id: &str| {
        setup
            .settings
            .iter()
            .find(|f| {
                f.get("enabledPlugins")
                    .and_then(|m| m.get(id))
                    .is_some_and(Value::is_boolean)
            })
            .map(|f| f.path.clone())
    };
    let mut found: Vec<Plugin> = Vec::new();
    let add = |found: &mut Vec<Plugin>,
               id: String,
               root: AbsPath,
               origin: Origin,
               manifest: Option<Value>,
               manifest_file: AbsPath| {
        let name = manifest
            .as_ref()
            .and_then(|m| m.get("name")?.as_str().map(str::to_owned))
            .unwrap_or_else(|| id.split('@').next().unwrap_or(&id).to_owned());
        let default_on = manifest
            .as_ref()
            .and_then(|m| m.get("defaultEnabled")?.as_bool())
            .unwrap_or(true);
        let enabled = merged.get(&id).copied().unwrap_or(default_on);
        found.push(Plugin {
            forced_by_managed: forced.contains(&id),
            enabled_by: enabled_by(&id),
            id,
            name,
            root,
            origin,
            enabled,
            manifest,
            manifest_file,
        });
    };

    // Session-only plugins: CLAUDE_CODE_PLUGIN_DIRS, absolute paths separated by ':'.
    if let Some(dirs) = cx.env.var("CLAUDE_CODE_PLUGIN_DIRS") {
        for raw in dirs.split(':').filter(|d| d.starts_with('/')) {
            let Ok(dir) = AbsPath::parse(raw) else {
                continue;
            };
            let manifest_file = dir.join(".claude-plugin/plugin.json");
            let manifest = read_json(cx, &manifest_file);
            if manifest.is_some() {
                let name = manifest_name(&manifest, &dir);
                add(
                    &mut found,
                    format!("{name}@inline"),
                    dir,
                    Origin::Inline,
                    manifest,
                    manifest_file,
                );
            }
        }
    }

    // Marketplace installs: an install record that applies and an install path that exists.
    let installed = read_json(cx, &setup.plugins.join("installed_plugins.json"));
    if let Some(Value::Object(plugins)) = installed.as_ref().and_then(|v| v.get("plugins")) {
        for (id, records) in plugins {
            let Some(records) = records.as_array() else {
                continue;
            };
            let applies = |record: &Value| match record.get("scope").and_then(Value::as_str) {
                Some("project" | "local") => record
                    .get("projectPath")
                    .and_then(Value::as_str)
                    .is_some_and(|p| {
                        [Some(&setup.cwd), setup.project.main_root.as_ref()]
                            .into_iter()
                            .flatten()
                            .any(|c| c.as_str() == p)
                    }),
                _ => true,
            };
            let Some(record) = records.iter().find(|r| applies(r)) else {
                continue;
            };
            let Some(root) = record
                .get("installPath")
                .and_then(Value::as_str)
                .and_then(|p| AbsPath::parse(p).ok())
            else {
                continue;
            };
            if !is_dir(cx, &root) {
                continue;
            }
            let manifest_file = root.join(".claude-plugin/plugin.json");
            let manifest = read_json(cx, &manifest_file);
            add(
                &mut found,
                id.clone(),
                root,
                Origin::Marketplace,
                manifest,
                manifest_file,
            );
        }
    }

    // Skills-directory plugins: the user copy beats the project copy (spec 4, name conflicts).
    let mut skill_roots = vec![setup.config.join("skills")];
    if setup.trust().level != TrustLevel::Untrusted {
        skill_roots.push(setup.cwd.join(".claude/skills"));
    }
    for base in skill_roots {
        let Ok(entries) = cx.fs.list(&base) else {
            continue;
        };
        for entry in entries.iter().filter(|e| e.kind == EntryKind::Dir) {
            let root = base.join(&entry.name);
            let manifest_file = root.join(".claude-plugin/plugin.json");
            let manifest = read_json(cx, &manifest_file);
            if manifest.is_some() {
                let name = manifest_name(&manifest, &root);
                add(
                    &mut found,
                    format!("{name}@skills-dir"),
                    root,
                    Origin::SkillsDir,
                    manifest,
                    manifest_file,
                );
            }
        }
    }

    // Name conflicts: managed-listed id, then inline, then marketplace, then skills-dir.
    let rank = |p: &Plugin| {
        (
            !p.forced_by_managed,
            match p.origin {
                Origin::Inline => 0,
                Origin::Marketplace => 1,
                Origin::SkillsDir => 2,
            },
        )
    };
    found.sort_by_key(rank);
    let mut seen = BTreeSet::new();
    found.retain(|p| seen.insert(p.name.clone()));
    found
}

fn manifest_name(manifest: &Option<Value>, root: &AbsPath) -> String {
    manifest
        .as_ref()
        .and_then(|m| m.get("name")?.as_str().map(str::to_owned))
        .unwrap_or_else(|| root.file_name().unwrap_or("plugin").to_owned())
}
