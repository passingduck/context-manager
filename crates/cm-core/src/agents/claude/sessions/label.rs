//! The kind, label and file of each span of a message that only the request record holds (session
//! spec, section 11; design section 14, "A kind comes from the record").
//!
//! A blob message joins text that attachment records and conversation records hold one by one.
//! Each of those records is a [`Source`]: its text, and the kind its record type gives it. A blob's
//! text block is split where those texts occur in it. What lies between them is a separator
//! (`Frame`) when it is white space, and `Other` when nothing explains it.

use crate::model::{AbsPath, PieceKind};
use crate::trace::Span;

use super::record::{Attachment, Embedded};

/// Section 11: a `system`-role attachment is sent without this wrapper, and a `user`-role one with
/// it, so the wrapper is a frame of its own.
pub(super) const REMINDER_OPEN: &str = "<system-reminder>\n";
pub(super) const REMINDER_CLOSE: &str = "\n</system-reminder>";

/// Text a record says was sent, with the kind its record gives it.
#[derive(Debug, Clone)]
pub(super) struct Source {
    pub text: String,
    pub kind: PieceKind,
    pub label: Option<String>,
    pub file: Option<AbsPath>,
    /// Texts embedded whole in `text`, such as one instruction file each. The rest of `text` is
    /// frame around them.
    pub parts: Vec<Source>,
}

impl Source {
    pub fn new(text: String, kind: PieceKind, label: Option<String>) -> Source {
        Source {
            text,
            kind,
            label,
            file: None,
            parts: Vec::new(),
        }
    }
}

/// The kind of an attachment's rendered text, by its type (section 11's table).
fn attachment_kind(ty: &str) -> PieceKind {
    match ty {
        "instructions" | "nested_memory" => PieceKind::Instructions,
        "skill_listing"
        | "agent_listing_delta"
        | "deferred_tools_delta"
        | "mcp_instructions_delta" => PieceKind::Catalog,
        "environment" | "date" | "model" | "session_context" => PieceKind::Environment,
        "file" | "compact_file_reference" | "edited_text_file" => PieceKind::FileMention,
        "invoked_skills" => PieceKind::ExtensionBody,
        "queued_command" => PieceKind::UserText,
        "task_reminder"
        | "total_tokens_reminder"
        | "silent_turn_reminder"
        | "budget_usd"
        | "auto_mode"
        | "remote_session_change"
        | "task_status"
        | "read_truncation_notice"
        | "skill_mention" => PieceKind::Reminder,
        t if t.starts_with("hook_") || t == "async_hook_response" => PieceKind::HookOutput,
        _ => PieceKind::Other,
    }
}

fn abs(path: Option<&str>) -> Option<AbsPath> {
    AbsPath::parse(path?).ok()
}

/// The sources of one attachment record: one per rendered piece. `system` says the pieces were
/// sent in a `system`-role message, so without the reminder wrapper.
pub(super) fn attachment_sources(
    attachment: &Attachment,
    nested: Option<String>,
    rendered: Vec<String>,
    system: bool,
) -> Vec<Source> {
    let ty = attachment.ty.as_deref().unwrap_or("");
    let kind = attachment_kind(ty);
    let label = match kind {
        PieceKind::HookOutput => attachment.hook_event.clone(),
        PieceKind::FileMention => attachment.display_path.clone(),
        _ if ty == "skill_mention" => attachment.skill_name.clone(),
        _ => Some(ty.to_owned()),
    };
    let file = abs(attachment.filename.as_deref()).or_else(|| abs(attachment.path.as_deref()));
    let embedded = |list: &Option<Vec<Embedded>>, kind: PieceKind, named: bool| -> Vec<Source> {
        list.iter()
            .flatten()
            .filter_map(|e| {
                let text = e.content.clone().filter(|t| !t.is_empty())?;
                Some(Source {
                    text,
                    kind,
                    label: if named { e.name.clone() } else { e.ty.clone() },
                    file: abs(e.path.as_deref()),
                    parts: Vec::new(),
                })
            })
            .collect()
    };
    let mut parts = embedded(&attachment.files, PieceKind::Instructions, false);
    parts.extend(embedded(&attachment.skills, PieceKind::ExtensionBody, true));
    if let Some(text) = nested.filter(|t| !t.is_empty()) {
        parts.push(Source {
            text,
            kind: PieceKind::Instructions,
            label: None,
            file: file.clone(),
            parts: Vec::new(),
        });
    }
    rendered
        .into_iter()
        .filter_map(|text| {
            let text = if system {
                unwrap_reminder(&text).unwrap_or(&text).to_owned()
            } else {
                text
            };
            (!text.is_empty()).then(|| Source {
                text,
                kind,
                label: label.clone(),
                file: file.clone(),
                parts: parts.clone(),
            })
        })
        .collect()
}

/// The text inside the reminder wrapper, when `text` is wrapped.
fn unwrap_reminder(text: &str) -> Option<&str> {
    let inner = text
        .strip_prefix(REMINDER_OPEN)?
        .strip_suffix(REMINDER_CLOSE)?;
    Some(inner)
}

/// The spans of one blob text block, and the indexes of the sources that explain a part of it.
pub(super) fn split(text: &str, sources: &[&Source]) -> (Vec<Span>, Vec<usize>) {
    let mut found: Vec<(usize, usize, usize)> = sources
        .iter()
        .enumerate()
        .filter(|(_, s)| !s.text.is_empty())
        .filter_map(|(i, s)| text.find(&s.text).map(|at| (at, at + s.text.len(), i)))
        .collect();
    // The longest text wins a place, so a short prompt quoted inside a long attachment does not
    // take the attachment's place.
    found.sort_by(|a, b| (b.1 - b.0).cmp(&(a.1 - a.0)).then(a.0.cmp(&b.0)));
    let mut chosen: Vec<(usize, usize, usize)> = Vec::new();
    for candidate in found {
        if chosen
            .iter()
            .all(|c| candidate.1 <= c.0 || candidate.0 >= c.1)
        {
            chosen.push(candidate);
        }
    }
    chosen.sort_unstable();
    let mut spans = Vec::new();
    let mut at = 0;
    let mut used = Vec::with_capacity(chosen.len());
    for (start, end, i) in chosen {
        used.push(i);
        gap(&mut spans, &text[at..start]);
        spans.extend(source_spans(sources[i]));
        at = end;
    }
    gap(&mut spans, &text[at..]);
    (merge_frames(spans), used)
}

/// Adjacent frames become one piece.
fn merge_frames(spans: Vec<Span>) -> Vec<Span> {
    let mut out: Vec<Span> = Vec::with_capacity(spans.len());
    for span in spans {
        match out.last_mut() {
            Some(last) if last.kind == PieceKind::Frame && span.kind == PieceKind::Frame => {
                last.text.push_str(&span.text)
            }
            _ => out.push(span),
        }
    }
    out
}

/// Text no source explains: a separator when it is white space, else `Other`. A reminder
/// wrapper around it is a frame of its own.
fn gap(spans: &mut Vec<Span>, text: &str) {
    if text.is_empty() {
        return;
    }
    if text.trim().is_empty() {
        spans.push(Span::new(PieceKind::Frame, text));
        return;
    }
    let (open, rest) = match text.strip_prefix(REMINDER_OPEN) {
        Some(rest) => (REMINDER_OPEN, rest),
        None => ("", text),
    };
    let (body, close) = match rest.strip_suffix(REMINDER_CLOSE) {
        Some(body) => (body, REMINDER_CLOSE),
        None => (rest, ""),
    };
    if !open.is_empty() {
        spans.push(Span::new(PieceKind::Frame, open));
    }
    if !body.is_empty() {
        let kind = if body.trim().is_empty() {
            PieceKind::Frame
        } else {
            PieceKind::Other
        };
        spans.push(Span::new(kind, body));
    }
    if !close.is_empty() {
        spans.push(Span::new(PieceKind::Frame, close));
    }
}

/// A source's text as spans: the reminder wrapper and the text around embedded parts are frame.
pub(super) fn source_spans(source: &Source) -> Vec<Span> {
    let own = |text: &str| {
        Span::new(source.kind, text)
            .labelled(source.label.clone())
            .from_file(source.file.clone())
    };
    let (open, body, close) = match unwrap_reminder(&source.text) {
        Some(inner) => (REMINDER_OPEN, inner, REMINDER_CLOSE),
        None => ("", source.text.as_str(), ""),
    };
    let mut spans = Vec::new();
    if !open.is_empty() {
        spans.push(Span::new(PieceKind::Frame, open));
    }
    let mut located = Vec::new();
    let mut at = 0;
    for part in &source.parts {
        if let Some(found) = body[at..].find(&part.text) {
            let start = at + found;
            located.push((start, start + part.text.len(), part));
            at = start + part.text.len();
        }
    }
    if located.is_empty() {
        if !body.is_empty() {
            spans.push(own(body));
        }
    } else {
        let mut at = 0;
        for (start, end, part) in located {
            if start > at {
                spans.push(Span::new(PieceKind::Frame, &body[at..start]));
            }
            spans.push(
                Span::new(part.kind, &body[start..end])
                    .labelled(part.label.clone())
                    .from_file(part.file.clone()),
            );
            at = end;
        }
        if at < body.len() {
            spans.push(Span::new(PieceKind::Frame, &body[at..]));
        }
    }
    if !close.is_empty() {
        spans.push(Span::new(PieceKind::Frame, close));
    }
    spans
}

#[cfg(test)]
mod tests {
    use super::*;

    fn joined(spans: &[Span]) -> String {
        spans.iter().map(|s| s.text.as_str()).collect()
    }

    #[test]
    fn a_system_message_splits_into_one_piece_per_attachment_with_frames_between() {
        let env = Source::new("# Environment\ncwd".into(), PieceKind::Environment, None);
        let date = Source::new("Today is Monday.".into(), PieceKind::Environment, None);
        let skills = Source::new("- a: does a".into(), PieceKind::Catalog, None);
        let text = "# Environment\ncwd\n\n- a: does a\n\nToday is Monday.";
        let sources = [&env, &date, &skills];
        let (spans, mut used) = split(text, &sources);
        used.sort_unstable();
        assert_eq!(joined(&spans), text);
        let kinds: Vec<_> = spans.iter().map(|s| s.kind).collect();
        assert_eq!(
            kinds,
            [
                PieceKind::Environment,
                PieceKind::Frame,
                PieceKind::Catalog,
                PieceKind::Frame,
                PieceKind::Environment
            ]
        );
        assert_eq!(used, [0, 1, 2]);
    }

    #[test]
    fn instruction_files_are_their_own_pieces_inside_the_wrapper() {
        let file = |text: &str, path: &str| Source {
            text: text.into(),
            kind: PieceKind::Instructions,
            label: Some("Project".into()),
            file: AbsPath::parse(path).ok(),
            parts: Vec::new(),
        };
        let rendered = "<system-reminder>\nIntro.\n\nContents of /p/CLAUDE.md (project):\n\nbe brief\n</system-reminder>";
        let source = Source {
            text: rendered.into(),
            kind: PieceKind::Instructions,
            label: None,
            file: None,
            parts: vec![file("be brief", "/p/CLAUDE.md")],
        };
        let block = format!("{rendered}\n");
        let (spans, _) = split(&block, &[&source]);
        assert_eq!(joined(&spans), block);
        let pieces: Vec<_> = spans
            .iter()
            .map(|s| (s.kind, s.file.as_ref().map(|f| f.to_string())))
            .collect();
        assert_eq!(
            pieces,
            [
                (PieceKind::Frame, None),
                (PieceKind::Instructions, Some("/p/CLAUDE.md".into())),
                (PieceKind::Frame, None),
            ]
        );
    }

    #[test]
    fn the_longest_source_wins_and_unexplained_text_is_other() {
        let prompt = Source::new("hello".into(), PieceKind::UserText, None);
        let quote = Source::new("they said hello twice".into(), PieceKind::Reminder, None);
        let text = "they said hello twice; odd";
        let (spans, _) = split(text, &[&prompt, &quote]);
        let kinds: Vec<_> = spans.iter().map(|s| (s.kind, s.text.as_str())).collect();
        assert_eq!(
            kinds,
            [
                (PieceKind::Reminder, "they said hello twice"),
                (PieceKind::Other, "; odd")
            ]
        );
    }
}
