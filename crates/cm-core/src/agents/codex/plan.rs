//! Planners (spec 2.5, 3.2, 5.3, 7.2): a new item, a memory note, a hook added, changed, removed
//! or switched. Text in, text out. Every plan returns only operations, warnings and conflicts.
//! The core recomputes `before` and the digest and re-checks the boundary, the base and the
//! guard.
//!
//! No plan writes `trusted_hash` or `trusted_content_hash`. A hook add, update or remove changes
//! what the hash covers or where the hook sits, so the hook stops until the operator trusts it
//! again in Codex (`Warning::HookTrustReset`). A switch writes only
//! `[hooks.state."<key>"].enabled`.

use std::collections::BTreeMap;

use serde_json::{Value as Json, json};

use super::hooks::{self, EVENTS, snake};
use super::out::me;
use super::setup::Setup;
use crate::agents::{PlanCx, ScanCx};
use crate::cfg::{jsonc, toml};
use crate::model::*;

const RULE_NAME: &str = "codex.edit.name";
const RULE_COLLISION: &str = "codex.edit.collision";
const RULE_SLOT: &str = "codex.edit.slot";
const RULE_STORE: &str = "codex.edit.store";
const RULE_EVENT: &str = "codex.edit.hook_event";
const RULE_HANDLER: &str = "codex.edit.hook_handler";
const RULE_FILE: &str = "codex.edit.hook_file";
const RULE_UNREADABLE: &str = "codex.edit.unreadable";
const RULE_NEXT_SESSION: &str = "codex.takes_effect.next_session";
const RULE_HOOKS_REREAD: &str = "codex.takes_effect.hooks";
const RULE_NOTE: &str = "codex.takes_effect.memory_note";
const RULE_MEMORY_OFF: &str = "codex.takes_effect.memory_off";

/// `None`: another agent's edit, or one the generic plan handles.
pub(super) fn plan(cx: &PlanCx, edit: &Edit) -> Option<Result<Preview, ApiError>> {
    match edit {
        Edit::NewItem { agent, slot, name } if *agent == me() => {
            Some(new_item(cx, slot, name.as_deref()))
        }
        Edit::AddMemory {
            agent,
            store,
            title,
            description,
            body,
        } if *agent == me() => Some(add_memory(cx, store, title, description, body)),
        Edit::AddHook { agent, file, draft } if *agent == me() => Some(add_hook(cx, file, draft)),
        Edit::UpdateHook { draft, .. } => cx.hook.map(|hook| update_hook(cx, hook, draft)),
        Edit::RemoveHook { .. } => cx.hook.map(|hook| remove_hook(cx, hook)),
        Edit::SetHookEnabled { enabled, .. } => cx.hook.map(|hook| set_enabled(cx, hook, *enabled)),
        _ => None,
    }
}

// ---------------------------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------------------------

fn invalid(field: &str, why: &'static str) -> ApiError {
    ApiError::InvalidEdit {
        field: field.to_owned(),
        why: RuleRef::new(why),
    }
}

fn view<'a>(cx: &'a PlanCx) -> Option<&'a AgentView> {
    cx.snapshot.agents.iter().find(|v| v.agent == me())
}

fn scan_cx<'a>(cx: &'a PlanCx) -> ScanCx<'a> {
    ScanCx::new(cx.env, cx.fs)
}

fn warning_effect(rule: &'static str) -> Warning {
    Warning::TakesEffect {
        agent: me(),
        when: RuleRef::new(rule),
    }
}

/// Warnings that follow a write to a Codex file whoever planned it, a plain save included. Codex
/// rewrites the user `config.toml` itself (spec 7.1).
pub(super) fn write_warnings(cx: &PlanCx, path: &AbsPath) -> Vec<Warning> {
    if Setup::load(&scan_cx(cx)).user_config == *path {
        vec![Warning::AgentAlsoWrites { agent: me() }]
    } else {
        Vec::new()
    }
}

/// The text of a file: `None` when it does not exist.
fn read(cx: &PlanCx, path: &AbsPath) -> Result<Option<String>, ApiError> {
    match cx.fs.read(path) {
        Ok(bytes) => String::from_utf8(bytes.as_ref().clone())
            .map(Some)
            .map_err(|_| ApiError::ReadOnly {
                why: ReadOnlyReason::File {
                    why: ReadOnlyWhy::NotUtf8,
                },
            }),
        Err(err) if err.is_not_found() => Ok(None),
        Err(err) => Err(err.into()),
    }
}

fn exists(cx: &PlanCx, path: &AbsPath) -> bool {
    cx.fs.stat(path).is_ok()
}

fn write_op(path: &AbsPath, old: &Option<String>, new: String) -> FileOp {
    FileOp {
        path: path.clone(),
        expect: match old {
            Some(text) => Base::Rev {
                rev: Revision::of(text.as_bytes()),
            },
            None => Base::Absent,
        },
        action: Action::Write { text: new },
    }
}

fn preview(
    edit: ChangeEdit,
    targets: Vec<AbsPath>,
    ops: Vec<FileOp>,
    warnings: Vec<Warning>,
) -> Preview {
    Preview {
        digest: String::new(),
        change_set: ChangeSet {
            summary: ChangeSummary {
                edit,
                agent: Some(me()),
                targets,
            },
            ops,
        },
        before: BTreeMap::new(),
        warnings,
        conflicts: Vec::new(),
    }
}

/// A plan with one write, or none when the text is already what the plan wants.
fn one_write(
    edit: ChangeEdit,
    path: &AbsPath,
    old: Option<String>,
    new: String,
    warnings: Vec<Warning>,
) -> Preview {
    let ops = if old.as_deref() == Some(new.as_str()) {
        Vec::new()
    } else {
        vec![write_op(path, &old, new)]
    };
    preview(edit, vec![path.clone()], ops, warnings)
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.chars().count() <= 64
        && !name.starts_with('.')
        && name
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

// ---------------------------------------------------------------------------------------------
// NewItem (spec 5.1, 5.3, 4, 1.1)
// ---------------------------------------------------------------------------------------------

fn new_item(cx: &PlanCx, slot: &SlotId, name: Option<&str>) -> Result<Preview, ApiError> {
    let slot = view(cx)
        .and_then(|v| v.slots.iter().find(|s| s.id == *slot))
        .ok_or_else(|| invalid("slot", RULE_SLOT))?;
    let (path, text) = match (&slot.new_kind, &slot.place) {
        (NewItemKind::Instructions, Place::File { path }) => (path.clone(), String::new()),
        (NewItemKind::Skill, Place::NamedDir { dir, file }) => {
            let name = name
                .filter(|n| valid_name(n))
                .ok_or_else(|| invalid("name", RULE_NAME))?;
            let folder = dir.join(name);
            if exists(cx, &folder) {
                return Err(invalid("name", RULE_COLLISION));
            }
            (
                folder.join(file),
                format!(
                    "---\nname: {name}\ndescription: Describe what this skill does and when to use it.\n---\n\n"
                ),
            )
        }
        (NewItemKind::Subagent, Place::NamedFile { dir, ext }) => {
            let name = name
                .filter(|n| valid_name(n))
                .ok_or_else(|| invalid("name", RULE_NAME))?;
            (
                dir.join(&format!("{name}.{}", ext.trim_start_matches('.'))),
                format!("name = \"{name}\"\ndescription = \"Describe what this subagent does.\"\n"),
            )
        }
        (NewItemKind::Rule, Place::NamedFile { dir, ext }) => {
            let name = name
                .filter(|n| valid_name(n))
                .ok_or_else(|| invalid("name", RULE_NAME))?;
            (
                dir.join(&format!("{name}.{}", ext.trim_start_matches('.'))),
                String::new(),
            )
        }
        _ => return Err(invalid("slot", RULE_SLOT)),
    };
    if exists(cx, &path) {
        return Err(invalid("name", RULE_COLLISION));
    }
    Ok(preview(
        ChangeEdit::NewItem,
        vec![path.clone()],
        vec![write_op(&path, &None, text)],
        vec![warning_effect(RULE_NEXT_SESSION)],
    ))
}

// ---------------------------------------------------------------------------------------------
// AddMemory (spec 3.2)
// ---------------------------------------------------------------------------------------------

/// The file stem of a note: letters and digits of the title, joined by dashes.
fn slug(title: &str) -> String {
    let mut out = String::new();
    for c in title.chars() {
        if c.is_alphanumeric() {
            out.extend(c.to_lowercase());
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    let out: String = out.trim_end_matches('-').chars().take(60).collect();
    if out.is_empty() {
        "note".to_owned()
    } else {
        out
    }
}

fn add_memory(
    cx: &PlanCx,
    store: &str,
    title: &str,
    description: &str,
    body: &str,
) -> Result<Preview, ApiError> {
    let store = view(cx)
        .and_then(|v| v.memory.iter().find(|m| m.id == store))
        .ok_or_else(|| invalid("store", RULE_STORE))?;
    // Generated memory files stay read-only. A note is the one supported way in.
    let MemoryWrite::NotesDir { dir } = &store.write else {
        return Err(invalid("store", RULE_STORE));
    };
    if title.trim().is_empty() {
        return Err(invalid("title", RULE_NAME));
    }
    let path = dir.join(&format!("{}.md", slug(title)));
    if exists(cx, &path) {
        return Err(invalid("title", RULE_COLLISION));
    }
    let mut text = format!("# {}\n", title.trim());
    for part in [description.trim(), body.trim()] {
        if !part.is_empty() {
            text.push('\n');
            text.push_str(part);
            text.push('\n');
        }
    }
    let when = match store.enabled {
        Enabled::On => RULE_NOTE,
        Enabled::Off { .. } => RULE_MEMORY_OFF,
    };
    Ok(preview(
        ChangeEdit::AddMemory,
        vec![path.clone()],
        vec![write_op(&path, &None, text)],
        vec![warning_effect(when)],
    ))
}

// ---------------------------------------------------------------------------------------------
// Hooks (spec 2.4, 2.5)
// ---------------------------------------------------------------------------------------------

/// Where a hook sits in its file.
struct At {
    event: String,
    group: usize,
    handler: usize,
    /// `Some(path)` for a TOML source, `None` for JSON.
    toml: Option<Vec<TomlSeg>>,
    pointer: String,
}

fn at_of(hook: &Hook) -> Option<At> {
    match &hook.source.at {
        Locator::Json { pointer } => {
            let parts: Vec<&str> = pointer.split('/').collect();
            // "", "hooks", event, group, "hooks", handler
            let [_, "hooks", event, group, "hooks", handler] = parts.as_slice() else {
                return None;
            };
            Some(At {
                event: (*event).to_owned(),
                group: group.parse().ok()?,
                handler: handler.parse().ok()?,
                toml: None,
                pointer: pointer.clone(),
            })
        }
        Locator::Toml { path } => {
            let [
                TomlSeg::Key { .. },
                TomlSeg::Key { name },
                TomlSeg::Index { index: group },
                TomlSeg::Key { .. },
                TomlSeg::Index { index: handler },
            ] = path.as_slice()
            else {
                return None;
            };
            Some(At {
                event: name.clone(),
                group: *group as usize,
                handler: *handler as usize,
                toml: Some(path.clone()),
                pointer: String::new(),
            })
        }
        _ => None,
    }
}

fn check_draft(draft: &HookDraft) -> Result<(), ApiError> {
    if !EVENTS.contains(&draft.event.as_str()) {
        return Err(invalid("draft.event", RULE_EVENT));
    }
    // Codex rejects the whole file for a handler it cannot parse.
    hooks::validate_handler(&draft.handler).map_err(|_| invalid("draft.handler", RULE_HANDLER))?;
    Ok(())
}

fn group_of(draft: &HookDraft) -> Json {
    let mut group = serde_json::Map::new();
    if let Some(matcher) = &draft.matcher {
        group.insert("matcher".to_owned(), json!(matcher));
    }
    group.insert("hooks".to_owned(), json!([draft.handler]));
    Json::Object(group)
}

fn unreadable<E>(_: E) -> ApiError {
    invalid("file", RULE_UNREADABLE)
}

fn hook_warnings() -> Vec<Warning> {
    vec![Warning::HookTrustReset, warning_effect(RULE_HOOKS_REREAD)]
}

/// A new group at the end of the event, so no other hook's key moves.
fn append_group(
    format: HookFormat,
    text: &str,
    event: &str,
    group: &Json,
) -> Result<String, ApiError> {
    match format {
        HookFormat::CodexHooksJson => {
            if text.trim().is_empty() {
                let mut out = serde_json::to_string_pretty(&json!({"hooks": {event: [group]}}))
                    .map_err(unreadable)?;
                out.push('\n');
                return Ok(out);
            }
            jsonc::array_append(text, &format!("/hooks/{event}"), group).map_err(unreadable)
        }
        _ => toml::aot_append(text, &["hooks", event], group).map_err(unreadable),
    }
}

fn add_hook(cx: &PlanCx, file: &AbsPath, draft: &HookDraft) -> Result<Preview, ApiError> {
    let hook_file = view(cx)
        .and_then(|v| v.hook_files.iter().find(|f| f.file == *file))
        .ok_or_else(|| invalid("file", RULE_FILE))?;
    if let EditPolicy::ReadOnly { why } = &hook_file.policy {
        return Err(ApiError::ReadOnly {
            why: ReadOnlyReason::File { why: why.clone() },
        });
    }
    check_draft(draft)?;
    let old = read(cx, file)?;
    let text = append_group(
        hook_file.format,
        old.as_deref().unwrap_or(""),
        &draft.event,
        &group_of(draft),
    )?;
    Ok(one_write(
        ChangeEdit::AddHook,
        file,
        old,
        text,
        hook_warnings(),
    ))
}

/// The group the hook sits in, read from the file, with the handler checked against the scan.
fn group_in(text: &str, hook: &Hook, at: &At) -> Result<Json, ApiError> {
    let group = match &at.toml {
        None => {
            jsonc::get(text, &format!("/hooks/{}/{}", at.event, at.group)).map_err(unreadable)?
        }
        Some(_) => toml::get(text, &["hooks", &at.event])
            .map_err(unreadable)?
            .and_then(|groups| groups.get(at.group).cloned()),
    };
    let group = group.ok_or(ApiError::NotFound)?;
    let same = group
        .get("hooks")
        .and_then(|h| h.get(at.handler))
        .is_some_and(|h| *h == hook.handler.raw);
    if same {
        Ok(group)
    } else {
        Err(ApiError::NotFound)
    }
}

fn handlers_in(group: &Json) -> usize {
    group
        .get("hooks")
        .and_then(Json::as_array)
        .map_or(0, Vec::len)
}

fn group_path(at: &At) -> Vec<TomlSeg> {
    at.toml
        .as_ref()
        .map(|p| p[..3].to_vec())
        .unwrap_or_default()
}

fn remove_entry(text: &str, at: &At, group: &Json) -> Result<String, ApiError> {
    let last = handlers_in(group) <= 1;
    match &at.toml {
        None => {
            let group_pointer = format!("/hooks/{}/{}", at.event, at.group);
            if !last {
                return jsonc::remove(text, &at.pointer).map_err(unreadable);
            }
            let text = jsonc::remove(text, &group_pointer).map_err(unreadable)?;
            let event_pointer = format!("/hooks/{}", at.event);
            let empty = matches!(
                jsonc::get(&text, &event_pointer).map_err(unreadable)?,
                Some(Json::Array(groups)) if groups.is_empty()
            );
            if empty {
                jsonc::remove(&text, &event_pointer).map_err(unreadable)
            } else {
                Ok(text)
            }
        }
        Some(path) => {
            let target = if last { group_path(at) } else { path.clone() };
            toml::element_remove(text, &target).map_err(unreadable)
        }
    }
}

fn remove_hook(cx: &PlanCx, hook: &Hook) -> Result<Preview, ApiError> {
    let at = at_of(hook).ok_or(ApiError::NotFound)?;
    let file = &hook.source.file;
    let old = read(cx, file)?;
    let text = old.clone().ok_or(ApiError::NotFound)?;
    let group = group_in(&text, hook, &at)?;
    let new = remove_entry(&text, &at, &group)?;
    Ok(one_write(
        ChangeEdit::RemoveHook,
        file,
        old,
        new,
        hook_warnings(),
    ))
}

fn update_hook(cx: &PlanCx, hook: &Hook, draft: &HookDraft) -> Result<Preview, ApiError> {
    let at = at_of(hook).ok_or(ApiError::NotFound)?;
    check_draft(draft)?;
    let file = &hook.source.file;
    let old = read(cx, file)?;
    let text = old.clone().ok_or(ApiError::NotFound)?;
    let group = group_in(&text, hook, &at)?;
    let matcher = group.get("matcher").and_then(Json::as_str);
    let same_matcher = draft.matcher.as_deref() == matcher;
    let same_handler = draft.handler == hook.handler.raw;
    let in_place = draft.event == at.event && (same_matcher || handlers_in(&group) == 1);
    let new = if !in_place {
        // Another event, or another matcher than the group's other handlers share: the entry
        // leaves its group and joins a new one at the end of its event.
        let removed = remove_entry(&text, &at, &group)?;
        append_group(
            if at.toml.is_some() {
                HookFormat::CodexConfigToml
            } else {
                HookFormat::CodexHooksJson
            },
            &removed,
            &draft.event,
            &group_of(draft),
        )?
    } else {
        let mut text = text;
        if !same_handler {
            text = match &at.toml {
                None => jsonc::set(&text, &at.pointer, &draft.handler).map_err(unreadable)?,
                Some(path) => toml::table_merge(&text, path, &draft.handler).map_err(unreadable)?,
            };
        }
        if !same_matcher {
            text = match &at.toml {
                None => {
                    let pointer = format!("/hooks/{}/{}/matcher", at.event, at.group);
                    match &draft.matcher {
                        Some(m) => jsonc::set(&text, &pointer, &json!(m)),
                        None => jsonc::remove(&text, &pointer),
                    }
                    .map_err(unreadable)?
                }
                Some(_) => toml::table_set_key(
                    &text,
                    &group_path(&at),
                    "matcher",
                    draft.matcher.as_ref().map(|m| json!(m)).as_ref(),
                )
                .map_err(unreadable)?,
            };
        }
        text
    };
    Ok(one_write(
        ChangeEdit::UpdateHook,
        file,
        old,
        new,
        hook_warnings(),
    ))
}

/// Spec 2.5: `[hooks.state."<key>"].enabled` in the user `config.toml`, nothing else.
fn set_enabled(cx: &PlanCx, hook: &Hook, enabled: bool) -> Result<Preview, ApiError> {
    let at = at_of(hook).ok_or(ApiError::NotFound)?;
    let suffix = format!(":{}:{}:{}", snake(&at.event), at.group, at.handler);
    let scx = scan_cx(cx);
    let listed = hooks::inspect(&scx)
        .into_iter()
        .find(|i| i.source_path == hook.source.file && i.key.ends_with(&suffix))
        .ok_or_else(|| invalid("hook", "codex.hooks.toggle_unsupported"))?;
    let config = Setup::load(&scx).user_config;
    let old = read(cx, &config)?;
    let warnings = vec![warning_effect(RULE_HOOKS_REREAD)];
    if listed.enabled == enabled {
        return Ok(preview(
            ChangeEdit::SetHookEnabled,
            vec![config],
            Vec::new(),
            warnings,
        ));
    }
    let base = old.clone().unwrap_or_default();
    let path = ["hooks", "state", listed.key.as_str(), "enabled"];
    let new = if enabled {
        // Absent means on. Removing the key puts back what the file had before the switch.
        toml::remove_pruned(&base, &path)
    } else {
        toml::set(&base, &path, &json!(false))
    }
    .map_err(unreadable)?;
    Ok(one_write(
        ChangeEdit::SetHookEnabled,
        &config,
        old,
        new,
        warnings,
    ))
}

#[cfg(test)]
mod tests;
