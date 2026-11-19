//! Claude Code hooks (spec 3). Every definition location that is a file: settings files,
//! managed files, the server-managed cache, plugin hook files and manifests, and the
//! frontmatter of skills, commands and subagents. Hooks defined in code (the SDK, `--settings`,
//! `--agents`) are not on disk and are not listed.

mod exec;
mod schema;

use serde_json::Value;

use super::extensions::{ExtOut, plugins::Plugin};
use super::setup::{SettingsFile, SettingsKind, Setup};
use crate::agents::{FileClaim, ScanCx};
use crate::md::frontmatter;
use crate::model::*;
use crate::parked;
use schema::Event;

pub(super) struct HooksOut {
    pub hooks: Vec<Hook>,
    pub hook_files: Vec<HookFile>,
    pub diagnostics: Vec<Diagnostic>,
    /// Files this half looked at that no load names.
    pub files: Vec<FileClaim>,
}

/// Why a draft is refused before it is written (spec 3, Schema, Handler types, Matcher).
pub(super) enum DraftProblem {
    UnknownEvent,
    Handler,
    Matcher,
}

/// A hook a user adds or changes must be one Claude Code would run: a known event, a handler it
/// accepts there, and a matcher that is not an invalid regular expression. A broken entry under
/// `PreToolUse` or `PermissionRequest` can stop the whole file from applying.
pub(super) fn check_draft(draft: &HookDraft) -> Result<(), DraftProblem> {
    let info = schema::event(&draft.event).ok_or(DraftProblem::UnknownEvent)?;
    if check_handler(Some(info), &draft.event, &draft.handler).is_some() {
        return Err(DraftProblem::Handler);
    }
    let matcher = draft.matcher.as_deref().filter(|m| !m.is_empty());
    if matches!(matcher_eval(info, matcher).0, MatcherEval::Invalid { .. }) {
        return Err(DraftProblem::Matcher);
    }
    Ok(())
}

pub(super) fn schema() -> &'static HookSchema {
    schema::schema()
}

pub(super) fn sample_input(event: &str, cx: &ScanCx) -> Value {
    schema::sample_input(event, cx)
}

pub(super) fn exec_spec(hook: &Hook, input: &Value, cx: &ScanCx) -> Result<ExecSpec, RuleRef> {
    exec::exec_spec(hook, input, cx)
}

/// Where a hook definition came from, for the switches of spec 3.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Origin {
    Managed,
    Settings,
    /// A plugin by index into the plugin list.
    Plugin(usize),
    /// Frontmatter of a skill, command or subagent that is not in a plugin.
    Frontmatter,
}

struct Walker<'a, 'b> {
    cx: &'a ScanCx<'b>,
    setup: &'a Setup,
    plugins: &'a [Plugin],
    out: HooksOut,
}

/// Every hook of every definition location that is a file (spec 3, "Definition locations").
pub(super) fn scan(cx: &ScanCx, setup: &Setup, ext: &ExtOut) -> HooksOut {
    let mut walker = Walker {
        cx,
        setup,
        plugins: &ext.plugins,
        out: HooksOut {
            hooks: Vec::new(),
            hook_files: Vec::new(),
            diagnostics: Vec::new(),
            files: Vec::new(),
        },
    };
    walker.settings_files();
    walker.plugin_files();
    walker.frontmatter(ext);
    walker.hook_files();
    walker.out
}

fn escape(segment: &str) -> String {
    segment.replace('~', "~0").replace('/', "~1")
}

fn hook_id(file: &AbsPath, locator: &str, raw: &Value) -> HookId {
    let digest = serde_json::to_string(raw).unwrap_or_default();
    HookId(sha256_hex(format!("claude\0{file}\0{locator}\0{digest}").as_bytes())[..32].to_owned())
}

impl Walker<'_, '_> {
    fn diag(&mut self, severity: Severity, at: Subject, problem: Problem) {
        self.out.diagnostics.push(Diagnostic {
            severity,
            agent: Some(AgentId::builtin("claude")),
            at,
            problem,
            fix: None,
        });
    }

    /// Spec 3: User, project and local settings and the managed files, in the order managed
    /// first, then user, project, local. Managed files are read-only.
    fn settings_files(&mut self) {
        let setup = self.setup;
        let mut ordered: Vec<&SettingsFile> = Vec::new();
        // `setup.settings` is highest precedence first. Managed: the base file before its
        // drop-ins, the server-managed cache after them.
        ordered.extend(
            setup
                .settings
                .iter()
                .filter(|f| f.kind == SettingsKind::Managed)
                .rev(),
        );
        ordered.extend(
            setup
                .settings
                .iter()
                .filter(|f| f.kind == SettingsKind::Remote),
        );
        for kind in [
            SettingsKind::User,
            SettingsKind::Project,
            SettingsKind::Local,
        ] {
            ordered.extend(setup.settings.iter().filter(|f| f.kind == kind).rev());
        }
        for file in ordered {
            self.out.files.push(FileClaim {
                path: file.path.clone(),
                policy: file.policy(),
            });
            let start = self.out.hooks.len();
            let parkable = !matches!(file.kind, SettingsKind::Managed | SettingsKind::Remote);
            if let Some(map) = file.get("hooks") {
                let origin = if parkable {
                    Origin::Settings
                } else {
                    Origin::Managed
                };
                let toggle = if parkable {
                    Toggle::Park
                } else {
                    Toggle::Unavailable {
                        why: RuleRef::new("claude.hook.toggle_managed"),
                    }
                };
                self.event_map(
                    map,
                    &file.path,
                    &file.scope,
                    origin,
                    toggle,
                    Registration::SessionStart,
                    &Loc::Json(String::new()),
                );
            }
            if parkable {
                self.parked(file, start);
            }
        }
    }

    /// Spec 3, Switches: Claude Code has no per-hook switch, so a hook the operator switched off
    /// lives in the tool's record of this file. It is listed in its old place, `Disabled`.
    /// `ParkedHook::index` counts the event's handlers with the parked ones in, so inserting the
    /// records in ascending order rebuilds the original order.
    fn parked(&mut self, file: &SettingsFile, start: usize) {
        let Some(dir) = self.cx.parked_dir else {
            return;
        };
        let records = parked::read(self.cx.fs, dir, &file.path);
        let mut order: Vec<usize> = (0..records.len()).collect();
        order.sort_by_key(|&key| (records[key].index, key));
        for key in order {
            let record = &records[key];
            let eval = match schema::event(&record.event) {
                Some(info) => matcher_eval(info, record.matcher.as_deref()).0,
                None => MatcherEval::All,
            };
            let same_event: Vec<usize> = (start..self.out.hooks.len())
                .filter(|&at| self.out.hooks[at].event == record.event)
                .collect();
            let at = match same_event.get(record.index) {
                Some(&at) => at,
                None => same_event
                    .last()
                    .map_or(self.out.hooks.len(), |&last| last + 1),
            };
            let hook = Hook {
                id: hook_id(&file.path, &format!("parked/{key}"), &record.handler),
                source: HookSource {
                    file: file.path.clone(),
                    at: Locator::Parked {
                        key: key.to_string(),
                    },
                    scope: file.scope.clone(),
                    registered: Registration::SessionStart,
                },
                event: record.event.clone(),
                matcher: Matcher {
                    raw: record.matcher.clone(),
                    eval,
                },
                handler: handler_of(&record.handler),
                state: HookState::Disabled {
                    by: DisabledBy::Parked,
                },
                toggle: Toggle::Park,
            };
            self.out.hooks.insert(at, hook);
        }
    }

    /// Spec 3: `<plugin-root>/hooks/hooks.json` and the manifest `hooks` key.
    fn plugin_files(&mut self) {
        let plugins = self.plugins;
        for (index, plugin) in plugins.iter().enumerate() {
            let scope = plugin.scope();
            let toggle = Toggle::Unavailable {
                why: RuleRef::new("claude.hook.toggle_plugin"),
            };
            let default_file = plugin.root.join("hooks/hooks.json");
            let mut loaded_default = false;
            let manifest_hooks = plugin.manifest.as_ref().and_then(|m| m.get("hooks"));
            // Manifest entries: a path to a hooks file, an inline event map, or an array of both.
            let entries: Vec<(Option<Value>, String)> = match manifest_hooks {
                Some(Value::Array(items)) => items
                    .iter()
                    .enumerate()
                    .map(|(i, v)| (Some(v.clone()), format!("/hooks/{i}")))
                    .collect(),
                Some(v) => vec![(Some(v.clone()), "/hooks".to_owned())],
                None => Vec::new(),
            };
            for (value, base) in entries {
                match value {
                    Some(Value::String(raw)) => {
                        let path = plugin.root.join(&raw);
                        if !path.starts_with(&plugin.root) {
                            continue;
                        }
                        if path == default_file {
                            // Naming the standard file loads it once.
                            loaded_default = true;
                        }
                        self.plugin_hooks_file(index, &path, &scope, &toggle);
                    }
                    Some(map @ Value::Object(_)) => {
                        self.event_map(
                            &map,
                            &plugin.manifest_file,
                            &scope,
                            Origin::Plugin(index),
                            toggle.clone(),
                            Registration::SessionStart,
                            &Loc::Json(base),
                        );
                    }
                    _ => {}
                }
            }
            if !loaded_default {
                self.plugin_hooks_file(index, &default_file, &scope, &toggle);
            }
        }
    }

    /// A plugin hooks file is the wrapper `{"hooks": {...}}`.
    fn plugin_hooks_file(&mut self, index: usize, path: &AbsPath, scope: &Scope, toggle: &Toggle) {
        let Ok(bytes) = self.cx.fs.read(path) else {
            return;
        };
        let plugin = &self.plugins[index];
        self.out.files.push(FileClaim {
            path: path.clone(),
            policy: plugin.policy(),
        });
        match serde_json::from_slice::<Value>(&bytes) {
            Ok(value) => {
                if let Some(map) = value.get("hooks") {
                    self.event_map(
                        map,
                        path,
                        scope,
                        Origin::Plugin(index),
                        toggle.clone(),
                        Registration::SessionStart,
                        &Loc::Json("/hooks".to_owned()),
                    );
                }
            }
            Err(error) => self.diag(
                Severity::Error,
                Subject::File {
                    path: path.clone(),
                    line: None,
                },
                Problem::ParseError {
                    format: Format::Json,
                    detail: error.to_string(),
                    fatal_for_agent: false,
                },
            ),
        }
    }

    /// Spec 3: frontmatter `hooks:` of skills and commands (registered on invoke) and of
    /// subagents (while the subagent runs). Plugin agents ignore `hooks`.
    fn frontmatter(&mut self, ext: &ExtOut) {
        for e in &ext.extensions {
            if matches!(e.status, ExtStatus::Rejected { .. }) {
                continue;
            }
            let plugin_index = match &e.scope {
                Scope::Plugin { id } => self.plugins.iter().position(|p| &p.id == id),
                _ => None,
            };
            if plugin_index.is_some() && e.ext_kind == ExtKind::Subagent {
                continue;
            }
            let Ok(bytes) = self.cx.fs.read(&e.file) else {
                continue;
            };
            let text = String::from_utf8_lossy(&bytes);
            let fm = frontmatter::parse(&text);
            let Some(map) = fm.map.as_ref().and_then(|m| m.get("hooks")) else {
                continue;
            };
            let registration = match e.ext_kind {
                ExtKind::Subagent => Registration::WhileSubagent { ext: e.id.clone() },
                _ => Registration::OnSkillInvoke { ext: e.id.clone() },
            };
            let origin = plugin_index.map_or(Origin::Frontmatter, Origin::Plugin);
            self.event_map(
                map,
                &e.file,
                &e.scope,
                origin,
                Toggle::Unavailable {
                    why: RuleRef::new("claude.hook.toggle_frontmatter"),
                },
                registration,
                &Loc::Frontmatter,
            );
        }
    }

    /// Where a new hook may be added (spec 3, "Notes for the editor"): the three settings files
    /// the user owns. The managed files and the server-managed cache are listed read-only when
    /// they exist.
    fn hook_files(&mut self) {
        let setup = self.setup;
        let mut files = Vec::new();
        for kind in [
            SettingsKind::User,
            SettingsKind::Project,
            SettingsKind::Local,
        ] {
            // The first local file is the one at the main checkout root.
            if let Some(file) = setup.settings.iter().find(|f| f.kind == kind) {
                files.push(file);
            }
        }
        files.extend(setup.settings.iter().filter(|f| {
            matches!(f.kind, SettingsKind::Managed | SettingsKind::Remote) && f.exists
        }));
        for file in files {
            self.out.hook_files.push(HookFile {
                file: file.path.clone(),
                scope: file.scope.clone(),
                format: HookFormat::ClaudeSettingsJson,
                exists: file.exists,
                policy: file.policy(),
            });
        }
    }

    /// Reads one event map: `{ <Event>: [ { matcher?, hooks: [handler] } ] }`.
    #[allow(clippy::too_many_arguments)]
    fn event_map(
        &mut self,
        map: &Value,
        file: &AbsPath,
        scope: &Scope,
        origin: Origin,
        toggle: Toggle,
        registration: Registration,
        loc: &Loc,
    ) {
        let Some(events) = map.as_object() else {
            return;
        };
        for (event_name, groups) in events {
            let info = schema::event(event_name);
            let Some(groups) = groups.as_array() else {
                continue;
            };
            if info.is_none() {
                // Spec 3, bad entries: an unknown event name is ignored with a warning.
                self.diag(
                    Severity::Warning,
                    Subject::File {
                        path: file.clone(),
                        line: None,
                    },
                    Problem::HookUnknownEvent {
                        event: event_name.clone(),
                    },
                );
            }
            for (gi, group) in groups.iter().enumerate() {
                let matcher_raw = group.get("matcher").and_then(Value::as_str);
                let Some(handlers) = group.get("hooks").and_then(Value::as_array) else {
                    continue;
                };
                for (hi, handler) in handlers.iter().enumerate() {
                    let at = loc.at(event_name, gi, hi);
                    let id = hook_id(file, &loc.key(event_name, gi, hi), handler);
                    let (eval, problems) = match info {
                        Some(info) => matcher_eval(info, matcher_raw),
                        None => (MatcherEval::All, Vec::new()),
                    };
                    for problem in problems {
                        self.diag(
                            Severity::Warning,
                            Subject::Hook { hook: id.clone() },
                            problem,
                        );
                    }
                    let invalid = check_handler(info, event_name, handler);
                    if let (Some(reason), Some(info)) = (&invalid, info)
                        && matches!(info.name, "PreToolUse" | "PermissionRequest")
                    {
                        // Spec 3: any broken entry under these two events is fatal for what it
                        // sits in. UNVERIFIED for a settings file (H1).
                        self.diag(
                            Severity::Error,
                            Subject::File {
                                path: file.clone(),
                                line: None,
                            },
                            Problem::ParseError {
                                format: Format::Json,
                                detail: format!("{}: {reason}", loc.key(event_name, gi, hi)),
                                fatal_for_agent: false,
                            },
                        );
                    } else if invalid.is_some() && info.is_some() {
                        self.diag(
                            Severity::Warning,
                            Subject::Hook { hook: id.clone() },
                            Problem::HookWillNotRun {
                                why: HookBlock::UnsupportedType,
                            },
                        );
                    }
                    let state = if invalid.is_some() || info.is_none() {
                        HookState::WillNotRun {
                            why: HookBlock::UnsupportedType,
                        }
                    } else {
                        self.state(origin, scope)
                    };
                    self.out.hooks.push(Hook {
                        id,
                        source: HookSource {
                            file: file.clone(),
                            at,
                            scope: scope.clone(),
                            registered: registration.clone(),
                        },
                        event: event_name.clone(),
                        matcher: Matcher {
                            raw: matcher_raw.map(str::to_owned),
                            eval,
                        },
                        handler: handler_of(handler),
                        state,
                        toggle: toggle.clone(),
                    });
                }
            }
        }
    }

    /// Spec 3, Switches, for a hook that is itself valid.
    fn state(&self, origin: Origin, _scope: &Scope) -> HookState {
        let setup = self.setup;
        let cx = self.cx;
        let managed_kinds = [SettingsKind::Managed, SettingsKind::Remote];
        let is_managed = origin == Origin::Managed;
        // `--bare` skips discovery of hooks.
        if setup.flag(cx, "CLAUDE_CODE_SIMPLE") == Some(true) {
            return HookState::WillNotRun {
                why: HookBlock::FeatureOff,
            };
        }
        let plugin = match origin {
            Origin::Plugin(i) => self.plugins.get(i),
            _ => None,
        };
        if plugin.is_some_and(|p| !p.enabled) {
            return HookState::WillNotRun {
                why: HookBlock::PluginDisabled,
            };
        }
        let forced_plugin = plugin.is_some_and(|p| p.forced_by_managed);
        if !is_managed && !forced_plugin {
            let managed_only = setup
                .first_in("allowManagedHooksOnly", &managed_kinds)
                .is_some_and(|(v, _)| v.as_bool() == Some(true));
            // Spec 3: `strictPluginOnlyCustomization` stops settings and personal or project
            // frontmatter hooks, and keeps plugin hooks.
            let strict = match setup
                .first_in("strictPluginOnlyCustomization", &managed_kinds)
                .map(|(v, _)| v)
            {
                Some(Value::Bool(b)) => *b,
                Some(Value::Array(a)) => a.iter().any(|v| v.as_str() == Some("hooks")),
                _ => false,
            } && plugin.is_none();
            if managed_only || strict {
                return HookState::WillNotRun {
                    why: HookBlock::ManagedOnly,
                };
            }
            // `--safe-mode`: no hooks except managed-policy hooks.
            if setup.flag(cx, "CLAUDE_CODE_SAFE_MODE") == Some(true) {
                return HookState::WillNotRun {
                    why: HookBlock::FeatureOff,
                };
            }
        }
        // Spec 3, Workspace trust: settings-file hooks wait for a trusted folder. Unknown trust
        // counts as trusted, because `-p` and SDK sessions treat the folder as trusted.
        if origin == Origin::Settings && setup.trust().level == TrustLevel::Untrusted {
            return HookState::WillNotRun {
                why: HookBlock::FolderUntrusted,
            };
        }
        // `disableAllHooks`: the value after precedence wins. Managed hooks keep running unless
        // the key is set in managed settings.
        if let Some((value, file)) = setup.first("disableAllHooks")
            && value.as_bool() == Some(true)
            && (!is_managed || managed_kinds.contains(&file.kind))
        {
            return HookState::Disabled {
                by: DisabledBy::AllHooksOff {
                    set_in: file.path.clone(),
                },
            };
        }
        HookState::Active
    }
}

/// How a hook locates in its file.
enum Loc {
    /// A JSON pointer to the event map (empty for a settings file: the map is at `/hooks`).
    Json(String),
    Frontmatter,
}

impl Loc {
    fn key(&self, event: &str, group: usize, handler: usize) -> String {
        match self {
            Loc::Json(base) => {
                let base = if base.is_empty() { "/hooks" } else { base };
                format!("{base}/{}/{group}/hooks/{handler}", escape(event))
            }
            Loc::Frontmatter => format!("hooks.{event}.{group}.hooks.{handler}"),
        }
    }

    fn at(&self, event: &str, group: usize, handler: usize) -> Locator {
        match self {
            Loc::Json(_) => Locator::Json {
                pointer: self.key(event, group, handler),
            },
            Loc::Frontmatter => Locator::Frontmatter {
                path: [
                    "hooks".to_owned(),
                    event.to_owned(),
                    group.to_string(),
                    "hooks".to_owned(),
                    handler.to_string(),
                ]
                .to_vec(),
            },
        }
    }
}

/// Spec 3, "Matcher semantics".
fn matcher_eval(event: &Event, raw: Option<&str>) -> (MatcherEval, Vec<Problem>) {
    let raw = raw.map(str::trim_end);
    let blank = raw.is_none_or(|m| m.is_empty() || m == "*");
    if event.field.is_none() {
        // A matcher on an event without matcher support is silently ignored.
        return if blank {
            (MatcherEval::All, Vec::new())
        } else {
            (MatcherEval::Ignored, vec![Problem::HookMatcherIgnored])
        };
    }
    if blank {
        return (MatcherEval::All, Vec::new());
    }
    let raw = raw.unwrap_or_default();
    // FileChanged and StopFailure split on `|` only; `-`, space and `,` send the value to the
    // regex path.
    let pipe_only = matches!(event.name, "FileChanged" | "StopFailure");
    let exact = raw.chars().all(|c| {
        c.is_ascii_alphanumeric()
            || c == '_'
            || c == '|'
            || (!pipe_only && matches!(c, ',' | '-' | ' '))
    });
    if exact {
        let values: Vec<String> = raw
            .split(|c| c == '|' || (!pipe_only && c == ','))
            .map(|v| v.trim().to_owned())
            .filter(|v| !v.is_empty())
            .collect();
        // The closed values of the event: an unknown value never matches (spec 3, local
        // evidence: `init` under SessionStart).
        let problems = values
            .iter()
            .filter(|v| !event.values.is_empty() && !event.values.contains(&v.as_str()))
            .map(|v| Problem::HookMatcherInvalid {
                error: format!("unknown_value:{v}"),
            })
            .collect();
        return (MatcherEval::Exact { values }, problems);
    }
    match js_regex_error(raw) {
        Some(error) => (
            MatcherEval::Invalid {
                error: error.to_owned(),
            },
            vec![Problem::HookMatcherInvalid {
                error: error.to_owned(),
            }],
        ),
        None => (MatcherEval::Regex, Vec::new()),
    }
}

/// An approximation of JS `new RegExp(pattern)` validity, without a regex engine: balanced
/// groups and classes, no dangling escape, no quantifier with nothing to repeat.
fn js_regex_error(pattern: &str) -> Option<&'static str> {
    let mut depth = 0usize;
    let mut in_class = false;
    let mut escaped = false;
    let mut can_repeat = false;
    let mut prev = '\0';
    for c in pattern.chars() {
        if escaped {
            escaped = false;
            can_repeat = true;
            prev = c;
            continue;
        }
        if in_class {
            match c {
                '\\' => escaped = true,
                ']' => {
                    in_class = false;
                    can_repeat = true;
                }
                _ => {}
            }
            prev = c;
            continue;
        }
        match c {
            '\\' => escaped = true,
            '[' => in_class = true,
            '(' => {
                depth += 1;
                can_repeat = false;
            }
            ')' => {
                if depth == 0 {
                    return Some("unmatched_paren");
                }
                depth -= 1;
                can_repeat = true;
            }
            '|' => can_repeat = false,
            '*' | '+' => {
                if !can_repeat {
                    return Some("nothing_to_repeat");
                }
            }
            '?' => {
                // `(?:`, `(?=` and a lazy `*?` are fine; a bare `?` at the start is not.
                if !can_repeat && prev != '(' && !matches!(prev, '*' | '+' | '?' | '}') {
                    return Some("nothing_to_repeat");
                }
            }
            '^' | '$' => can_repeat = false,
            _ => can_repeat = true,
        }
        prev = c;
    }
    if escaped {
        return Some("trailing_backslash");
    }
    if in_class {
        return Some("unterminated_class");
    }
    if depth != 0 {
        return Some("unterminated_group");
    }
    None
}

/// Spec 3, "Handler types and fields": why Claude Code would ignore this handler, if it would.
fn check_handler(info: Option<&Event>, event: &str, handler: &Value) -> Option<String> {
    let Some(obj) = handler.as_object() else {
        return Some("not_an_object".to_owned());
    };
    let kind = obj.get("type").and_then(Value::as_str);
    let text = |key: &str| {
        obj.get(key)
            .and_then(Value::as_str)
            .is_some_and(|s| !s.is_empty())
    };
    let kind = match kind {
        Some(k @ ("command" | "http" | "mcp_tool" | "prompt" | "agent")) => k,
        Some(other) => return Some(format!("unknown_type:{other}")),
        None => return Some("missing_type".to_owned()),
    };
    let missing = match kind {
        "command" => !text("command"),
        "http" => !obj
            .get("url")
            .and_then(Value::as_str)
            .is_some_and(|u| u.contains("://")),
        "mcp_tool" => !(text("server") && text("tool")),
        _ => !text("prompt"),
    };
    if missing {
        return Some(format!("missing_field:{kind}"));
    }
    let info = info?;
    if !info.handlers.supports(kind) {
        return Some(format!("unsupported_here:{kind}"));
    }
    // `if` is evaluated only on tool events; elsewhere the handler never runs.
    if obj.contains_key("if") && !schema::event_takes_if(event) {
        return Some("if_on_non_tool_event".to_owned());
    }
    None
}

fn handler_of(handler: &Value) -> Handler {
    let text = |key: &str| handler.get(key).and_then(Value::as_str).unwrap_or("");
    let kind = text("type");
    let summary = match kind {
        "command" => {
            let args = handler.get("args").and_then(Value::as_array).map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(" ")
            });
            match args {
                Some(args) if !args.is_empty() => format!("{} {args}", text("command")),
                _ => text("command").to_owned(),
            }
        }
        "http" => text("url").to_owned(),
        "mcp_tool" => format!("{}:{}", text("server"), text("tool")),
        "prompt" | "agent" => text("prompt").chars().take(120).collect(),
        _ => String::new(),
    };
    // Only an explicit timeout is reported; the defaults are per type and event (spec 3).
    let timeout_ms = handler
        .get("timeout")
        .and_then(Value::as_f64)
        .filter(|t| *t > 0.0)
        .map(|t| (t * 1000.0) as u64);
    Handler {
        handler_kind: kind.to_owned(),
        raw: handler.clone(),
        summary,
        timeout_ms,
    }
}

#[cfg(test)]
mod tests;
