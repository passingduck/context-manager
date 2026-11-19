//! Hooks (spec 2): every source in discovery order, the per-hook key, the trust hash and the
//! `hooks.state` enablement.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use serde_json::{Map, Value as Json, json};
use sha2::{Digest, Sha256};

use super::out::Out;
use super::plugins::Plugin;
use super::setup::{Layer, LayerKind, Setup, get_bool, get_path, to_json};
use crate::agents::ScanCx;
use crate::model::*;
use crate::probe_fs::EntryKind;

/// Spec 2.4: the 14 event keys, in `HookEventsToml` field order, which is the order Codex lists
/// them in within one source (`into_matcher_groups`).
pub(super) const EVENTS: [&str; 14] = [
    "PreModelRequest",
    "PostModelRequest",
    "PreToolUse",
    "PermissionRequest",
    "PostToolUse",
    "PreCompact",
    "PostCompact",
    "SessionStart",
    "SessionEnd",
    "UserPromptSubmit",
    "SubagentStart",
    "SubagentStop",
    "Stop",
    "Interrupt",
];
/// Spec 2.4: the events whose matcher means something.
const MATCHER_EVENTS: [&str; 9] = [
    "PreToolUse",
    "PermissionRequest",
    "PostToolUse",
    "PreCompact",
    "PostCompact",
    "SessionStart",
    "SessionEnd",
    "SubagentStart",
    "SubagentStop",
];
const CONTEXT_LIMIT_EVENTS: [&str; 5] = [
    "PreToolUse",
    "PostToolUse",
    "SessionStart",
    "UserPromptSubmit",
    "SubagentStart",
];
/// `DEFAULT_HOOK_OUTPUT_TOKEN_LIMIT`.
const DEFAULT_CONTEXT_LIMIT: u64 = 2_500;
const DEFAULT_TIMEOUT: u64 = 600;
const SESSION_END_DEFAULT: u64 = 1;
const SESSION_END_MAX: u64 = 3;

pub(super) const RULE_PLUGIN_CONTENT: &str = "codex.assumption.plugin_hook_content";
pub(super) const RULE_TOGGLE_MANAGED: &str = "codex.hooks.toggle_managed";
pub(super) const RULE_TOGGLE_UNSUPPORTED: &str = "codex.hooks.toggle_unsupported";

pub(super) fn snake(event: &str) -> String {
    let mut out = String::new();
    for (i, ch) in event.chars().enumerate() {
        if ch.is_ascii_uppercase() && i > 0 {
            out.push('_');
        }
        out.push(ch.to_ascii_lowercase());
    }
    out
}

/// Spec 2.5: the trust status Codex reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrustStatus {
    Managed,
    Trusted,
    Modified,
    Untrusted,
}

/// What the adapter knows about one listed hook, in the terms of `hooks/list`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookInspection {
    /// `<source>:<event_snake>:<group>:<handler>`.
    pub key: String,
    pub event: String,
    pub source_path: AbsPath,
    pub enabled: bool,
    pub trust: TrustStatus,
    pub current_hash: String,
}

pub(super) struct Discovered {
    pub hook: Hook,
    pub inspection: Option<HookInspection>,
}

/// One file or table that holds hook declarations.
struct Source {
    file: AbsPath,
    key_source: String,
    format: HookFormat,
    scope: Scope,
    managed: bool,
    /// A project layer that is not trusted: listed, never run.
    folder_untrusted: bool,
    plugin: Option<(String, AbsPath)>,
    events: Map<String, Json>,
}

// ---------------------------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------------------------

/// A handler that `HookHandlerConfig` accepts, or the reason the whole file does not parse.
fn validate(events: &Map<String, Json>) -> Result<(), String> {
    for event in EVENTS {
        let Some(groups) = events.get(event) else {
            continue;
        };
        let groups = groups
            .as_array()
            .ok_or_else(|| format!("invalid type for `{event}`: expected a sequence"))?;
        for group in groups {
            let group = group
                .as_object()
                .ok_or_else(|| format!("invalid `{event}` entry: expected a map"))?;
            match group.get("matcher") {
                None | Some(Json::Null) | Some(Json::String(_)) => {}
                Some(_) => return Err("invalid type for `matcher`: expected a string".to_owned()),
            }
            let hooks = match group.get("hooks") {
                None => continue,
                Some(Json::Array(h)) => h,
                Some(_) => return Err("invalid type for `hooks`: expected a sequence".to_owned()),
            };
            for handler in hooks {
                validate_handler(handler)?;
            }
        }
    }
    Ok(())
}

fn validate_handler(handler: &Json) -> Result<(), String> {
    let obj = handler
        .as_object()
        .ok_or_else(|| "invalid hook handler: expected a map".to_owned())?;
    let kind = obj
        .get("type")
        .and_then(Json::as_str)
        .ok_or_else(|| "missing field `type`".to_owned())?;
    let uint = |key: &str| match obj.get(key) {
        None | Some(Json::Null) => Ok(()),
        Some(v) if v.is_u64() => Ok(()),
        Some(_) => Err(format!(
            "invalid type for `{key}`: expected an unsigned integer"
        )),
    };
    let string = |key: &str, required: bool| match obj.get(key) {
        None | Some(Json::Null) if !required => Ok(()),
        Some(Json::String(_)) => Ok(()),
        None | Some(Json::Null) => Err(format!("missing field `{key}`")),
        Some(_) => Err(format!("invalid type for `{key}`: expected a string")),
    };
    match kind {
        "command" => {
            string("command", true)?;
            string("commandWindows", false)?;
            string("command_windows", false)?;
            string("statusMessage", false)?;
            uint("timeout")?;
            uint("additionalContextLimit")?;
            match obj.get("async") {
                None | Some(Json::Bool(_)) => {}
                Some(_) => return Err("invalid type for `async`: expected a boolean".to_owned()),
            }
            Ok(())
        }
        "mcp_tool" => {
            string("server", true)?;
            string("tool", true)?;
            string("statusMessage", false)?;
            uint("timeout")?;
            match obj.get("input") {
                None | Some(Json::Object(_)) => Ok(()),
                Some(_) => Err("invalid type for `input`: expected a map".to_owned()),
            }
        }
        "prompt" | "agent" => Ok(()),
        other => Err(format!("unknown variant `{other}`")),
    }
}

fn read_json_hooks(cx: &ScanCx, path: &AbsPath, out: &mut Out) -> Option<Map<String, Json>> {
    let bytes = cx.fs.read(path).ok()?;
    let parse_error = |out: &mut Out, detail: String| {
        out.diagnose(
            Severity::Warning,
            Subject::File {
                path: path.clone(),
                line: None,
            },
            Problem::ParseError {
                format: Format::Json,
                detail,
                fatal_for_agent: false,
            },
        );
    };
    let value: Json = match serde_json::from_slice(&bytes) {
        Ok(v) => v,
        Err(err) => {
            parse_error(out, err.to_string());
            return None;
        }
    };
    // `HooksFile` denies unknown top-level fields (spec 2.4).
    let Some(top) = value.as_object() else {
        parse_error(out, "expected an object".to_owned());
        return None;
    };
    if let Some(key) = top.keys().find(|k| *k != "description" && *k != "hooks") {
        parse_error(out, format!("unknown field `{key}`"));
        return None;
    }
    let events = match top.get("hooks") {
        None => Map::new(),
        Some(Json::Object(events)) => events.clone(),
        Some(_) => {
            parse_error(out, "invalid type for `hooks`: expected a map".to_owned());
            return None;
        }
    };
    for key in events.keys().filter(|k| !EVENTS.contains(&k.as_str())) {
        out.diagnose(
            Severity::Warning,
            Subject::File {
                path: path.clone(),
                line: None,
            },
            Problem::HookUnknownEvent { event: key.clone() },
        );
    }
    if let Err(detail) = validate(&events) {
        parse_error(out, detail);
        return None;
    }
    (!events.is_empty()).then_some(events)
}

fn toml_hooks(layer: &Layer, out: &mut Out) -> Option<Map<String, Json>> {
    let hooks = get_path(layer.config.as_ref()?, &["hooks"])?;
    let Json::Object(table) = to_json(hooks) else {
        return None;
    };
    let mut events = Map::new();
    for (key, value) in table {
        if EVENTS.contains(&key.as_str()) {
            events.insert(key, value);
        } else if key != "state" {
            out.diagnose(
                Severity::Warning,
                Subject::File {
                    path: layer.config_path.clone(),
                    line: None,
                },
                Problem::HookUnknownEvent { event: key },
            );
        }
    }
    if let Err(detail) = validate(&events) {
        out.diagnose(
            Severity::Warning,
            Subject::File {
                path: layer.config_path.clone(),
                line: None,
            },
            Problem::ParseError {
                format: Format::Toml,
                detail,
                fatal_for_agent: false,
            },
        );
        return None;
    }
    (!events.is_empty()).then_some(events)
}

// ---------------------------------------------------------------------------------------------
// Sources in discovery order
// ---------------------------------------------------------------------------------------------

struct Policy {
    managed_only: bool,
}

fn requirements(cx: &ScanCx, out: &mut Out) -> (Option<Source>, Policy) {
    let path = AbsPath::parse(super::setup::SYSTEM_DIR)
        .expect("absolute")
        .join("requirements.toml");
    let Ok(bytes) = cx.fs.read(&path) else {
        return (
            None,
            Policy {
                managed_only: false,
            },
        );
    };
    let Ok(table) = toml::from_str::<toml::Table>(&String::from_utf8_lossy(&bytes)) else {
        return (
            None,
            Policy {
                managed_only: false,
            },
        );
    };
    let managed_only = get_bool(&table, &["allow_managed_hooks_only"]).unwrap_or(false);
    let Some(hooks) = table.get("hooks") else {
        return (None, Policy { managed_only });
    };
    let Json::Object(raw) = to_json(hooks) else {
        return (None, Policy { managed_only });
    };
    let mut events = Map::new();
    for (key, value) in raw {
        if EVENTS.contains(&key.as_str()) {
            events.insert(key, value);
        }
    }
    if validate(&events).is_err() {
        return (None, Policy { managed_only });
    }
    let _ = out;
    // Spec 2.1: `managed_dir` only names where admin scripts live. The key source is the
    // requirements file, or `managed_dir` when it is absolute.
    let key_source = table
        .get("hooks")
        .and_then(|h| h.get("managed_dir"))
        .and_then(toml::Value::as_str)
        .filter(|d| d.starts_with('/'))
        .map(str::to_owned)
        .unwrap_or_else(|| path.to_string());
    (
        (!events.is_empty()).then_some(Source {
            file: path,
            key_source,
            format: HookFormat::CodexConfigToml,
            scope: Scope::Managed,
            managed: true,
            folder_untrusted: false,
            plugin: None,
            events,
        }),
        Policy { managed_only },
    )
}

fn plugin_sources(cx: &ScanCx, plugins: &[Plugin], out: &mut Out) -> Vec<Source> {
    let mut sources = Vec::new();
    for plugin in plugins.iter().filter(|p| p.enabled) {
        // Spec 2.1: `hooks/hooks.json` by default, or the manifest `hooks` paths.
        let mut relatives: Vec<String> = Vec::new();
        match plugin.manifest.as_ref().and_then(|m| m.get("hooks")) {
            Some(Json::String(path)) => relatives.push(path.clone()),
            Some(Json::Array(items)) => {
                relatives.extend(items.iter().filter_map(|i| i.as_str().map(str::to_owned)))
            }
            Some(_) => {}
            None => relatives.push("hooks/hooks.json".to_owned()),
        }
        for rel in relatives {
            let clean = rel.trim_start_matches("./").to_owned();
            let path = plugin.root.join(&clean);
            if cx.fs.stat(&path).is_err() {
                continue;
            }
            let Some(events) = read_json_hooks(cx, &path, out) else {
                continue;
            };
            sources.push(Source {
                key_source: format!("{}:{clean}", plugin.id),
                file: path,
                format: HookFormat::CodexHooksJson,
                scope: Scope::Plugin {
                    id: plugin.id.clone(),
                },
                managed: false,
                folder_untrusted: false,
                plugin: Some((plugin.id.clone(), plugin.root.clone())),
                events,
            });
        }
    }
    sources
}

fn layer_sources(cx: &ScanCx, setup: &Setup, out: &mut Out) -> Vec<Source> {
    let mut sources = Vec::new();
    let mut visited: Vec<AbsPath> = Vec::new();
    for layer in &setup.layers {
        let managed = layer.is_managed();
        let folder_untrusted = !layer.enabled;
        let json_path = layer.hooks_folder.join("hooks.json");
        // Spec 2.1: `hooks.json` is read once per folder. Legacy managed config has none.
        if layer.kind != LayerKind::LegacyManaged && !visited.contains(&layer.hooks_folder) {
            visited.push(layer.hooks_folder.clone());
            if cx.fs.stat(&json_path).is_ok()
                && let Some(events) = read_json_hooks(cx, &json_path, out)
            {
                sources.push(Source {
                    key_source: json_path.to_string(),
                    file: json_path.clone(),
                    format: HookFormat::CodexHooksJson,
                    scope: layer.scope(),
                    managed,
                    folder_untrusted,
                    plugin: None,
                    events,
                });
            }
        }
        let toml_path = layer.hooks_folder.join("config.toml");
        let toml_file = if layer.hooks_folder == layer.folder {
            layer.config_path.clone()
        } else {
            toml_path
        };
        let config_layer = if layer.hooks_folder == layer.folder {
            layer.clone()
        } else {
            // A worktree reads the root checkout's `[hooks]` (spec 2.1).
            let mut other = layer.clone();
            other.config = cx
                .fs
                .read(&toml_file)
                .ok()
                .and_then(|b| toml::from_str(&String::from_utf8_lossy(&b)).ok());
            other.config_path = toml_file.clone();
            other
        };
        if let Some(events) = toml_hooks(&config_layer, out) {
            sources.push(Source {
                key_source: toml_file.to_string(),
                file: toml_file,
                format: HookFormat::CodexConfigToml,
                scope: layer.scope(),
                managed,
                folder_untrusted,
                plugin: None,
                events,
            });
        }
    }
    sources
}

// ---------------------------------------------------------------------------------------------
// Hash (spec 2.5)
// ---------------------------------------------------------------------------------------------

fn sorted(value: &Json) -> Json {
    match value {
        Json::Object(map) => {
            let ordered: BTreeMap<_, _> = map.iter().map(|(k, v)| (k.clone(), sorted(v))).collect();
            Json::Object(ordered.into_iter().collect())
        }
        Json::Array(items) => Json::Array(items.iter().map(sorted).collect()),
        other => other.clone(),
    }
}

/// `hook_hash`: SHA-256 over the key-sorted JSON of the normalized identity.
pub(super) fn declaration_hash(event: &str, matcher: Option<&str>, handler: &Json) -> String {
    let mut identity = Map::new();
    identity.insert("event_name".to_owned(), Json::String(snake(event)));
    if let Some(matcher) = matcher {
        identity.insert("matcher".to_owned(), Json::String(matcher.to_owned()));
    }
    identity.insert("hooks".to_owned(), Json::Array(vec![handler.clone()]));
    let bytes = serde_json::to_vec(&sorted(&Json::Object(identity))).unwrap_or_default();
    format!("sha256:{}", sha256_hex(&bytes))
}

/// The installed binary builds `serde_json` with `arbitrary_precision`, so a number inside an
/// MCP hook's `input` reaches the hash as `{"$serde_json::private::Number": "<text>"}`. Found
/// by comparing with `hooks/list` (probe: `input = {"n": 3}`).
fn number_objects(value: &Json) -> Json {
    match value {
        Json::Number(n) => json!({ "$serde_json::private::Number": n.to_string() }),
        Json::Array(items) => Json::Array(items.iter().map(number_objects).collect()),
        Json::Object(map) => Json::Object(
            map.iter()
                .map(|(k, v)| (k.clone(), number_objects(v)))
                .collect(),
        ),
        other => other.clone(),
    }
}

/// The normalized handler of spec 2.5: the timeout filled in, `commandWindows` dropped.
fn normalize(event: &str, handler: &Json) -> Option<Json> {
    let obj = handler.as_object()?;
    let timeout = |default: u64, session_like: bool| {
        let raw = obj.get("timeout").and_then(Json::as_u64);
        if session_like {
            raw.unwrap_or(SESSION_END_DEFAULT).clamp(1, SESSION_END_MAX)
        } else {
            raw.unwrap_or(default).max(1)
        }
    };
    let session_like = matches!(event, "SessionEnd" | "Interrupt");
    match obj.get("type")?.as_str()? {
        "command" => {
            let mut out = Map::new();
            out.insert("type".into(), json!("command"));
            out.insert("command".into(), obj.get("command")?.clone());
            out.insert(
                "timeout".into(),
                json!(timeout(DEFAULT_TIMEOUT, session_like)),
            );
            out.insert(
                "async".into(),
                json!(obj.get("async").and_then(Json::as_bool).unwrap_or(false)),
            );
            if let Some(message) = obj.get("statusMessage").filter(|v| v.is_string()) {
                out.insert("statusMessage".into(), message.clone());
            }
            let limit = CONTEXT_LIMIT_EVENTS
                .contains(&event)
                .then(|| obj.get("additionalContextLimit").and_then(Json::as_u64))
                .flatten()
                .filter(|l| *l != DEFAULT_CONTEXT_LIMIT);
            if let Some(limit) = limit {
                out.insert("additionalContextLimit".into(), json!(limit));
            }
            Some(Json::Object(out))
        }
        "mcp_tool" => {
            let mut out = Map::new();
            out.insert("type".into(), json!("mcp_tool"));
            out.insert("server".into(), obj.get("server")?.clone());
            out.insert("tool".into(), obj.get("tool")?.clone());
            out.insert(
                "input".into(),
                number_objects(obj.get("input").unwrap_or(&json!({}))),
            );
            out.insert(
                "timeout".into(),
                json!(timeout(DEFAULT_TIMEOUT, session_like)),
            );
            if let Some(message) = obj.get("statusMessage").filter(|v| v.is_string()) {
                out.insert("statusMessage".into(), message.clone());
            }
            Some(Json::Object(out))
        }
        _ => None,
    }
}

/// The current hash of a hook, without the plugin content part (spec 2.5). Used by the guard
/// and by tests that compare with the oracle's `currentHash`.
pub fn current_hash(hook: &Hook) -> Option<String> {
    let matcher = effective_matcher(&hook.event, hook.matcher.raw.as_deref());
    let handler = normalize(&hook.event, &hook.handler.raw)?;
    Some(declaration_hash(&hook.event, matcher, &handler))
}

/// `matcher_pattern_for_event`: the five events without a matcher drop it.
fn effective_matcher<'a>(event: &str, raw: Option<&'a str>) -> Option<&'a str> {
    MATCHER_EVENTS.contains(&event).then_some(raw).flatten()
}

fn eval_matcher(event: &str, raw: Option<&str>) -> MatcherEval {
    if !MATCHER_EVENTS.contains(&event) {
        return MatcherEval::Ignored;
    }
    match raw {
        None | Some("") | Some("*") => MatcherEval::All,
        Some(p)
            if p.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '|') =>
        {
            MatcherEval::Exact {
                values: p.split('|').map(str::to_owned).collect(),
            }
        }
        Some(p) => match regex::Regex::new(p) {
            Ok(_) => MatcherEval::Regex,
            Err(err) => MatcherEval::Invalid {
                error: err.to_string(),
            },
        },
    }
}

/// `plugin_content_hash`, over the files of the plugin directory.
fn plugin_content_hash(cx: &ScanCx, root: &AbsPath, max_entries: usize) -> Option<String> {
    let mut hash = Sha256::new();
    hash.update(b"codex-plugin-bundle-v1");
    let mut pending = vec![root.clone()];
    let mut seen = 0usize;
    while let Some(path) = pending.pop() {
        seen += 1;
        if seen > max_entries {
            return None;
        }
        let name = path.file_name().unwrap_or("");
        let relative = &path.as_str()[root.as_str().len()..];
        let relative = relative.trim_start_matches('/');
        if path != *root
            && (matches!(name, "__pycache__" | ".venv")
                || name.ends_with(".pyc")
                || name.ends_with(".egg-info"))
        {
            continue;
        }
        hash.update((relative.len() as u64).to_le_bytes());
        hash.update(relative.as_bytes());
        let stat = cx.fs.stat(&path).ok()?;
        match stat.kind {
            EntryKind::Dir => {
                hash.update(b"directory");
                let list = cx.fs.list(&path).ok()?;
                for entry in list.iter().rev() {
                    pending.push(path.join(&entry.name));
                }
            }
            EntryKind::File => {
                hash.update(b"file");
                let bytes = cx.fs.read(&path).ok()?;
                hash.update((bytes.len() as u64).to_le_bytes());
                hash.update(&bytes[..]);
            }
            EntryKind::Other => return None,
        }
    }
    Some(format!("sha256:{}", crate::model::hex(&hash.finalize())))
}

// ---------------------------------------------------------------------------------------------
// Discovery
// ---------------------------------------------------------------------------------------------

#[derive(Default, Clone)]
struct State {
    enabled: Option<bool>,
    trusted_hash: Option<String>,
    trusted_content_hash: Option<String>,
}

/// Spec 2.5: `[hooks.state."<key>"]`, read only from the user layer.
fn states(setup: &Setup) -> BTreeMap<String, State> {
    let mut out = BTreeMap::new();
    let Some(table) = setup
        .user_layer()
        .config
        .as_ref()
        .and_then(|c| get_path(c, &["hooks", "state"]))
        .and_then(toml::Value::as_table)
    else {
        return out;
    };
    for (key, value) in table {
        out.insert(
            key.clone(),
            State {
                enabled: value.get("enabled").and_then(toml::Value::as_bool),
                trusted_hash: value
                    .get("trusted_hash")
                    .and_then(toml::Value::as_str)
                    .map(str::to_owned),
                trusted_content_hash: value
                    .get("trusted_content_hash")
                    .and_then(toml::Value::as_str)
                    .map(str::to_owned),
            },
        );
    }
    out
}

pub(super) fn discover(
    cx: &ScanCx,
    setup: &Setup,
    plugins: &[Plugin],
    full_hash: bool,
    out: &mut Out,
) -> Vec<Discovered> {
    let (requirement, policy) = requirements(cx, out);
    let mut sources: Vec<Source> = requirement.into_iter().collect();
    sources.extend(layer_sources(cx, setup, out));
    sources.extend(plugin_sources(cx, plugins, out));
    let states = states(setup);
    let mut content_hashes: BTreeMap<AbsPath, Option<String>> = BTreeMap::new();
    let mut found = Vec::new();

    for source in &sources {
        for event in EVENTS {
            let Some(groups) = source.events.get(event).and_then(Json::as_array) else {
                continue;
            };
            for (g, group) in groups.iter().enumerate() {
                let matcher_raw = group.get("matcher").and_then(Json::as_str);
                let eval = eval_matcher(event, matcher_raw);
                let Some(handlers) = group.get("hooks").and_then(Json::as_array) else {
                    continue;
                };
                for (h, handler) in handlers.iter().enumerate() {
                    let hook = build(
                        cx,
                        setup,
                        source,
                        &policy,
                        &states,
                        &mut content_hashes,
                        full_hash,
                        Position {
                            event,
                            group: g,
                            handler: h,
                            matcher_raw,
                            eval: &eval,
                        },
                        handler,
                        out,
                    );
                    found.push(hook);
                }
            }
        }
    }
    found
}

struct Position<'a> {
    event: &'a str,
    group: usize,
    handler: usize,
    matcher_raw: Option<&'a str>,
    eval: &'a MatcherEval,
}

#[allow(clippy::too_many_arguments)]
fn build(
    cx: &ScanCx,
    setup: &Setup,
    source: &Source,
    policy: &Policy,
    states: &BTreeMap<String, State>,
    content_hashes: &mut BTreeMap<AbsPath, Option<String>>,
    full_hash: bool,
    at: Position,
    handler: &Json,
    out: &mut Out,
) -> Discovered {
    let key = format!(
        "{}:{}:{}:{}",
        source.key_source,
        snake(at.event),
        at.group,
        at.handler
    );
    let kind = handler
        .get("type")
        .and_then(Json::as_str)
        .unwrap_or("command")
        .to_owned();
    let locator = match source.format {
        HookFormat::CodexHooksJson => Locator::Json {
            pointer: format!("/hooks/{}/{}/hooks/{}", at.event, at.group, at.handler),
        },
        _ => Locator::Toml {
            path: vec![
                TomlSeg::Key {
                    name: "hooks".to_owned(),
                },
                TomlSeg::Key {
                    name: at.event.to_owned(),
                },
                TomlSeg::Index {
                    index: at.group as u32,
                },
                TomlSeg::Key {
                    name: "hooks".to_owned(),
                },
                TomlSeg::Index {
                    index: at.handler as u32,
                },
            ],
        },
    };
    let summary = match kind.as_str() {
        "command" => handler
            .get("command")
            .and_then(Json::as_str)
            .unwrap_or("")
            .to_owned(),
        "mcp_tool" => format!(
            "{}/{}",
            handler.get("server").and_then(Json::as_str).unwrap_or(""),
            handler.get("tool").and_then(Json::as_str).unwrap_or("")
        ),
        other => other.to_owned(),
    };
    let normalized = normalize(at.event, handler);
    let timeout_ms = normalized
        .as_ref()
        .and_then(|n| n.get("timeout"))
        .and_then(Json::as_u64)
        .map(|s| s * 1000);

    let id = HookId(sha256_hex(
        format!("codex\0{}\0{:?}\0{}", source.file, locator, handler).as_bytes(),
    ));

    // What Codex lists: command and mcp_tool handlers with a valid matcher, non-empty fields.
    let blank = match kind.as_str() {
        "command" => summary.trim().is_empty(),
        "mcp_tool" => {
            handler
                .get("server")
                .and_then(Json::as_str)
                .is_none_or(|s| s.trim().is_empty())
                || handler
                    .get("tool")
                    .and_then(Json::as_str)
                    .is_none_or(|s| s.trim().is_empty())
        }
        _ => false,
    };
    let session_end_mcp = kind == "mcp_tool" && at.event == "SessionEnd";
    let invalid_matcher = matches!(at.eval, MatcherEval::Invalid { .. });
    let unsupported = normalized.is_none() || blank || session_end_mcp || invalid_matcher;

    if let MatcherEval::Invalid { error } = at.eval
        && at.handler == 0
    {
        out.diagnose(
            Severity::Warning,
            Subject::Hook { hook: id.clone() },
            Problem::HookMatcherInvalid {
                error: error.clone(),
            },
        );
    }
    if matches!(at.eval, MatcherEval::Ignored) && at.matcher_raw.is_some() && at.handler == 0 {
        out.diagnose(
            Severity::Info,
            Subject::Hook { hook: id.clone() },
            Problem::HookMatcherIgnored,
        );
    }

    let mut inspection = None;
    let state = if unsupported {
        WillNot(HookBlock::UnsupportedType)
    } else {
        let normalized = normalized.as_ref().expect("checked above");
        let matcher = effective_matcher(at.event, at.matcher_raw);
        let declaration = declaration_hash(at.event, matcher, normalized);
        let user_state = states.get(&key);
        let mut current = declaration.clone();
        let mut trust;
        if source.managed {
            trust = TrustStatus::Managed;
        } else {
            let trusted_hash = user_state.and_then(|s| s.trusted_hash.as_deref());
            trust = match trusted_hash {
                Some(t) if t == current => TrustStatus::Trusted,
                Some(_) => TrustStatus::Modified,
                None => TrustStatus::Untrusted,
            };
            if let Some((plugin_id, root)) = &source.plugin {
                // The content hash reads the whole plugin, so a scan computes it only for
                // hooks that have an approval to check. `inspect` always computes it.
                // A plugin with many files (an installed `node_modules`) is not hashed in a scan.
                // The trust then rests on the declaration hash alone, which is an assumption.
                let max_entries = if full_hash { 200_000 } else { 5_000 };
                let content = if full_hash || trusted_hash.is_some() {
                    content_hashes
                        .entry(root.clone())
                        .or_insert_with(|| plugin_content_hash(cx, root, max_entries))
                        .clone()
                } else {
                    None
                };
                if let Some(content) = &content {
                    current = format!("{declaration}/{content}");
                }
                if let (Some(trusted), Some(content)) = (trusted_hash, &content) {
                    trust = plugin_trust(
                        cx,
                        setup,
                        plugin_id,
                        &declaration,
                        trusted,
                        user_state.and_then(|s| s.trusted_content_hash.as_deref()),
                        content,
                    );
                }
            }
        }
        let enabled = source.managed || user_state.and_then(|s| s.enabled) != Some(false);
        inspection = Some(HookInspection {
            key: key.clone(),
            event: at.event.to_owned(),
            source_path: source.file.clone(),
            enabled,
            trust,
            current_hash: current,
        });
        if !setup.features.hooks {
            WillNot(HookBlock::FeatureOff)
        } else if policy.managed_only && !source.managed {
            WillNot(HookBlock::ManagedOnly)
        } else if source.folder_untrusted {
            // Not listed by Codex: the layer is disabled (spec 6.3).
            inspection = None;
            WillNot(HookBlock::FolderUntrusted)
        } else if !enabled {
            HookState::Disabled {
                by: DisabledBy::Native {
                    set_in: setup.user_config.clone(),
                },
            }
        } else {
            match trust {
                TrustStatus::Managed | TrustStatus::Trusted => HookState::Active,
                TrustStatus::Modified => WillNot(HookBlock::Modified),
                TrustStatus::Untrusted => WillNot(HookBlock::NeedsTrust),
            }
        }
    };

    if let HookState::WillNotRun { why } = &state
        && !matches!(why, HookBlock::UnsupportedType | HookBlock::FolderUntrusted)
    {
        out.diagnose(
            Severity::Warning,
            Subject::Hook { hook: id.clone() },
            Problem::HookWillNotRun { why: *why },
        );
    }

    let toggle = if source.managed {
        Toggle::Unavailable {
            why: RuleRef::new(RULE_TOGGLE_MANAGED),
        }
    } else if unsupported {
        Toggle::Unavailable {
            why: RuleRef::new(RULE_TOGGLE_UNSUPPORTED),
        }
    } else {
        Toggle::Native
    };

    let hook = Hook {
        id,
        source: HookSource {
            file: source.file.clone(),
            at: locator,
            scope: source.scope.clone(),
            registered: Registration::SessionStart,
        },
        event: at.event.to_owned(),
        matcher: Matcher {
            raw: at.matcher_raw.map(str::to_owned),
            eval: at.eval.clone(),
        },
        handler: Handler {
            handler_kind: kind,
            raw: handler.clone(),
            summary,
            timeout_ms,
        },
        state,
        toggle,
    };
    Discovered { hook, inspection }
}

#[allow(non_snake_case)]
fn WillNot(why: HookBlock) -> HookState {
    HookState::WillNotRun { why }
}

/// `PluginHookTrust::matches_hash` (spec 2.5, plugin hooks).
fn plugin_trust(
    cx: &ScanCx,
    setup: &Setup,
    plugin_id: &str,
    declaration: &str,
    trusted_hash: &str,
    trusted_content: Option<&str>,
    content: &str,
) -> TrustStatus {
    if trusted_hash != declaration {
        return TrustStatus::Modified;
    }
    if let Some(expected) = trusted_content {
        return if expected == content {
            TrustStatus::Trusted
        } else {
            TrustStatus::Modified
        };
    }
    // Legacy approval: a baseline file under `.hook-trust`, keyed by the config file and the
    // plugin id. A missing baseline is created on first use, so it matches.
    let mut identity = Sha256::new();
    for part in [setup.user_config.as_str().as_bytes(), plugin_id.as_bytes()] {
        identity.update((part.len() as u64).to_le_bytes());
        identity.update(part);
    }
    let baseline = setup
        .codex_home
        .join(".hook-trust")
        .join(&crate::model::hex(&identity.finalize()));
    match cx.fs.read(&baseline) {
        Ok(bytes) if String::from_utf8_lossy(&bytes) == content => TrustStatus::Trusted,
        Ok(_) => TrustStatus::Modified,
        Err(_) => TrustStatus::Untrusted,
    }
}

// ---------------------------------------------------------------------------------------------
// Schema and files
// ---------------------------------------------------------------------------------------------

pub(super) fn schema() -> &'static HookSchema {
    static SCHEMA: OnceLock<HookSchema> = OnceLock::new();
    SCHEMA.get_or_init(|| {
        let field = |key: &str, ty: FieldType, required: bool| HandlerField {
            key: key.to_owned(),
            ty,
            required,
        };
        HookSchema {
            events: EVENTS
                .iter()
                .map(|name| HookEventSpec {
                    name: (*name).to_owned(),
                    // Spec 2.4: exact when only `[A-Za-z0-9_|]`, else a regex.
                    matcher: if MATCHER_EVENTS.contains(name) {
                        MatcherKind::ExactOrRegex
                    } else {
                        MatcherKind::None
                    },
                })
                .collect(),
            handler_kinds: vec![
                HandlerKindSpec {
                    kind: "command".to_owned(),
                    fields: vec![
                        field("command", FieldType::String, true),
                        field("commandWindows", FieldType::String, false),
                        field("timeout", FieldType::Number, false),
                        field("async", FieldType::Bool, false),
                        field("statusMessage", FieldType::String, false),
                        field("additionalContextLimit", FieldType::Number, false),
                    ],
                },
                HandlerKindSpec {
                    kind: "mcp_tool".to_owned(),
                    fields: vec![
                        field("server", FieldType::String, true),
                        field("tool", FieldType::String, true),
                        field("input", FieldType::Json, false),
                        field("timeout", FieldType::Number, false),
                        field("statusMessage", FieldType::String, false),
                    ],
                },
                // Spec 2.4: parsed, then skipped with a warning.
                HandlerKindSpec {
                    kind: "prompt".to_owned(),
                    fields: Vec::new(),
                },
                HandlerKindSpec {
                    kind: "agent".to_owned(),
                    fields: Vec::new(),
                },
            ],
        }
    })
}

/// Files a new hook may be added to (spec 2.1, 7.2).
pub(super) fn hook_files(cx: &ScanCx, setup: &Setup, out: &mut Out) {
    let exists = |p: &AbsPath| {
        cx.fs
            .stat(p)
            .map(|s| s.kind == EntryKind::File)
            .unwrap_or(false)
    };
    let mut files: Vec<HookFile> = Vec::new();
    let mut add = |file: AbsPath, scope: Scope, format: HookFormat, policy: EditPolicy| {
        let exists = exists(&file);
        files.push(HookFile {
            file,
            scope,
            format,
            exists,
            policy,
        });
    };
    for layer in &setup.layers {
        let editable = match layer.kind {
            LayerKind::System | LayerKind::LegacyManaged => EditPolicy::ReadOnly {
                why: ReadOnlyWhy::Managed,
            },
            _ => EditPolicy::Editable,
        };
        if layer.is_managed() {
            for (name, format) in [
                ("hooks.json", HookFormat::CodexHooksJson),
                ("config.toml", HookFormat::CodexConfigToml),
            ] {
                let path = layer.folder.join(name);
                if layer.kind == LayerKind::System && exists(&path) {
                    add(path, layer.scope(), format, editable.clone());
                }
            }
            continue;
        }
        add(
            layer.hooks_folder.join("hooks.json"),
            layer.scope(),
            HookFormat::CodexHooksJson,
            editable.clone(),
        );
        add(
            layer.config_path.clone(),
            layer.scope(),
            HookFormat::CodexConfigToml,
            editable,
        );
    }
    // A project hook file the walk has not made a layer yet: cwd's `.codex/`.
    let cwd_dot = setup.cwd.join(".codex");
    if !setup.layers.iter().any(|l| l.folder == cwd_dot) {
        let scope = Scope::Project {
            dir: setup.cwd.clone(),
        };
        add(
            cwd_dot.join("hooks.json"),
            scope.clone(),
            HookFormat::CodexHooksJson,
            EditPolicy::Editable,
        );
        add(
            cwd_dot.join("config.toml"),
            scope,
            HookFormat::CodexConfigToml,
            EditPolicy::Editable,
        );
    }
    out.hook_files.extend(files);
}

pub(super) fn run(cx: &ScanCx, setup: &Setup, plugins: &[Plugin], out: &mut Out) {
    for found in discover(cx, setup, plugins, false, out) {
        let policy = match &found.hook.source.scope {
            Scope::Plugin { id } => EditPolicy::ReadOnly {
                why: ReadOnlyWhy::PluginCache { plugin: id.clone() },
            },
            Scope::Managed => EditPolicy::ReadOnly {
                why: ReadOnlyWhy::Managed,
            },
            _ => EditPolicy::Editable,
        };
        out.claim(&found.hook.source.file, policy);
        if matches!(found.hook.source.scope, Scope::Plugin { .. })
            && found.hook.state == HookState::Active
            && !out
                .assumptions
                .iter()
                .any(|a| matches!(a, Assumption::Unknown { what } if what.0 == RULE_PLUGIN_CONTENT))
        {
            out.assumptions.push(Assumption::Unknown {
                what: RuleRef::new(RULE_PLUGIN_CONTENT),
            });
        }
        out.hooks.push(found.hook);
    }
    hook_files(cx, setup, out);
    for file in out.hook_files.clone() {
        out.claim(&file.file, file.policy.clone());
    }
}

/// Every listed hook with its key, enabled flag, trust status and current hash, as
/// `hooks/list` reports them for the scan's cwd.
pub fn inspect(cx: &ScanCx) -> Vec<HookInspection> {
    let setup = Setup::load(cx);
    let plugins = super::plugins::installed(cx, &setup);
    let mut out = Out::default();
    discover(cx, &setup, &plugins, true, &mut out)
        .into_iter()
        .filter_map(|d| d.inspection)
        .collect()
}
