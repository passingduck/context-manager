//! `AddHook`, `UpdateHook`, `RemoveHook` and `SetHookEnabled` for Claude Code (spec 3).
//!
//! Hooks live in strict JSON files, and only the `hooks` subtree is rewritten. Claude Code has no
//! per-hook switch, so switching a hook off removes its handler from the file and stores it in
//! the tool's record of that file (`parked.rs`), in one change set. Switching it on puts it
//! back into its old place and removes the record.

use serde_json::Value;

use super::hook_text::{self as text, Pos};
use super::{invalid, preview, read_text, takes_effect, write};
use crate::agents::PlanCx;
use crate::agents::claude::hooks::{DraftProblem, check_draft};
use crate::model::*;
use crate::parked::{self, ParkedFile, ParkedHook, Restore};

fn checked(draft: &HookDraft) -> Result<Option<String>, ApiError> {
    check_draft(draft).map_err(|problem| match problem {
        DraftProblem::UnknownEvent => invalid("event", "claude.plan.unknown_event"),
        DraftProblem::Handler => invalid("handler", "claude.plan.invalid_handler"),
        DraftProblem::Matcher => invalid("matcher", "claude.plan.invalid_matcher"),
    })?;
    Ok(draft.matcher.clone().filter(|m| !m.is_empty()))
}

fn claude_view<'a>(cx: &'a PlanCx) -> Option<&'a AgentView> {
    cx.snapshot
        .agents
        .iter()
        .find(|view| view.agent == super::claude())
}

/// The text of a settings file as the edit starts from it. A file that does not exist yet starts
/// as an empty object. A file that is not strict JSON is not touched.
fn settings_text(cx: &PlanCx, file: &AbsPath) -> Result<(String, Option<Base>), ApiError> {
    let (text, base) = match read_text(cx, file)? {
        Some((text, base)) => (text, Some(base)),
        None => ("{}\n".to_owned(), None),
    };
    match serde_json::from_str::<Value>(&text) {
        Ok(Value::Object(_)) => Ok((text, base)),
        _ => Err(invalid("json", "claude.guard.settings_not_json")),
    }
}

fn settings_warnings(file: &AbsPath) -> Vec<Warning> {
    let name = file.file_name().unwrap_or("");
    let mut out = Vec::new();
    if name == "settings.json" || name == "settings.local.json" {
        out.push(takes_effect("claude.takes_effect.settings_live"));
    } else {
        out.push(takes_effect("claude.takes_effect.reload_plugins"));
    }
    out
}

/// The place of a hook in its JSON file, checked against the text on disk. A hook whose entry
/// changed since the scan is `NotFound`.
fn locate(hook: &Hook, file_text: &str) -> Result<Pos, ApiError> {
    let Locator::Json { pointer } = &hook.source.at else {
        return Err(invalid("hook", "claude.plan.hook_not_in_json"));
    };
    let pos = Pos::parse(pointer).ok_or(ApiError::NotFound)?;
    match text::handler_at(file_text, &pos)? {
        Some(now) if now == hook.handler.raw => Ok(pos),
        _ => Err(ApiError::NotFound),
    }
}

// ---------------------------------------------------------------------------------------------
// Add, update, remove
// ---------------------------------------------------------------------------------------------

pub(super) fn add(
    cx: &PlanCx,
    edit: &Edit,
    file: &AbsPath,
    draft: &HookDraft,
) -> Result<Preview, ApiError> {
    let matcher = checked(draft)?;
    let listed =
        claude_view(cx).is_some_and(|view| view.hook_files.iter().any(|f| f.file == *file));
    if !listed {
        return Err(invalid("file", "claude.plan.hook_file"));
    }
    let (start, base) = settings_text(cx, file)?;
    let after = text::insert(
        &start,
        "/hooks",
        &draft.event,
        matcher.as_deref(),
        &draft.handler,
        None,
    )?;
    Ok(preview(
        edit,
        vec![write(file, base.as_ref(), after)],
        settings_warnings(file),
    ))
}

pub(super) fn update(
    cx: &PlanCx,
    edit: &Edit,
    hook: &Hook,
    draft: &HookDraft,
) -> Result<Preview, ApiError> {
    let matcher = checked(draft)?;
    let file = &hook.source.file;
    if matches!(hook.source.at, Locator::Parked { .. }) {
        return update_parked(cx, edit, hook, draft, matcher);
    }
    if !matches!(hook.source.at, Locator::Json { .. }) {
        return Err(invalid("hook", "claude.plan.hook_not_in_json"));
    }
    let (start, base) = settings_text(cx, file)?;
    let pos = locate(hook, &start)?;
    let current_matcher = text::matcher_at(&start, &pos)?;
    let same_place = pos.event == draft.event && current_matcher == matcher;
    let after = if same_place && draft.handler == hook.handler.raw {
        start.clone()
    } else if same_place {
        text::replace(&start, &pos, &draft.handler)?
    } else if pos.event == draft.event && text::is_sole_in_group(&start, &pos)? {
        let moved = text::set_matcher(&start, &pos, matcher.as_deref())?;
        text::replace(&moved, &pos, &draft.handler)?
    } else {
        let removed = text::remove(&start, &pos)?;
        text::insert(
            &removed,
            &pos.map,
            &draft.event,
            matcher.as_deref(),
            &draft.handler,
            None,
        )?
    };
    let ops = if after == start {
        Vec::new()
    } else {
        vec![write(file, base.as_ref(), after)]
    };
    Ok(preview(edit, ops, settings_warnings(file)))
}

pub(super) fn remove(cx: &PlanCx, edit: &Edit, hook: &Hook) -> Result<Preview, ApiError> {
    let file = &hook.source.file;
    if let Locator::Parked { key } = &hook.source.at {
        let mut record = Record::read(cx, file)?;
        let at = record.position(key, hook)?;
        let gone = record.hooks.remove(at);
        // The later entries of the event counted the removed one.
        for other in record.hooks.iter_mut().filter(|o| o.event == gone.event) {
            if other.index > gone.index {
                other.index -= 1;
            }
        }
        return Ok(preview(edit, vec![record.op(cx, file)], Vec::new()));
    }
    if !matches!(hook.source.at, Locator::Json { .. }) {
        return Err(invalid("hook", "claude.plan.hook_not_in_json"));
    }
    let (start, base) = settings_text(cx, file)?;
    let pos = locate(hook, &start)?;
    let after = text::remove(&start, &pos)?;
    Ok(preview(
        edit,
        vec![write(file, base.as_ref(), after)],
        settings_warnings(file),
    ))
}

// ---------------------------------------------------------------------------------------------
// Parking (spec 3, Switches: no per-hook disable exists)
// ---------------------------------------------------------------------------------------------

/// The tool's record of the hooks parked from one settings file.
struct Record {
    hooks: Vec<ParkedHook>,
    /// The record file as it is on disk.
    was: Option<Base>,
}

impl Record {
    fn read(cx: &PlanCx, file: &AbsPath) -> Result<Record, ApiError> {
        let path = parked::record_path(cx.parked_dir, file);
        let was = read_text(cx, &path)?.map(|(_, base)| base);
        Ok(Record {
            hooks: parked::read(cx.fs, cx.parked_dir, file),
            was,
        })
    }

    /// The entry a parked hook stands for. A record that changed since the scan is `NotFound`.
    fn position(&self, key: &str, hook: &Hook) -> Result<usize, ApiError> {
        let at: usize = key.parse().map_err(|_| ApiError::NotFound)?;
        match self.hooks.get(at) {
            Some(entry) if entry.handler == hook.handler.raw => Ok(at),
            _ => Err(ApiError::NotFound),
        }
    }

    /// The operation that leaves the record as it is now: a write, or its removal when it is empty.
    fn op(&self, cx: &PlanCx, file: &AbsPath) -> FileOp {
        let path = parked::record_path(cx.parked_dir, file);
        if self.hooks.is_empty() {
            return FileOp {
                path,
                expect: self.was.clone().unwrap_or(Base::Absent),
                action: Action::Trash,
            };
        }
        let text = parked::render(&ParkedFile {
            source: file.clone(),
            hooks: self.hooks.clone(),
        });
        write(&path, self.was.as_ref(), text)
    }
}

fn update_parked(
    cx: &PlanCx,
    edit: &Edit,
    hook: &Hook,
    draft: &HookDraft,
    matcher: Option<String>,
) -> Result<Preview, ApiError> {
    let Locator::Parked { key } = &hook.source.at else {
        unreachable!("the caller checked the locator");
    };
    let file = &hook.source.file;
    let mut record = Record::read(cx, file)?;
    let at = record.position(key, hook)?;
    let entry = &mut record.hooks[at];
    if entry.event != draft.event {
        // It lands after the event's other handlers, parked or not.
        let (start, _) = settings_text(cx, file)?;
        let live = event_len(&start, &draft.event);
        let parked_in_event = record
            .hooks
            .iter()
            .filter(|o| o.event == draft.event)
            .count();
        let entry = &mut record.hooks[at];
        entry.index = live + parked_in_event;
    }
    let entry = &mut record.hooks[at];
    entry.event = draft.event.clone();
    entry.matcher = matcher;
    entry.handler = draft.handler.clone();
    entry.restore = None;
    Ok(preview(edit, vec![record.op(cx, file)], Vec::new()))
}

fn event_len(text: &str, event: &str) -> usize {
    crate::cfg::jsonc::get(
        text,
        &format!("/hooks/{}", event.replace('~', "~0").replace('/', "~1")),
    )
    .ok()
    .flatten()
    .and_then(|groups| {
        groups.as_array().map(|groups| {
            groups
                .iter()
                .map(|g| g.get("hooks").and_then(Value::as_array).map_or(0, Vec::len))
                .sum()
        })
    })
    .unwrap_or(0)
}

pub(super) fn set_enabled(
    cx: &PlanCx,
    edit: &Edit,
    hook: &Hook,
    enabled: bool,
) -> Result<Preview, ApiError> {
    let parked = matches!(
        hook.state,
        HookState::Disabled {
            by: DisabledBy::Parked
        }
    );
    match (parked, enabled) {
        (true, false) | (false, true) => Ok(preview(edit, Vec::new(), Vec::new())),
        (false, false) => park(cx, edit, hook),
        (true, true) => unpark(cx, edit, hook),
    }
}

/// Removes the handler from the settings file and records it, both in one change set.
fn park(cx: &PlanCx, edit: &Edit, hook: &Hook) -> Result<Preview, ApiError> {
    let file = &hook.source.file;
    let (start, base) = settings_text(cx, file)?;
    let pos = locate(hook, &start)?;
    let matcher = text::matcher_at(&start, &pos)?;
    let live_index = text::flat_index(&start, &pos)?;
    let mut record = Record::read(cx, file)?;
    // The index counts the handlers parked from this event, so that putting them back in any
    // order rebuilds the original order.
    let mut index = live_index;
    let mut earlier: Vec<usize> = record
        .hooks
        .iter()
        .filter(|o| o.event == pos.event)
        .map(|o| o.index)
        .collect();
    earlier.sort_unstable();
    for at in earlier {
        if at <= index {
            index += 1;
        }
    }
    let after = text::remove(&start, &pos)?;
    record.hooks.push(ParkedHook {
        event: pos.event.clone(),
        matcher,
        handler: hook.handler.raw.clone(),
        index,
        restore: removal(&start, &after),
    });
    let mut warnings = settings_warnings(file);
    warnings.push(Warning::ParkedOnThisMachine);
    Ok(preview(
        edit,
        vec![write(file, base.as_ref(), after), record.op(cx, file)],
        warnings,
    ))
}

/// What a removal took out, when it only took text out: `after` is `before` with one span cut.
fn removal(before: &str, after: &str) -> Option<Restore> {
    let mut prefix = before
        .bytes()
        .zip(after.bytes())
        .take_while(|(a, b)| a == b)
        .count();
    while !before.is_char_boundary(prefix) {
        prefix -= 1;
    }
    let mut suffix = before
        .bytes()
        .rev()
        .zip(after.bytes().rev())
        .take_while(|(a, b)| a == b)
        .count()
        .min(after.len() - prefix);
    while !before.is_char_boundary(before.len() - suffix) {
        suffix -= 1;
    }
    (prefix + suffix == after.len()).then(|| Restore {
        offset: prefix,
        text: before[prefix..before.len() - suffix].to_owned(),
        after: sha256_hex(after.as_bytes()),
    })
}

/// The file as it was before the removal, when it is still as the removal left it.
fn restored(now: &str, restore: &Restore) -> Option<String> {
    if sha256_hex(now.as_bytes()) != restore.after || !now.is_char_boundary(restore.offset) {
        return None;
    }
    let mut text = String::with_capacity(now.len() + restore.text.len());
    text.push_str(&now[..restore.offset]);
    text.push_str(&restore.text);
    text.push_str(&now[restore.offset..]);
    serde_json::from_str::<Value>(&text).ok()?;
    Some(text)
}

/// Puts the handler back into its old group when that group still exists, else into a new one,
/// and removes it from the record.
fn unpark(cx: &PlanCx, edit: &Edit, hook: &Hook) -> Result<Preview, ApiError> {
    let Locator::Parked { key } = &hook.source.at else {
        return Err(ApiError::NotFound);
    };
    let file = &hook.source.file;
    let mut record = Record::read(cx, file)?;
    let at = record.position(key, hook)?;
    let entry = record.hooks.remove(at);
    // Its place among the handlers that are in the file: those parked before it are not.
    let before = record
        .hooks
        .iter()
        .filter(|o| o.event == entry.event && o.index < entry.index)
        .count();
    let (start, base) = settings_text(cx, file)?;
    let exact = entry.restore.as_ref().and_then(|r| restored(&start, r));
    let after = match exact {
        Some(text) => text,
        None => text::insert(
            &start,
            "/hooks",
            &entry.event,
            entry.matcher.as_deref(),
            &entry.handler,
            Some(entry.index.saturating_sub(before)),
        )?,
    };
    let mut warnings = settings_warnings(file);
    warnings.retain(|w| !matches!(w, Warning::ParkedOnThisMachine));
    Ok(preview(
        edit,
        vec![write(file, base.as_ref(), after), record.op(cx, file)],
        warnings,
    ))
}
