//! Text edits of the `hooks` event map of a strict JSON file (spec 3, "Notes for the editor"):
//! `{ <Event>: [ { matcher?, hooks: [handler] } ] }`. Only the hook's own entry changes. Every
//! other byte of the file stays, and a new entry copies the layout of its neighbours.

use serde_json::{Value, json};

use super::splice::{self, Kind, Node, Seg};
use crate::model::*;

pub(super) fn invalid(field: &str, why: &'static str) -> ApiError {
    ApiError::InvalidEdit {
        field: field.to_owned(),
        why: RuleRef::new(why),
    }
}

fn shape() -> ApiError {
    invalid("hooks", "claude.plan.hooks_shape")
}

fn not_json() -> ApiError {
    invalid("json", "claude.guard.settings_not_json")
}

fn unescape(segment: &str) -> String {
    segment.replace("~1", "/").replace("~0", "~")
}

/// Where a handler sits: the event map that holds it, its event, its group and its place in it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Pos {
    /// A pointer to the event map: `/hooks` in a settings file.
    pub map: String,
    pub event: String,
    pub group: usize,
    pub handler: usize,
}

impl Pos {
    /// `<map>/<Event>/<group>/hooks/<handler>`, the pointer the scan gives every hook.
    pub fn parse(pointer: &str) -> Option<Pos> {
        let mut parts = pointer.rsplitn(5, '/');
        let handler = parts.next()?.parse().ok()?;
        if parts.next()? != "hooks" {
            return None;
        }
        let group = parts.next()?.parse().ok()?;
        let event = unescape(parts.next()?);
        let map = parts.next()?.to_owned();
        Some(Pos {
            map,
            event,
            group,
            handler,
        })
    }

    /// The segments from the root to the event map. Every segment of a map pointer is a key,
    /// except the numbers of a plugin manifest's list of maps.
    fn map_path(&self) -> Vec<Seg> {
        map_path(&self.map)
    }

    fn event_path(&self) -> Vec<Seg> {
        let mut path = self.map_path();
        path.push(Seg::Key(self.event.clone()));
        path
    }

    fn group_path(&self) -> Vec<Seg> {
        let mut path = self.event_path();
        path.push(Seg::Index(self.group));
        path
    }

    fn handlers_path(&self) -> Vec<Seg> {
        let mut path = self.group_path();
        path.push(Seg::Key("hooks".to_owned()));
        path
    }
}

fn map_path(pointer: &str) -> Vec<Seg> {
    splice::path(pointer, |depth| depth > 0)
}

struct Doc<'a> {
    text: &'a str,
    root: Node,
}

impl<'a> Doc<'a> {
    fn new(text: &'a str) -> Result<Doc<'a>, ApiError> {
        let root = splice::parse(text).ok_or_else(not_json)?;
        if !matches!(root.kind, Kind::Object(_)) {
            return Err(shape());
        }
        Ok(Doc { text, root })
    }

    fn at(&self, path: &[Seg]) -> Option<&Node> {
        self.root.find(path)
    }

    fn array(&self, path: &[Seg]) -> Result<Option<&Node>, ApiError> {
        match self.at(path) {
            None => Ok(None),
            Some(node) if matches!(node.kind, Kind::Array(_)) => Ok(Some(node)),
            Some(_) => Err(shape()),
        }
    }

    fn items(node: &Node) -> &[Node] {
        match &node.kind {
            Kind::Array(items) => items,
            _ => &[],
        }
    }

    fn value(&self, node: &Node) -> Value {
        serde_json::from_str(&self.text[node.start..node.end]).unwrap_or(Value::Null)
    }
}

/// The handler at `pos`, or `None` when the file no longer has one there.
pub(super) fn handler_at(text: &str, pos: &Pos) -> Result<Option<Value>, ApiError> {
    let doc = Doc::new(text)?;
    let mut path = pos.handlers_path();
    path.push(Seg::Index(pos.handler));
    Ok(doc.at(&path).map(|node| doc.value(node)))
}

/// The group's `matcher`, when it is a string.
pub(super) fn matcher_at(text: &str, pos: &Pos) -> Result<Option<String>, ApiError> {
    let doc = Doc::new(text)?;
    let mut path = pos.group_path();
    path.push(Seg::Key("matcher".to_owned()));
    Ok(doc
        .at(&path)
        .and_then(|node| doc.value(node).as_str().map(str::to_owned)))
}

/// How many handlers of the event come before this one, across groups.
pub(super) fn flat_index(text: &str, pos: &Pos) -> Result<usize, ApiError> {
    let doc = Doc::new(text)?;
    let groups = doc.array(&pos.event_path())?;
    let before: usize = groups.map_or(0, |groups| {
        Doc::items(groups)
            .iter()
            .take(pos.group)
            .map(handlers_in)
            .sum()
    });
    Ok(before + pos.handler)
}

fn handlers_in(group: &Node) -> usize {
    group
        .find(&[Seg::Key("hooks".to_owned())])
        .map_or(0, Node::len)
}

/// Whether this handler is the only one in its group.
pub(super) fn is_sole_in_group(text: &str, pos: &Pos) -> Result<bool, ApiError> {
    let doc = Doc::new(text)?;
    Ok(doc
        .array(&pos.handlers_path())?
        .is_some_and(|handlers| handlers.len() == 1))
}

/// Removes the handler. A group left without handlers goes with it, then an event left without
/// groups, then a settings file's `hooks` key left without events.
pub(super) fn remove(text: &str, pos: &Pos) -> Result<String, ApiError> {
    let doc = Doc::new(text)?;
    let handlers = doc
        .array(&pos.handlers_path())?
        .filter(|h| h.len() > pos.handler)
        .ok_or(ApiError::NotFound)?;
    if handlers.len() > 1 {
        return Ok(splice::remove_entry(text, handlers, pos.handler));
    }
    let groups = doc.array(&pos.event_path())?.ok_or(ApiError::NotFound)?;
    if groups.len() > 1 {
        return Ok(splice::remove_entry(text, groups, pos.group));
    }
    let map = doc.at(&pos.map_path()).ok_or(ApiError::NotFound)?;
    let Kind::Object(events) = &map.kind else {
        return Err(shape());
    };
    let at = events
        .iter()
        .position(|m| m.key == pos.event)
        .ok_or(ApiError::NotFound)?;
    if events.len() > 1 || pos.map != "/hooks" {
        return Ok(splice::remove_entry(text, map, at));
    }
    // The event was the last one of a settings file: the `hooks` key goes too.
    let Kind::Object(keys) = &doc.root.kind else {
        return Err(shape());
    };
    let at = keys
        .iter()
        .position(|m| m.key == "hooks")
        .ok_or(ApiError::NotFound)?;
    Ok(splice::remove_entry(text, &doc.root, at))
}

/// Replaces the handler in place.
pub(super) fn replace(text: &str, pos: &Pos, handler: &Value) -> Result<String, ApiError> {
    let doc = Doc::new(text)?;
    let mut path = pos.handlers_path();
    path.push(Seg::Index(pos.handler));
    let node = doc.at(&path).ok_or(ApiError::NotFound)?;
    Ok(splice::replace(text, node, handler))
}

/// Sets or clears the `matcher` of the group at `pos`.
pub(super) fn set_matcher(
    text: &str,
    pos: &Pos,
    matcher: Option<&str>,
) -> Result<String, ApiError> {
    let doc = Doc::new(text)?;
    let group = doc.at(&pos.group_path()).ok_or(ApiError::NotFound)?;
    let Kind::Object(members) = &group.kind else {
        return Err(shape());
    };
    let at = members.iter().position(|m| m.key == "matcher");
    Ok(match (at, matcher) {
        (Some(at), Some(m)) => splice::replace(text, &members[at].value, &json!(m)),
        (Some(at), None) => splice::remove_entry(text, group, at),
        (None, Some(m)) => splice::object_insert(text, group, Some(0), "matcher", &json!(m)),
        (None, None) => text.to_owned(),
    })
}

/// Adds a handler to `event` in the event map at `map`.
///
/// `restore_at` is the handler's index among the event's handlers when the hook is put back, and
/// it picks the place: inside the group that has the same matcher and holds that index, else in
/// a new group at that index. Without it the handler joins the first group that has the same
/// matcher, else a new group at the end.
pub(super) fn insert(
    text: &str,
    map: &str,
    event: &str,
    matcher: Option<&str>,
    handler: &Value,
    restore_at: Option<usize>,
) -> Result<String, ApiError> {
    let doc = Doc::new(text)?;
    let new_group = match matcher {
        Some(m) => json!({"matcher": m, "hooks": [handler]}),
        None => json!({"hooks": [handler]}),
    };
    let map_path = map_path(map);
    let Some(map_node) = doc.at(&map_path) else {
        // No event map yet: `hooks` of a settings file, created whole.
        if map != "/hooks" {
            return Err(shape());
        }
        let events = json!({ event: [new_group] });
        return Ok(splice::object_insert(
            text, &doc.root, None, "hooks", &events,
        ));
    };
    if !matches!(map_node.kind, Kind::Object(_)) {
        return Err(shape());
    }
    let mut event_path = map_path.clone();
    event_path.push(Seg::Key(event.to_owned()));
    let Some(groups) = doc.array(&event_path)? else {
        if doc.at(&event_path).is_some() {
            return Err(shape());
        }
        return Ok(splice::object_insert(
            text,
            map_node,
            None,
            event,
            &json!([new_group]),
        ));
    };
    let items = Doc::items(groups);
    let same_matcher = |group: &Node| {
        group
            .find(&[Seg::Key("matcher".to_owned())])
            .map(|n| doc.value(n))
            .and_then(|v| v.as_str().map(str::to_owned))
            .as_deref()
            == matcher
    };
    let mut start = 0usize;
    let mut target: Option<(usize, usize)> = None;
    let mut new_at = items.len();
    let mut found_new_at = false;
    for (index, group) in items.iter().enumerate() {
        let len = handlers_in(group);
        match restore_at {
            None => {
                if target.is_none() && same_matcher(group) {
                    target = Some((index, len));
                }
            }
            Some(at) => {
                if target.is_none() && same_matcher(group) && start <= at && at <= start + len {
                    target = Some((index, at - start));
                }
                if !found_new_at {
                    if start >= at {
                        new_at = index;
                        found_new_at = true;
                    } else if start + len > at {
                        new_at = index + 1;
                        found_new_at = true;
                    }
                }
            }
        }
        start += len;
    }
    match target {
        Some((group, at)) => {
            let handlers = items[group]
                .find(&[Seg::Key("hooks".to_owned())])
                .filter(|h| matches!(h.kind, Kind::Array(_)))
                .ok_or_else(shape)?;
            Ok(splice::array_insert(text, handlers, Some(at), handler))
        }
        None => Ok(splice::array_insert(text, groups, Some(new_at), &new_group)),
    }
}
