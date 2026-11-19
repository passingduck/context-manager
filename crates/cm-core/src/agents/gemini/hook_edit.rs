//! Hook edits (spec 5, spec 8): text in, text out through `cfg::jsonc`, so comments, key order
//! and the file's indentation survive. Only the hook's own entry changes. Gemini CLI refuses to
//! start on a trailing comma, so a result that has one that the file did not is refused.

use serde_json::{Value, json};

use super::plan::{invalid, preview, read, takes_effect, view, write_op};
use super::settings::LayerKind;
use super::world::World;
use crate::agents::{PlanCx, ScanCx};
use crate::cfg::jsonc::{self, JsoncError};
use crate::model::*;

type Planned = Result<Preview, ApiError>;

pub(super) fn add(cx: &PlanCx, file: &AbsPath, draft: &HookDraft) -> Planned {
    if !view(cx)?.hook_files.iter().any(|f| f.file == *file) {
        return Err(ApiError::OutsideBoundary { path: file.clone() });
    }
    check_draft(draft)?;
    let disk = read(cx, file)?;
    let after = add_handler(disk.as_deref().unwrap_or(""), draft)?;
    finish(file, disk, after, ChangeEdit::AddHook)
}

pub(super) fn update(cx: &PlanCx, id: &HookId, draft: &HookDraft) -> Planned {
    check_draft(draft)?;
    let (hook, text) = locate(cx, id)?;
    let at = Position::of(hook)?;
    let group_ptr = format!("/hooks/{}/{}", at.event, at.group);
    let group = get(&text, &group_ptr)?.unwrap_or(Value::Null);
    let old_matcher = group.get("matcher").and_then(Value::as_str);
    let new_matcher = draft.matcher.as_deref();
    let single = group
        .get("hooks")
        .and_then(Value::as_array)
        .is_some_and(|handlers| handlers.len() == 1);
    let handler_ptr = format!("{group_ptr}/hooks/{}", at.handler);
    let replace_handler = |text: String| -> Result<String, ApiError> {
        if hook.handler.raw == draft.handler {
            Ok(text)
        } else {
            shape(jsonc::set(&text, &handler_ptr, &draft.handler))
        }
    };
    let after = if draft.event == at.event && old_matcher == new_matcher {
        replace_handler(text.clone())?
    } else if draft.event == at.event && single {
        // The group holds only this handler, so its matcher can change in place.
        let matcher_ptr = format!("{group_ptr}/matcher");
        let text = match new_matcher {
            Some(m) => shape(jsonc::set(&text, &matcher_ptr, &json!(m)))?,
            None => shape(jsonc::remove(&text, &matcher_ptr))?,
        };
        replace_handler(text)?
    } else {
        // Another event or matcher: the handler moves to a group of its own. The new group goes
        // last, so the position of the old one does not change.
        let moved = add_handler(&text, draft)?;
        remove_handler(&moved, &at)?
    };
    finish(&hook.source.file, Some(text), after, ChangeEdit::UpdateHook)
}

pub(super) fn remove(cx: &PlanCx, id: &HookId) -> Planned {
    let (hook, text) = locate(cx, id)?;
    let after = remove_handler(&text, &Position::of(hook)?)?;
    finish(&hook.source.file, Some(text), after, ChangeEdit::RemoveHook)
}

/// Spec 5, `hooksConfig.disabled`: the name of the hook, else its command.
pub(super) fn switch_key(hook: &Hook) -> Option<&str> {
    let text = |key: &str| {
        hook.handler
            .raw
            .get(key)
            .and_then(Value::as_str)
            .filter(|v| !v.is_empty())
    };
    text("name").or_else(|| text("command"))
}

pub(super) fn set_enabled(cx: &PlanCx, id: &HookId, enabled: bool) -> Planned {
    let view = view(cx)?;
    let hook = find(view, id)?;
    let key = switch_key(hook).ok_or_else(|| invalid("hook", "gemini.hook.no_switch_key"))?;
    let others = view
        .hooks
        .iter()
        .filter(|h| h.id != hook.id && switch_key(h) == Some(key))
        .count();
    let mut warnings = vec![takes_effect("gemini.takes_effect.restart")];
    if others > 0 {
        warnings.push(Warning::HookKeyShared {
            others: others as u32,
        });
    }

    let scan = ScanCx {
        env: cx.env,
        fs: cx.fs,
        parked_dir: None,
    };
    let w = World::new(&scan);
    let listed = |value: &Value| {
        value
            .pointer("/hooksConfig/disabled")
            .and_then(Value::as_array)
            .is_some_and(|names| names.iter().any(|n| n.as_str() == Some(key)))
    };
    let mut ops = Vec::new();
    if enabled {
        // The hook runs again only when no layer lists its key.
        for layer in w.settings.layers.iter().filter(|l| l.active && l.exists) {
            let editable = matches!(layer.kind, LayerKind::User | LayerKind::Workspace);
            if !listed(&layer.value) {
                continue;
            }
            if !editable {
                return Err(ApiError::ReadOnly {
                    why: RuleRef::new("gemini.hook.disabled_by_policy"),
                });
            }
            let disk = read(cx, &layer.path)?;
            let Some(after) = remove_key(disk.as_deref().unwrap_or(""), key)? else {
                continue;
            };
            check_commas(disk.as_deref().unwrap_or(""), &after)?;
            ops.push(write_op(&layer.path, &disk, after));
        }
    } else if !w
        .settings
        .layers
        .iter()
        .filter(|l| l.active)
        .any(|l| listed(&l.value))
    {
        let user = w
            .settings
            .layer(LayerKind::User)
            .ok_or_else(|| invalid("hook", "gemini.hook.no_switch_key"))?;
        let disk = read(cx, &user.path)?;
        let text = disk.as_deref().unwrap_or("");
        let after = add_key(text, key)?;
        check_commas(text, &after)?;
        ops.push(write_op(&user.path, &disk, after));
    }
    if ops.is_empty() {
        warnings.clear();
    }
    Ok(preview(ChangeEdit::SetHookEnabled, ops, warnings))
}

fn add_key(text: &str, key: &str) -> Result<String, ApiError> {
    const LIST: &str = "/hooksConfig/disabled";
    match get(text, LIST)? {
        Some(Value::Array(_)) => shape(jsonc::array_append(text, LIST, &json!(key))),
        Some(_) => Err(invalid("hook", "gemini.hook.shape")),
        None => shape(jsonc::set(text, LIST, &json!([key]))),
    }
}

/// `None` when the file does not list the key. An emptied list goes with its `hooksConfig`
/// when nothing else is in there, so a switch off and on returns the file to its bytes.
fn remove_key(text: &str, key: &str) -> Result<Option<String>, ApiError> {
    const LIST: &str = "/hooksConfig/disabled";
    let Some(Value::Array(names)) = get(text, LIST)? else {
        return Ok(None);
    };
    let at: Vec<usize> = names
        .iter()
        .enumerate()
        .filter(|(_, n)| n.as_str() == Some(key))
        .map(|(i, _)| i)
        .collect();
    if at.is_empty() {
        return Ok(None);
    }
    let mut text = text.to_owned();
    for i in at.iter().rev() {
        text = shape(jsonc::remove(&text, &format!("{LIST}/{i}")))?;
    }
    if names.len() == at.len() {
        text = shape(jsonc::remove(&text, LIST))?;
        if get(&text, "/hooksConfig")?.is_some_and(|v| v.as_object().is_some_and(|o| o.is_empty()))
        {
            text = shape(jsonc::remove(&text, "/hooksConfig"))?;
        }
    }
    Ok(Some(text))
}

// ---------------------------------------------------------------------------------------------
// The hook's own entry
// ---------------------------------------------------------------------------------------------

/// Where a hook sits: `/hooks/<event>/<group>/hooks/<handler>`.
struct Position {
    event: String,
    group: usize,
    handler: usize,
}

impl Position {
    fn of(hook: &Hook) -> Result<Position, ApiError> {
        let Locator::Json { pointer } = &hook.source.at else {
            return Err(ApiError::NotFound);
        };
        let parts: Vec<&str> = pointer.split('/').collect();
        match parts.as_slice() {
            ["", "hooks", event, group, "hooks", handler] => Ok(Position {
                event: (*event).to_owned(),
                group: group.parse().map_err(|_| ApiError::NotFound)?,
                handler: handler.parse().map_err(|_| ApiError::NotFound)?,
            }),
            _ => Err(ApiError::NotFound),
        }
    }
}

fn find<'a>(view: &'a AgentView, id: &HookId) -> Result<&'a Hook, ApiError> {
    view.hooks
        .iter()
        .find(|h| h.id == *id)
        .ok_or(ApiError::NotFound)
}

/// The hook of a fresh scan and the text of its file. An entry that is no longer what the scan
/// saw is `NotFound`, never a write to whatever sits at that position now.
fn locate<'a>(cx: &'a PlanCx, id: &HookId) -> Result<(&'a Hook, String), ApiError> {
    let hook = find(view(cx)?, id)?;
    let text = read(cx, &hook.source.file)?.ok_or(ApiError::NotFound)?;
    let Locator::Json { pointer } = &hook.source.at else {
        return Err(ApiError::NotFound);
    };
    if get(&text, pointer)?.as_ref() != Some(&hook.handler.raw) {
        return Err(ApiError::NotFound);
    }
    Ok((hook, text))
}

/// Spec 5: the 11 events, and a handler object with a string `type`, which the CLI discards
/// without.
fn check_draft(draft: &HookDraft) -> Result<(), ApiError> {
    if !super::hooks::schema()
        .events
        .iter()
        .any(|e| e.name == draft.event)
    {
        return Err(invalid("event", "gemini.hook.unknown_event"));
    }
    let typed = draft
        .handler
        .as_object()
        .is_some_and(|o| o.get("type").is_some_and(Value::is_string));
    if !typed {
        return Err(invalid("handler", "gemini.hook.invalid_handler"));
    }
    Ok(())
}

/// Appends a group of its own at the end of the event's array.
fn add_handler(text: &str, draft: &HookDraft) -> Result<String, ApiError> {
    let mut group = serde_json::Map::new();
    if let Some(matcher) = &draft.matcher {
        group.insert("matcher".to_owned(), json!(matcher));
    }
    group.insert("hooks".to_owned(), json!([draft.handler]));
    shape(jsonc::array_append(
        text,
        &format!("/hooks/{}", draft.event),
        &Value::Object(group),
    ))
}

/// Removes the handler. The last handler of a group takes the group with it, and the last group
/// of an event takes the event key.
fn remove_handler(text: &str, at: &Position) -> Result<String, ApiError> {
    let group_ptr = format!("/hooks/{}/{}", at.event, at.group);
    let count = get(text, &format!("{group_ptr}/hooks"))?
        .and_then(|v| v.as_array().map(Vec::len))
        .ok_or(ApiError::NotFound)?;
    if count > 1 {
        return shape(jsonc::remove(
            text,
            &format!("{group_ptr}/hooks/{}", at.handler),
        ));
    }
    let event_ptr = format!("/hooks/{}", at.event);
    let text = shape(jsonc::remove(text, &group_ptr))?;
    let emptied = get(&text, &event_ptr)?.is_some_and(|v| v.as_array().is_some_and(Vec::is_empty));
    if emptied {
        shape(jsonc::remove(&text, &event_ptr))
    } else {
        Ok(text)
    }
}

// ---------------------------------------------------------------------------------------------
// Plumbing
// ---------------------------------------------------------------------------------------------

fn get(text: &str, pointer: &str) -> Result<Option<Value>, ApiError> {
    shape(jsonc::get(text, pointer))
}

/// A file that does not parse, or a `hooks` key of the wrong type, is not ours to rewrite.
fn shape<T>(result: Result<T, JsoncError>) -> Result<T, ApiError> {
    result.map_err(|err| match err {
        JsoncError::Parse(_) => invalid("text", "gemini.settings_parse"),
        JsoncError::BadPointer(_) | JsoncError::Shape(_) => invalid("file", "gemini.hook.shape"),
    })
}

fn check_commas(before: &str, after: &str) -> Result<(), ApiError> {
    if jsonc::trailing_commas(after).len() > jsonc::trailing_commas(before).len() {
        return Err(invalid("text", "gemini.settings_parse"));
    }
    Ok(())
}

fn finish(file: &AbsPath, disk: Option<String>, after: String, edit: ChangeEdit) -> Planned {
    check_commas(disk.as_deref().unwrap_or(""), &after)?;
    if disk.as_deref() == Some(after.as_str()) {
        return Ok(preview(edit, Vec::new(), Vec::new()));
    }
    Ok(preview(
        edit,
        vec![write_op(file, &disk, after)],
        vec![takes_effect("gemini.takes_effect.restart")],
    ))
}
