//! Hooks (spec 5): the `hooks` key of every settings layer, extension `hooks/hooks.json`, the
//! 11 events, matcher evaluation, the enable switches and a test run.

use std::sync::OnceLock;

use regex::Regex;
use serde_json::{Value, json};

use super::settings::{Layer, LayerKind};
use super::util::{gemini_home, read_lossy, rule};
use super::world::World;
use crate::agents::ScanCx;
use crate::model::*;

/// Spec 5, "Events (complete)", with the matcher each one evaluates.
const EVENTS: &[(&str, MatcherKind)] = &[
    ("BeforeTool", MatcherKind::Regex),
    ("AfterTool", MatcherKind::Regex),
    ("BeforeAgent", MatcherKind::None),
    ("AfterAgent", MatcherKind::None),
    ("BeforeModel", MatcherKind::None),
    ("AfterModel", MatcherKind::None),
    ("BeforeToolSelection", MatcherKind::None),
    ("SessionStart", MatcherKind::Exact),
    ("SessionEnd", MatcherKind::Exact),
    ("Notification", MatcherKind::None),
    ("PreCompress", MatcherKind::Exact),
];

/// Keys inside `hooks` that are legacy settings, not events (`HOOKS_CONFIG_FIELDS`).
const LEGACY_FIELDS: &[&str] = &["enabled", "disabled", "notifications"];

const DEFAULT_TIMEOUT_MS: u64 = 60_000;

static SCHEMA: OnceLock<HookSchema> = OnceLock::new();

pub(super) fn schema() -> &'static HookSchema {
    SCHEMA.get_or_init(|| {
        let field = |key: &str, ty: FieldType, required: bool| HandlerField {
            key: key.to_owned(),
            ty,
            required,
        };
        HookSchema {
            events: EVENTS
                .iter()
                .map(|(name, matcher)| HookEventSpec {
                    name: (*name).to_owned(),
                    matcher: *matcher,
                })
                .collect(),
            // Spec 5, "Handler fields". The validator also accepts `plugin` and `runtime`, which
            // JSON cannot express, so the form offers `command` only.
            handler_kinds: vec![HandlerKindSpec {
                kind: "command".to_owned(),
                fields: vec![
                    field("command", FieldType::String, true),
                    field("name", FieldType::String, false),
                    field("description", FieldType::String, false),
                    field("timeout", FieldType::Number, false),
                    field("env", FieldType::StringMap, false),
                ],
            }],
        }
    })
}

pub(super) struct Hooks {
    pub hooks: Vec<Hook>,
    pub files: Vec<HookFile>,
    pub diagnostics: Vec<Diagnostic>,
}

struct Switches {
    enabled: bool,
    enabled_in: Option<AbsPath>,
    disabled: Vec<(String, AbsPath)>,
}

fn switches(w: &World) -> Switches {
    let enabled = w.settings.bool("hooksConfig.enabled", true);
    let enabled_in = w.settings.set_in("hooksConfig.enabled").map(|l| l.path.clone());
    let mut disabled: Vec<(String, AbsPath)> = Vec::new();
    for layer in w.settings.layers.iter().filter(|l| l.active) {
        if let Some(Value::Array(names)) = layer.value.pointer("/hooksConfig/disabled") {
            for name in names.iter().filter_map(Value::as_str) {
                if !disabled.iter().any(|(n, _)| n == name) {
                    disabled.push((name.to_owned(), layer.path.clone()));
                }
            }
        }
    }
    Switches {
        enabled,
        enabled_in,
        disabled,
    }
}

pub(super) fn scan(w: &World) -> Hooks {
    let sw = switches(w);
    let mut out = Hooks {
        hooks: Vec::new(),
        files: Vec::new(),
        diagnostics: Vec::new(),
    };
    for layer in &w.settings.layers {
        let scope = layer_scope(w, layer);
        let policy = match layer.kind {
            LayerKind::User | LayerKind::Workspace => EditPolicy::Editable,
            _ => EditPolicy::ReadOnly {
                why: ReadOnlyWhy::Managed,
            },
        };
        if layer.exists || matches!(layer.kind, LayerKind::User | LayerKind::Workspace) {
            out.files.push(HookFile {
                file: layer.path.clone(),
                scope: scope.clone(),
                format: HookFormat::GeminiSettingsJsonc,
                exists: layer.exists,
                policy,
            });
        }
        // The workspace file of an untrusted folder is not merged, but its hooks are listed so
        // the operator sees what a trusted folder would run.
        let layer_hooks = layer.value.get("hooks").cloned().unwrap_or(Value::Null);
        collect(
            &sw,
            &layer.path,
            &scope,
            &layer_hooks,
            !w.trusted(),
            &mut out,
        );
    }
    for ext in w.exts.iter().filter(|e| e.active()) {
        let file = ext.root.join("hooks/hooks.json");
        let Some(text) = read_lossy(w.cx, &file) else {
            continue;
        };
        let scope = Scope::Plugin {
            id: ext.name.clone(),
        };
        out.files.push(HookFile {
            file: file.clone(),
            scope: scope.clone(),
            format: HookFormat::GeminiSettingsJsonc,
            exists: true,
            policy: if ext.linked {
                EditPolicy::Editable
            } else {
                EditPolicy::ReadOnly {
                    why: ReadOnlyWhy::PluginCache {
                        plugin: ext.name.clone(),
                    },
                }
            },
        });
        let hooks = serde_json::from_str::<Value>(&text)
            .ok()
            .and_then(|v| v.get("hooks").cloned())
            .unwrap_or(Value::Null);
        // Extension hooks are not gated by folder trust (spec 5).
        collect(&sw, &file, &scope, &hooks, false, &mut out);
    }
    out
}

fn layer_scope(w: &World, layer: &Layer) -> Scope {
    match layer.kind {
        LayerKind::SystemDefaults | LayerKind::System => Scope::Managed,
        LayerKind::User => Scope::User,
        LayerKind::Workspace => Scope::Project {
            dir: w.cwd.clone(),
        },
    }
}

/// `blocked_by_trust` is true for settings-file hooks in an untrusted folder.
fn collect(
    sw: &Switches,
    file: &AbsPath,
    scope: &Scope,
    hooks: &Value,
    blocked_by_trust: bool,
    out: &mut Hooks,
) {
    let Some(events) = hooks.as_object() else {
        return;
    };
    for (event, definitions) in events {
        if LEGACY_FIELDS.contains(&event.as_str()) {
            continue;
        }
        if !EVENTS.iter().any(|(name, _)| name == event) {
            out.diagnostics.push(Diagnostic {
                severity: Severity::Warning,
                agent: None,
                at: Subject::File {
                    path: file.clone(),
                    line: None,
                },
                problem: Problem::HookUnknownEvent {
                    event: event.clone(),
                },
                fix: None,
            });
            continue;
        }
        let Some(definitions) = definitions.as_array() else {
            continue;
        };
        for (i, definition) in definitions.iter().enumerate() {
            let Some(handlers) = definition.get("hooks").and_then(Value::as_array) else {
                continue;
            };
            let matcher_raw = definition
                .get("matcher")
                .and_then(Value::as_str)
                .map(str::to_owned);
            for (j, handler) in handlers.iter().enumerate() {
                let Some(kind) = handler.get("type").and_then(Value::as_str) else {
                    continue;
                };
                let command = handler.get("command").and_then(Value::as_str);
                let name = handler.get("name").and_then(Value::as_str);
                let valid = match kind {
                    "command" => command.is_some_and(|c| !c.is_empty()),
                    "runtime" => name.is_some_and(|n| !n.is_empty()),
                    "plugin" => true,
                    _ => false,
                };
                if !valid {
                    continue;
                }
                let pointer = format!("/hooks/{event}/{i}/hooks/{j}");
                let (eval, matcher_problem) = matcher_eval(event, matcher_raw.as_deref());
                let hook = Hook {
                    id: hook_id(file, &pointer, handler),
                    source: HookSource {
                        file: file.clone(),
                        at: Locator::Json {
                            pointer: pointer.clone(),
                        },
                        scope: scope.clone(),
                        registered: Registration::SessionStart,
                    },
                    event: event.clone(),
                    matcher: Matcher {
                        raw: matcher_raw.clone(),
                        eval,
                    },
                    handler: Handler {
                        handler_kind: kind.to_owned(),
                        raw: handler.clone(),
                        summary: command.or(name).unwrap_or_default().to_owned(),
                        timeout_ms: (kind == "command").then(|| {
                            handler
                                .get("timeout")
                                .and_then(Value::as_u64)
                                .unwrap_or(DEFAULT_TIMEOUT_MS)
                        }),
                    },
                    state: state_of(sw, kind, name.or(command), file, blocked_by_trust),
                    toggle: Toggle::Native,
                };
                if let Some(error) = matcher_problem {
                    out.diagnostics.push(Diagnostic {
                        severity: Severity::Warning,
                        agent: None,
                        at: Subject::Hook {
                            hook: hook.id.clone(),
                        },
                        problem: Problem::HookMatcherInvalid { error },
                        fix: None,
                    });
                }
                if matches!(hook.matcher.eval, MatcherEval::Ignored) {
                    out.diagnostics.push(Diagnostic {
                        severity: Severity::Info,
                        agent: None,
                        at: Subject::Hook {
                            hook: hook.id.clone(),
                        },
                        problem: Problem::HookMatcherIgnored,
                        fix: None,
                    });
                }
                out.hooks.push(hook);
            }
        }
    }
}

fn hook_id(file: &AbsPath, pointer: &str, handler: &Value) -> HookId {
    let digest = sha256_hex(format!("gemini\0{file}\0{pointer}\0{handler}").as_bytes());
    HookId(format!("gemini:{}", &digest[..24]))
}

/// Spec 5, "Execution" and "Trust": the order the CLI checks, as far as a static scan sees it.
fn state_of(
    sw: &Switches,
    kind: &str,
    key: Option<&str>,
    file: &AbsPath,
    blocked_by_trust: bool,
) -> HookState {
    if !sw.enabled {
        return HookState::Disabled {
            by: DisabledBy::AllHooksOff {
                set_in: sw.enabled_in.clone().unwrap_or_else(|| file.clone()),
            },
        };
    }
    if blocked_by_trust {
        return HookState::WillNotRun {
            why: HookBlock::FolderUntrusted,
        };
    }
    if let Some((_, set_in)) = key.and_then(|k| sw.disabled.iter().find(|(name, _)| name == k)) {
        return HookState::Disabled {
            by: DisabledBy::Native {
                set_in: set_in.clone(),
            },
        };
    }
    if kind != "command" {
        return HookState::WillNotRun {
            why: HookBlock::UnsupportedType,
        };
    }
    HookState::Active
}

/// Spec 5, "Matcher semantics". The second value is a matcher that is not a valid regex: the
/// CLI then compares the whole string.
fn matcher_eval(event: &str, raw: Option<&str>) -> (MatcherEval, Option<String>) {
    let Some(matcher) = raw.map(str::trim).filter(|m| !m.is_empty() && *m != "*") else {
        return (MatcherEval::All, None);
    };
    let kind = EVENTS.iter().find(|(name, _)| *name == event).map(|(_, k)| *k);
    match kind {
        Some(MatcherKind::Regex) => match Regex::new(matcher) {
            Ok(_) => (MatcherEval::Regex, None),
            // JavaScript accepts look-around and back-references that this engine does not.
            Err(err) if is_js_only(&err.to_string()) => (MatcherEval::Regex, None),
            Err(err) => (
                MatcherEval::Exact {
                    values: vec![matcher.to_owned()],
                },
                Some(err.to_string()),
            ),
        },
        Some(MatcherKind::Exact) => (
            MatcherEval::Exact {
                values: vec![matcher.to_owned()],
            },
            None,
        ),
        _ => (MatcherEval::Ignored, None),
    }
}

fn is_js_only(message: &str) -> bool {
    message.contains("look-around") || message.contains("backreference")
}

/// A sample hook input (spec 5; `hooks/types.ts`).
pub(super) fn sample_input(event: &str, cx: &ScanCx) -> Value {
    let cwd = cx.env.cwd.as_str();
    let transcript = gemini_home(cx).join(".gemini/tmp/project/chats/session.json");
    let mut input = json!({
        "session_id": "sample-session",
        "transcript_path": transcript.as_str(),
        "cwd": cwd,
        "hook_event_name": event,
        "timestamp": "2026-01-01T00:00:00.000Z",
    });
    let extra = match event {
        "BeforeTool" => json!({"tool_name": "write_file", "tool_input": {"file_path": "README.md", "content": "text"}}),
        "AfterTool" => json!({
            "tool_name": "write_file",
            "tool_input": {"file_path": "README.md", "content": "text"},
            "tool_response": {"llmContent": "ok", "returnDisplay": "ok"},
        }),
        "BeforeAgent" => json!({"prompt": "Describe this project."}),
        "AfterAgent" => json!({"prompt": "Describe this project.", "prompt_response": "Done.", "stop_hook_active": false}),
        "Notification" => json!({"notification_type": "ToolPermission", "message": "Allow write_file?", "details": {}}),
        "SessionStart" => json!({"source": "startup"}),
        "SessionEnd" => json!({"reason": "exit"}),
        "PreCompress" => json!({"trigger": "auto"}),
        "BeforeModel" | "BeforeToolSelection" => json!({"llm_request": llm_request()}),
        "AfterModel" => json!({"llm_request": llm_request(), "llm_response": {"text": "ok", "candidates": []}}),
        _ => json!({}),
    };
    if let (Some(base), Value::Object(extra)) = (input.as_object_mut(), extra) {
        base.extend(extra);
    }
    input
}

fn llm_request() -> Value {
    json!({"model": "gemini-2.5-pro", "messages": [{"role": "user", "content": "Describe this project."}], "config": {}})
}

/// The invocation the CLI itself would make (spec 5, "Execution"): `bash -c <command>` with the
/// five `$NAMES` replaced and shell-escaped, and the same names exported.
pub(super) fn exec_spec(
    hook: &Hook,
    input: &Value,
    cx: &ScanCx,
) -> Result<ExecSpec, RuleRef> {
    if cx.env.os == Os::Windows {
        return Err(rule("gemini.hook.windows_unsupported"));
    }
    if hook.handler.handler_kind != "command" {
        return Err(rule("gemini.hook.run_unsupported"));
    }
    let Some(command) = hook.handler.raw.get("command").and_then(Value::as_str) else {
        return Err(rule("gemini.hook.run_unsupported"));
    };
    let cwd = input
        .get("cwd")
        .and_then(Value::as_str)
        .and_then(|raw| AbsPath::parse(raw).ok())
        .unwrap_or_else(|| cx.env.cwd.clone());
    let session = input
        .get("session_id")
        .and_then(Value::as_str)
        .unwrap_or("sample-session");
    let plans = gemini_home(cx)
        .join(".gemini/tmp")
        .join(&project_slug(cx, &cwd))
        .join(session)
        .join("plans");
    let command = if let Scope::Plugin { .. } = hook.source.scope {
        let root = hook
            .source
            .file
            .parent()
            .and_then(|hooks_dir| hooks_dir.parent())
            .map_or(String::new(), |p| p.as_str().to_owned());
        command
            .replace("${extensionPath}", &root)
            .replace("${workspacePath}", cwd.as_str())
            .replace("${/}", "/")
            .replace("${pathSeparator}", "/")
    } else {
        command.to_owned()
    };
    let command = command
        .replace("$GEMINI_PROJECT_DIR", &shell_quote(cwd.as_str()))
        .replace("$GEMINI_CWD", &shell_quote(cwd.as_str()))
        .replace("$GEMINI_PLANS_DIR", &shell_quote(plans.as_str()))
        .replace("$GEMINI_SESSION_ID", &shell_quote(session))
        .replace("$CLAUDE_PROJECT_DIR", &shell_quote(cwd.as_str()));
    let mut env = vec![
        ("GEMINI_PROJECT_DIR".to_owned(), cwd.as_str().to_owned()),
        ("GEMINI_PLANS_DIR".to_owned(), plans.as_str().to_owned()),
        ("GEMINI_CWD".to_owned(), cwd.as_str().to_owned()),
        ("GEMINI_SESSION_ID".to_owned(), session.to_owned()),
        ("CLAUDE_PROJECT_DIR".to_owned(), cwd.as_str().to_owned()),
    ];
    if let Some(extra) = hook.handler.raw.get("env").and_then(Value::as_object) {
        for (key, value) in extra {
            if let Some(value) = value.as_str() {
                env.retain(|(k, _)| k != key);
                env.push((key.clone(), value.to_owned()));
            }
        }
    }
    Ok(ExecSpec {
        argv: vec!["bash".to_owned(), "-c".to_owned(), command],
        cwd,
        env,
        stdin: input.to_string(),
    })
}

/// A stand-in for `shell-quote`: plain text stays as it is, anything else is single-quoted.
fn shell_quote(arg: &str) -> String {
    if arg.is_empty() {
        return String::new();
    }
    let plain = arg
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | ',' | ':' | '/' | '@'));
    if plain {
        arg.to_owned()
    } else {
        format!("'{}'", arg.replace('\'', "'\\''"))
    }
}

/// Spec 4, slug row: the registered slug, else the one the CLI would claim.
fn project_slug(cx: &ScanCx, cwd: &AbsPath) -> String {
    let gdir = gemini_home(cx).join(".gemini");
    if let Some(slug) = super::memory::slug_in(cx, &gdir, cwd) {
        return slug;
    }
    let base = cwd.file_name().unwrap_or("").to_lowercase();
    let mut slug = String::new();
    for ch in base.chars() {
        let ch = if ch.is_ascii_lowercase() || ch.is_ascii_digit() {
            ch
        } else {
            '-'
        };
        if ch == '-' && slug.ends_with('-') {
            continue;
        }
        slug.push(ch);
    }
    let slug = slug.trim_matches('-');
    if slug.is_empty() {
        "project".to_owned()
    } else {
        slug.to_owned()
    }
}
