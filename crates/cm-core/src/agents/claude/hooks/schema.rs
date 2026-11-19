//! The 33 events, their matcher fields and handler support (spec 3, "Events (33)" and
//! "Handler types and fields"), and the sample input for each event.

use std::sync::OnceLock;

use serde_json::{Value, json};

use crate::agents::ScanCx;
use crate::agents::claude::setup::Setup;
use crate::model::*;

/// Spec 3, Events: which handler types an event accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Handlers {
    /// command, http, mcp_tool, prompt, agent.
    All,
    /// command, http, mcp_tool.
    Chm,
    CommandOnly,
    /// command, mcp_tool.
    SessionStart,
    /// command, http, mcp_tool, prompt.
    PermissionRequest,
}

impl Handlers {
    pub fn supports(self, kind: &str) -> bool {
        match (self, kind) {
            (_, "command") => true,
            (Handlers::CommandOnly, _) => false,
            (Handlers::SessionStart, k) => k == "mcp_tool",
            (Handlers::Chm, k) => matches!(k, "http" | "mcp_tool"),
            (Handlers::PermissionRequest, k) => matches!(k, "http" | "mcp_tool" | "prompt"),
            (Handlers::All, k) => matches!(k, "http" | "mcp_tool" | "prompt" | "agent"),
        }
    }
}

pub(super) struct Event {
    pub name: &'static str,
    /// The input field the matcher is tested against. `None`: the event has no matcher.
    pub field: Option<&'static str>,
    /// The closed set of values the field takes, when the sources agree on one. Empty: open.
    /// `Notification` and `StopFailure` stay open: the binary lists values the docs do not
    /// (spec 3, UNVERIFIED).
    pub values: &'static [&'static str],
    pub handlers: Handlers,
}

const fn ev(
    name: &'static str,
    field: Option<&'static str>,
    values: &'static [&'static str],
    handlers: Handlers,
) -> Event {
    Event {
        name,
        field,
        values,
        handlers,
    }
}

/// Spec 3, Events (33). The docs table and the binary's event array match.
pub(super) static EVENTS: [Event; 33] = [
    ev(
        "SessionStart",
        Some("source"),
        &["startup", "resume", "clear", "compact", "fork"],
        Handlers::SessionStart,
    ),
    ev(
        "Setup",
        Some("trigger"),
        &["init", "maintenance"],
        Handlers::CommandOnly,
    ),
    ev("UserPromptSubmit", None, &[], Handlers::All),
    ev(
        "UserPromptExpansion",
        Some("command_name"),
        &[],
        Handlers::All,
    ),
    ev("PreToolUse", Some("tool_name"), &[], Handlers::All),
    ev(
        "PermissionRequest",
        Some("tool_name"),
        &[],
        Handlers::PermissionRequest,
    ),
    ev("PermissionDenied", Some("tool_name"), &[], Handlers::All),
    ev("PostToolUse", Some("tool_name"), &[], Handlers::All),
    ev("PostToolUseFailure", Some("tool_name"), &[], Handlers::All),
    ev("PostToolBatch", None, &[], Handlers::All),
    ev(
        "Notification",
        Some("notification_type"),
        &[],
        Handlers::Chm,
    ),
    ev("MessageDisplay", None, &[], Handlers::Chm),
    ev("SubagentStart", Some("agent_type"), &[], Handlers::Chm),
    ev("SubagentStop", Some("agent_type"), &[], Handlers::All),
    ev("TaskCreated", None, &[], Handlers::All),
    ev("TaskCompleted", None, &[], Handlers::All),
    ev("Stop", None, &[], Handlers::All),
    ev("StopFailure", Some("error"), &[], Handlers::Chm),
    ev("TeammateIdle", None, &[], Handlers::All),
    ev(
        "InstructionsLoaded",
        Some("load_reason"),
        &[
            "session_start",
            "nested_traversal",
            "path_glob_match",
            "include",
            "compact",
        ],
        Handlers::Chm,
    ),
    ev(
        "ConfigChange",
        Some("source"),
        &[
            "user_settings",
            "project_settings",
            "local_settings",
            "policy_settings",
            "skills",
        ],
        Handlers::Chm,
    ),
    ev("CwdChanged", None, &[], Handlers::Chm),
    ev(
        "DirectoryAdded",
        Some("source"),
        &["slash_command", "register_repo_root"],
        Handlers::Chm,
    ),
    ev("FileChanged", Some("file_path"), &[], Handlers::Chm),
    ev("WorktreeCreate", None, &[], Handlers::Chm),
    ev("WorktreeRemove", None, &[], Handlers::Chm),
    ev(
        "PreCompact",
        Some("trigger"),
        &["manual", "auto"],
        Handlers::Chm,
    ),
    ev(
        "PostCompact",
        Some("trigger"),
        &["manual", "auto"],
        Handlers::Chm,
    ),
    ev("PreModelSwitch", Some("to_model"), &[], Handlers::Chm),
    ev("PostModelSwitch", Some("to_model"), &[], Handlers::Chm),
    ev("Elicitation", Some("mcp_server_name"), &[], Handlers::Chm),
    ev(
        "ElicitationResult",
        Some("mcp_server_name"),
        &[],
        Handlers::Chm,
    ),
    ev(
        "SessionEnd",
        Some("reason"),
        &["clear", "resume", "logout", "prompt_input_exit", "other"],
        Handlers::Chm,
    ),
];

pub(super) fn event(name: &str) -> Option<&'static Event> {
    EVENTS.iter().find(|e| e.name == name)
}

/// Spec 3, Common fields: `if` runs only on these events.
pub(super) fn event_takes_if(name: &str) -> bool {
    matches!(
        name,
        "PreToolUse"
            | "PostToolUse"
            | "PostToolUseFailure"
            | "PermissionRequest"
            | "PermissionDenied"
    )
}

fn field(key: &str, ty: FieldType, required: bool) -> HandlerField {
    HandlerField {
        key: key.to_owned(),
        ty,
        required,
    }
}

fn kind(name: &str, own: Vec<HandlerField>) -> HandlerKindSpec {
    // Spec 3, Common fields (all five types).
    let mut fields = vec![
        field("if", FieldType::String, false),
        field("timeout", FieldType::Number, false),
        field("statusMessage", FieldType::String, false),
        field("once", FieldType::Bool, false),
    ];
    fields.extend(own);
    HandlerKindSpec {
        kind: name.to_owned(),
        fields,
    }
}

/// Spec 3, Handler types and fields. The `@internal` fields (`rewakeMessage`, `rewakeSummary`,
/// `cloud`) are kept on edit and not offered.
pub(super) fn schema() -> &'static HookSchema {
    static SCHEMA: OnceLock<HookSchema> = OnceLock::new();
    SCHEMA.get_or_init(|| HookSchema {
        events: EVENTS
            .iter()
            .map(|e| HookEventSpec {
                name: e.name.to_owned(),
                matcher: if e.field.is_some() {
                    MatcherKind::ExactOrRegex
                } else {
                    MatcherKind::None
                },
            })
            .collect(),
        handler_kinds: vec![
            kind(
                "command",
                vec![
                    field("command", FieldType::String, true),
                    field("args", FieldType::StringList, false),
                    field("shell", FieldType::String, false),
                    field("async", FieldType::Bool, false),
                    field("asyncRewake", FieldType::Bool, false),
                    field("onFailure", FieldType::String, false),
                ],
            ),
            kind(
                "http",
                vec![
                    field("url", FieldType::String, true),
                    field("headers", FieldType::StringMap, false),
                    field("allowedEnvVars", FieldType::StringList, false),
                    field("onFailure", FieldType::String, false),
                ],
            ),
            kind(
                "mcp_tool",
                vec![
                    field("server", FieldType::String, true),
                    field("tool", FieldType::String, true),
                    field("input", FieldType::Json, false),
                    field("onFailure", FieldType::String, false),
                ],
            ),
            kind(
                "prompt",
                vec![
                    field("prompt", FieldType::String, true),
                    field("model", FieldType::String, false),
                    field("continueOnBlock", FieldType::Bool, false),
                ],
            ),
            kind(
                "agent",
                vec![
                    field("prompt", FieldType::String, true),
                    field("model", FieldType::String, false),
                ],
            ),
        ],
    })
}

/// Hook input (spec 3, "Path placeholders and hook environment" names the common fields).
/// Event fields follow the matcher field of the events table.
pub(super) fn sample_input(event_name: &str, cx: &ScanCx) -> Value {
    let setup = Setup::load(cx);
    let mut input = json!({
        "session_id": "00000000-0000-4000-8000-000000000000",
        "transcript_path": setup.config.join("projects/sample/sample-session.jsonl").as_str(),
        "cwd": cx.env.cwd.as_str(),
        "permission_mode": "default",
        "hook_event_name": event_name,
    });
    let extra = match event_name {
        "SessionStart" => json!({"source": "startup"}),
        "Setup" => json!({"trigger": "init"}),
        "UserPromptSubmit" => json!({"prompt": "Write a hello world"}),
        "UserPromptExpansion" => json!({"command_name": "review", "prompt": "/review"}),
        "PreToolUse" | "PermissionRequest" | "PermissionDenied" => json!({
            "tool_name": "Bash",
            "tool_input": {"command": "ls -la"},
            "tool_use_id": "toolu_sample"
        }),
        "PostToolUse" => json!({
            "tool_name": "Bash",
            "tool_input": {"command": "ls -la"},
            "tool_response": {"stdout": "", "stderr": ""},
            "tool_use_id": "toolu_sample"
        }),
        "PostToolUseFailure" => json!({
            "tool_name": "Bash",
            "tool_input": {"command": "false"},
            "error": "exit 1",
            "tool_use_id": "toolu_sample"
        }),
        "Notification" => json!({
            "message": "Claude needs your permission",
            "notification_type": "permission_prompt"
        }),
        "SubagentStart" | "SubagentStop" => json!({
            "agent_id": "agent-sample",
            "agent_type": "general-purpose"
        }),
        "Stop" => json!({"stop_hook_active": false}),
        "StopFailure" => json!({"error": "rate_limit"}),
        "InstructionsLoaded" => json!({
            "file_path": cx.env.cwd.join("CLAUDE.md").as_str(),
            "load_reason": "session_start"
        }),
        "ConfigChange" => json!({
            "source": "project_settings",
            "file_path": cx.env.cwd.join(".claude/settings.json").as_str()
        }),
        "CwdChanged" => json!({"old_cwd": cx.env.cwd.as_str(), "new_cwd": cx.env.cwd.as_str()}),
        "DirectoryAdded" => json!({"source": "slash_command", "path": cx.env.cwd.as_str()}),
        "FileChanged" => json!({"file_path": cx.env.cwd.join(".envrc").as_str()}),
        "PreCompact" | "PostCompact" => json!({"trigger": "manual"}),
        "PreModelSwitch" | "PostModelSwitch" => json!({"to_model": "claude-opus-5"}),
        "Elicitation" | "ElicitationResult" => json!({"mcp_server_name": "sample"}),
        "SessionEnd" => json!({"reason": "other"}),
        _ => json!({}),
    };
    if let (Some(base), Some(extra)) = (input.as_object_mut(), extra.as_object()) {
        for (k, v) in extra {
            base.insert(k.clone(), v.clone());
        }
    }
    input
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn there_are_33_events_with_unique_names() {
        let mut names: Vec<_> = EVENTS.iter().map(|e| e.name).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), 33);
        assert_eq!(schema().events.len(), 33);
    }

    #[test]
    fn handler_support_follows_the_events_table() {
        let support =
            |event: &str, kind: &str| super::event(event).unwrap().handlers.supports(kind);
        assert!(support("Setup", "command") && !support("Setup", "http"));
        assert!(support("SessionStart", "mcp_tool") && !support("SessionStart", "http"));
        assert!(support("PermissionRequest", "prompt") && !support("PermissionRequest", "agent"));
        assert!(support("Stop", "agent") && support("PreToolUse", "prompt"));
        assert!(support("Notification", "http") && !support("Notification", "prompt"));
    }

    #[test]
    fn the_schema_has_five_handler_kinds_with_the_required_fields() {
        let kinds: Vec<_> = schema()
            .handler_kinds
            .iter()
            .map(|k| k.kind.as_str())
            .collect();
        assert_eq!(kinds, ["command", "http", "mcp_tool", "prompt", "agent"]);
        let required = |k: &str| -> Vec<String> {
            schema()
                .handler_kinds
                .iter()
                .find(|h| h.kind == k)
                .unwrap()
                .fields
                .iter()
                .filter(|f| f.required)
                .map(|f| f.key.clone())
                .collect()
        };
        assert_eq!(required("command"), ["command"]);
        assert_eq!(required("http"), ["url"]);
        assert_eq!(required("mcp_tool"), ["server", "tool"]);
        assert_eq!(required("prompt"), ["prompt"]);
    }
}
