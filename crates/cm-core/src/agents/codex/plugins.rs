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

/// The marketplaces Codex leaves out depend on how the user is logged in (`PluginsConfigToml::
/// allows_plugin`): without a ChatGPT login, `openai-curated` and `openai-curated-remote` are
/// dropped. With one, `openai-curated` is replaced by the remote catalog. A scan cannot read the
/// login, so it looks for `auth.json` and records the guess as an assumption.
fn marketplace_allowed(has_login_file: bool, marketplace: &str) -> bool {
    if has_login_file {
        !matches!(marketplace, "openai-curated" | "openai-api-curated")
    } else {
        !matches!(marketplace, "openai-curated" | "openai-curated-remote")
    }
}

/// Spec 5.4: the plugins named in `[plugins."<name>@<marketplace>"]` that have an installed
/// version in the cache. A cache directory with no config entry is not loaded.
pub(super) fn installed(cx: &ScanCx, setup: &Setup) -> Vec<Plugin> {
    let mut out = Vec::new();
    let Some(table) = setup.merged.get("plugins").and_then(toml::Value::as_table) else {
        return out;
    };
    let has_login_file = cx.fs.stat(&setup.codex_home.join("auth.json")).is_ok();
    let cache = setup.codex_home.join("plugins/cache");
    // `load_plugins_from_layer_stack` sorts by the config name.
    let mut ids: Vec<&String> = table
        .keys()
        .filter(|k| k.as_str() != "_default" && k.contains('@'))
        .collect();
    ids.sort();
    for id in ids {
        let (name, marketplace) = id.split_once('@').expect("checked");
        let pdir = cache.join(marketplace).join(name);
        let Ok(versions) = cx.fs.list(&pdir) else {
            continue;
        };
        let dirs: Vec<&str> = versions
            .iter()
            .filter(|e| e.kind == EntryKind::Dir)
            .map(|e| e.name.as_str())
            .filter(|v| valid_version(v))
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
        let manifest_name = manifest
            .as_ref()
            .and_then(|m| m.get("name"))
            .and_then(|n| n.as_str())
            .map(str::to_owned);
        // Spec 5.4: an explicit setting overrides `_default`.
        let enabled = table
            .get(id)
            .and_then(|e| e.get("enabled"))
            .and_then(toml::Value::as_bool)
            .or_else(|| get_bool(&setup.merged, &["plugins", "_default", "enabled"]))
            .unwrap_or(true)
            && setup.features.plugins
            && marketplace_allowed(has_login_file, marketplace);
        out.push(Plugin {
            id: id.clone(),
            name: manifest_name.unwrap_or_else(|| name.to_owned()),
            root,
            enabled,
            manifest,
        });
    }
    out
}

/// `validate_plugin_version_segment`.
fn valid_version(name: &str) -> bool {
    !name.is_empty()
        && !matches!(name, "." | "..")
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '+'))
}
