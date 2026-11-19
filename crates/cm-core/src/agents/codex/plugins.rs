//! Installed plugins (spec 5.4): the active version of each cache entry, its skills and hooks.

use super::setup::{Setup, get_bool};
use crate::agents::ScanCx;
use crate::model::*;
use crate::probe_fs::EntryKind;

const MANIFESTS: [&str; 3] = [
    ".codex-plugin/plugin.json",
    ".claude-plugin/plugin.json",
    ".cursor-plugin/plugin.json",
];
/// Spec 5.4: `DEFAULT_PLUGIN_VERSION`.
const LOCAL_VERSION: &str = "local";

#[derive(Debug, Clone)]
pub(super) struct Plugin {
    /// `<plugin>@<marketplace>`, the key of `[plugins."..."]`.
    pub id: String,
    /// The manifest name, which namespaces its skills.
    pub name: String,
    /// The active version directory.
    pub root: AbsPath,
    pub enabled: bool,
    pub manifest: Option<serde_json::Value>,
}

impl Plugin {
    /// The skill roots: the manifest `skills` paths, else `<root>/skills` (spec 5.4).
    pub fn skill_roots(&self) -> Vec<AbsPath> {
        let listed = self
            .manifest
            .as_ref()
            .and_then(|m| m.get("skills"))
            .map(|v| match v {
                serde_json::Value::String(s) => vec![s.clone()],
                serde_json::Value::Array(items) => items
                    .iter()
                    .filter_map(|i| i.as_str().map(str::to_owned))
                    .collect(),
                _ => Vec::new(),
            });
        match listed {
            Some(paths) if !paths.is_empty() => paths.iter().map(|p| self.root.join(p)).collect(),
            _ => vec![self.root.join("skills")],
        }
    }
}

/// Spec 5.4, "`compare_plugin_versions`": numeric parts compare as numbers, the rest as text.
fn version_key(name: &str) -> Vec<(u64, String)> {
    name.split(['.', '-', '+'])
        .map(|part| match part.parse::<u64>() {
            Ok(n) => (n, String::new()),
            Err(_) => (0, part.to_owned()),
        })
        .collect()
}

pub(super) fn installed(cx: &ScanCx, setup: &Setup) -> Vec<Plugin> {
    let cache = setup.codex_home.join("plugins/cache");
    let mut out = Vec::new();
    let Ok(marketplaces) = cx.fs.list(&cache) else {
        return out;
    };
    for marketplace in marketplaces.iter().filter(|e| e.kind == EntryKind::Dir) {
        let mdir = cache.join(&marketplace.name);
        let Ok(plugins) = cx.fs.list(&mdir) else {
            continue;
        };
        for plugin in plugins.iter().filter(|e| e.kind == EntryKind::Dir) {
            let pdir = mdir.join(&plugin.name);
            let Ok(versions) = cx.fs.list(&pdir) else {
                continue;
            };
            let dirs: Vec<&str> = versions
                .iter()
                .filter(|e| e.kind == EntryKind::Dir)
                .map(|e| e.name.as_str())
                .collect();
            let active = if dirs.contains(&LOCAL_VERSION) {
                Some(LOCAL_VERSION)
            } else {
                dirs.iter().copied().max_by_key(|v| version_key(v))
            };
            let Some(version) = active else { continue };
            let root = pdir.join(version);
            let manifest = MANIFESTS.iter().find_map(|rel| {
                let bytes = cx.fs.read(&root.join(rel)).ok()?;
                serde_json::from_slice::<serde_json::Value>(&bytes).ok()
            });
            let name = manifest
                .as_ref()
                .and_then(|m| m.get("name"))
                .and_then(|n| n.as_str())
                .map(str::trim)
                .filter(|n| !n.is_empty())
                .unwrap_or(&plugin.name)
                .to_owned();
            let id = format!("{}@{}", plugin.name, marketplace.name);
            // Spec 5.4: an explicit setting overrides `_default`.
            let enabled = get_bool(&setup.merged, &["plugins", &id, "enabled"])
                .or_else(|| get_bool(&setup.merged, &["plugins", "_default", "enabled"]))
                .unwrap_or(true)
                && setup.features.plugins;
            out.push(Plugin {
                id,
                name,
                root,
                enabled,
                manifest,
            });
        }
    }
    out
}
