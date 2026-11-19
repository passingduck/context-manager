//! The four settings files, their merge rules and JSONC parsing (spec 2).

use serde_json::{Map, Value};

use crate::agents::ScanCx;
use crate::cfg::jsonc;
use crate::model::{AbsPath, Os};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum LayerKind {
    SystemDefaults,
    User,
    Workspace,
    System,
}

#[derive(Debug, Clone)]
pub(super) struct Layer {
    pub kind: LayerKind,
    pub path: AbsPath,
    pub exists: bool,
    /// An object. Empty when the file is missing, unreadable or does not parse.
    pub value: Value,
    pub error: Option<String>,
    /// False for a workspace file that an untrusted folder ignores (spec 2, "Workspace").
    pub active: bool,
}

#[derive(Debug, Clone)]
pub(super) struct Settings {
    /// In merge order, lowest first: system defaults, user, workspace, system.
    pub layers: Vec<Layer>,
    pub merged: Value,
}

/// Spec 2, "Comments (JSONC)": comments pass, a trailing comma does not, and the top level must
/// be an object.
pub(super) fn parse_settings_text(text: &str) -> Result<Value, String> {
    if let Some(comma) = jsonc::trailing_commas(text).first() {
        return Err(format!("trailing comma at {}:{}", comma.line, comma.col));
    }
    match jsonc::get(text, "") {
        Err(err) => Err(err.to_string()),
        Ok(None) => Err("the file is empty".to_owned()),
        Ok(Some(value @ Value::Object(_))) => Ok(value),
        Ok(Some(_)) => Err("the top level is not an object".to_owned()),
    }
}

/// Spec 2, system rows. `None` on Windows and other systems, where the paths are not POSIX.
pub(super) fn system_settings_path(cx: &ScanCx) -> Option<AbsPath> {
    if let Some(raw) = cx.env.var("GEMINI_CLI_SYSTEM_SETTINGS_PATH") {
        return Some(cx.env.cwd.join(raw));
    }
    match cx.env.os {
        Os::Macos => AbsPath::parse("/Library/Application Support/GeminiCli/settings.json").ok(),
        Os::Linux => AbsPath::parse("/etc/gemini-cli/settings.json").ok(),
        _ => None,
    }
}

pub(super) fn system_defaults_path(cx: &ScanCx) -> Option<AbsPath> {
    if let Some(raw) = cx.env.var("GEMINI_CLI_SYSTEM_DEFAULTS_PATH") {
        return Some(cx.env.cwd.join(raw));
    }
    system_settings_path(cx)?
        .parent()
        .map(|dir| dir.join("system-defaults.json"))
}

pub(super) fn read_layer(cx: &ScanCx, kind: LayerKind, path: AbsPath, active: bool) -> Layer {
    let mut layer = Layer {
        kind,
        path: path.clone(),
        exists: false,
        value: Value::Object(Map::new()),
        error: None,
        active,
    };
    let Ok(bytes) = cx.fs.read(&path) else {
        return layer;
    };
    layer.exists = true;
    match parse_settings_text(&String::from_utf8_lossy(&bytes)) {
        Ok(value) => layer.value = value,
        Err(detail) => layer.error = Some(detail),
    }
    layer
}

/// Spec 2, "Precedence": objects deep-merge, scalars take the last value, and arrays follow a
/// per-key strategy.
pub(super) fn merge_layers<'a>(layers: impl IntoIterator<Item = &'a Layer>) -> Value {
    let mut merged = Value::Object(Map::new());
    for layer in layers {
        merge(&mut merged, &layer.value, &mut Vec::new());
    }
    merged
}

enum ArrayMerge {
    Concat,
    Union,
    Replace,
}

fn array_strategy(path: &[String]) -> ArrayMerge {
    match path.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        ["hooks", _] | ["context", "includeDirectories"] => ArrayMerge::Concat,
        ["hooksConfig", "disabled"] | ["context", "fileFiltering", "customIgnoreFilePaths"] => {
            ArrayMerge::Union
        }
        _ => ArrayMerge::Replace,
    }
}

fn merge(base: &mut Value, over: &Value, path: &mut Vec<String>) {
    let (Value::Object(base_map), Value::Object(over_map)) = (&mut *base, over) else {
        return;
    };
    for (key, incoming) in over_map {
        path.push(key.clone());
        match (base_map.get_mut(key), incoming) {
            (Some(existing @ Value::Object(_)), Value::Object(_)) => merge(existing, incoming, path),
            (Some(Value::Array(existing)), Value::Array(more)) => match array_strategy(path) {
                ArrayMerge::Concat => existing.extend(more.iter().cloned()),
                ArrayMerge::Union => {
                    for item in more {
                        if !existing.contains(item) {
                            existing.push(item.clone());
                        }
                    }
                }
                ArrayMerge::Replace => *existing = more.clone(),
            },
            _ => {
                base_map.insert(key.clone(), incoming.clone());
            }
        }
        path.pop();
    }
}

impl Settings {
    /// A dotted key such as `hooksConfig.enabled`.
    pub fn get(&self, dotted: &str) -> Option<&Value> {
        lookup(&self.merged, dotted)
    }

    pub fn bool(&self, dotted: &str, default: bool) -> bool {
        self.get(dotted).and_then(Value::as_bool).unwrap_or(default)
    }

    pub fn strings(&self, dotted: &str) -> Vec<String> {
        match self.get(dotted) {
            Some(Value::Array(items)) => items
                .iter()
                .filter_map(|item| item.as_str().map(str::to_owned))
                .collect(),
            Some(Value::String(one)) => vec![one.clone()],
            _ => Vec::new(),
        }
    }

    /// The last active layer that sets the key.
    pub fn set_in(&self, dotted: &str) -> Option<&Layer> {
        self.layers
            .iter()
            .rev()
            .filter(|layer| layer.active)
            .find(|layer| lookup(&layer.value, dotted).is_some())
    }

    pub fn layer(&self, kind: LayerKind) -> Option<&Layer> {
        self.layers.iter().find(|layer| layer.kind == kind)
    }

    pub fn errors(&self) -> impl Iterator<Item = (&AbsPath, &str)> {
        self.layers
            .iter()
            .filter_map(|layer| Some((&layer.path, layer.error.as_deref()?)))
    }
}

fn lookup<'a>(value: &'a Value, dotted: &str) -> Option<&'a Value> {
    dotted.split('.').try_fold(value, |at, key| at.get(key))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn layer(value: Value) -> Layer {
        Layer {
            kind: LayerKind::User,
            path: AbsPath::root(),
            exists: true,
            value,
            error: None,
            active: true,
        }
    }

    #[test]
    fn hooks_and_include_directories_concatenate_and_disabled_unites() {
        let a = layer(json!({
            "hooks": {"BeforeTool": [{"matcher": "a"}]},
            "hooksConfig": {"disabled": ["x", "y"]},
            "context": {"includeDirectories": ["/a"], "fileName": ["A.md"]},
        }));
        let b = layer(json!({
            "hooks": {"BeforeTool": [{"matcher": "b"}]},
            "hooksConfig": {"disabled": ["y", "z"]},
            "context": {"includeDirectories": ["/b"], "fileName": ["B.md"]},
        }));
        let merged = merge_layers([&a, &b]);
        assert_eq!(
            merged["hooks"]["BeforeTool"],
            json!([{"matcher": "a"}, {"matcher": "b"}])
        );
        assert_eq!(merged["hooksConfig"]["disabled"], json!(["x", "y", "z"]));
        assert_eq!(merged["context"]["includeDirectories"], json!(["/a", "/b"]));
        assert_eq!(merged["context"]["fileName"], json!(["B.md"]));
    }

    #[test]
    fn comments_pass_and_a_trailing_comma_is_fatal() {
        assert!(parse_settings_text("// c\n{\"a\": 1 /* x */}").is_ok());
        assert!(parse_settings_text("{\"a\": [1, 2,]}").unwrap_err().contains("trailing comma"));
        assert!(parse_settings_text("[1]").is_err());
        assert!(parse_settings_text("  ").is_err());
    }
}
