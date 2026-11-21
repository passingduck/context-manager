//! Claude Code's session records, specified in `docs/agents/claude-code-sessions.md`: the list of
//! the sessions that started in a directory, and one session or one subagent of it read into a
//! [`TraceData`] (design section 14).

use crate::agents::{ScanCx, home_with_env};
use crate::model::*;
use crate::probe_fs::{EntryKind, ProbeFs};
use crate::trace::TraceData;
use crate::walk::{self, Listed, TreeLimits};

use super::memory::project_key;

mod canon;
mod label;
mod read;
mod record;

use record::{Line, MessageRec, SubagentMeta, get};

/// The session list reads this much of the start of each record first, then [`HEAD_MAX`] when
/// the first `cwd` is not in it.
const HEAD_FIRST: u64 = 256 * 1024;
const HEAD_MAX: u64 = 4 * 1024 * 1024;
/// The session list reads this much of the end of each record, for the titles and the last time.
const TAIL: u64 = 64 * 1024;
/// A title read from the first prompt keeps at most this many characters.
const TITLE_CHARS: usize = 200;
/// Spec section 1.2: a project key past 200 characters is cut and given a hash, so only the first
/// 200 compare as a prefix.
const KEY_PREFIX: usize = 200;
/// How many entries a listing of the projects folder, of one project folder or of a subagents
/// folder yields at most.
const MAX_LISTED: usize = 20_000;

/// Spec section 1.2: every session whose first `cwd` is the working directory or below it.
pub(super) fn list(cx: &ScanCx, agent: &AgentId) -> Vec<TraceInfo> {
    let cwd = real_cwd(cx);
    records(cx, &cwd)
        .into_iter()
        .filter_map(|(id, file)| info(cx.fs, agent, id, file, &cwd))
        .collect()
}

/// One session of [`list`], or one subagent of it, whole or only its first `prefix` bytes. `id`
/// and `part` are compared with the names the listings return and never joined into a path.
pub(super) fn open(
    cx: &ScanCx,
    agent: &AgentId,
    id: &TraceId,
    part: Option<&str>,
    prefix: Option<u64>,
) -> Result<TraceData, ApiError> {
    let cwd = real_cwd(cx);
    let (id, file) = records(cx, &cwd)
        .into_iter()
        .find(|(name, _)| *name == id.0)
        .ok_or(ApiError::NotFound)?;
    let mut info = info(cx.fs, agent, id, file, &cwd).ok_or(ApiError::NotFound)?;
    let session_dir = info
        .file
        .parent()
        .expect("a listed file has a folder")
        .join(info.id.0.as_str());
    let parts = parts(cx.fs, &session_dir);
    let Some(part) = part else {
        info.bytes = cx.fs.stat(&info.file)?.len;
        let mut data = read::read(cx.fs, info, prefix)?;
        data.parts = parts.into_iter().map(|(part, _)| part).collect();
        return Ok(data);
    };
    let (found, file) = parts
        .into_iter()
        .find(|(found, _)| found.part == part)
        .ok_or(ApiError::NotFound)?;
    info.bytes = cx.fs.stat(&file)?.len;
    info.file = file;
    info.title = found.description.or(found.label);
    read::read(cx.fs, info, prefix)
}

/// One record read whole, without the session list.
pub(super) fn read_file(
    fs: &ProbeFs,
    agent: &AgentId,
    file: &AbsPath,
) -> Result<TraceData, ApiError> {
    let stem = file.file_name().and_then(|n| n.strip_suffix(".jsonl"));
    let info = TraceInfo {
        agent: agent.clone(),
        id: TraceId(stem.unwrap_or_default().to_owned()),
        file: file.clone(),
        bytes: fs.stat(file)?.len,
        title: None,
        started: None,
        last: None,
        cwd: None,
    };
    read::read(fs, info, None)
}

/// The working directory as Claude Code names it: with its links resolved (spec section 1.2).
fn real_cwd(cx: &ScanCx) -> AbsPath {
    cx.fs
        .real_path(&cx.env.cwd)
        .unwrap_or_else(|_| cx.env.cwd.clone())
}

fn limits() -> TreeLimits {
    TreeLimits {
        depth: 1,
        entries: MAX_LISTED,
    }
}

/// Spec section 1.2: the records in every project folder whose name starts with the key of
/// `cwd`. A session started below `cwd`, in a subdirectory or a nested worktree, has a key that
/// starts with it. Two directories can share a key, so [`info`] confirms each with its `cwd`.
fn records(cx: &ScanCx, cwd: &AbsPath) -> Vec<(String, AbsPath)> {
    let config = home_with_env(cx, "CLAUDE_CONFIG_DIR", ".claude", false).path;
    let projects = config.join("projects");
    let key: String = project_key(cwd.as_str()).chars().take(KEY_PREFIX).collect();
    let mut out = Vec::new();
    let folders = walk::list_tree_with(cx.fs, &projects, limits(), |_| false).entries;
    for folder in folders {
        if folder.kind != EntryKind::Dir || !folder.name().starts_with(&key) {
            continue;
        }
        for entry in walk::list_tree_with(cx.fs, &folder.path, limits(), |_| false).entries {
            if entry.kind != EntryKind::File {
                continue;
            }
            if let Some(stem) = entry.name().strip_suffix(".jsonl") {
                out.push((stem.to_owned(), entry.path.clone()));
            }
        }
    }
    out
}

/// What the head and the tail of one record say (spec section 12), or `None` when its first
/// `cwd` is not `cwd` or below it.
fn info(
    fs: &ProbeFs,
    agent: &AgentId,
    id: String,
    file: AbsPath,
    cwd: &AbsPath,
) -> Option<TraceInfo> {
    let mut head = fs.read_head(&file, HEAD_FIRST).ok()?;
    let mut seen = Seen::default();
    seen.read(&head.bytes, (head.bytes.len() as u64) < head.len, false);
    if seen.cwd.is_none() && head.len > HEAD_FIRST {
        head = fs.read_head(&file, HEAD_MAX).ok()?;
        seen = Seen::default();
        seen.read(&head.bytes, (head.bytes.len() as u64) < head.len, false);
    }
    let started_in = seen.cwd.clone()?;
    if started_in != *cwd && !started_in.starts_with(cwd) {
        return None;
    }
    let mut end = Seen::default();
    if head.len > head.bytes.len() as u64 {
        let tail = fs.read_tail(&file, TAIL).ok()?;
        end.read(&tail.bytes, false, tail.len > tail.bytes.len() as u64);
    }
    let title = end
        .custom_title
        .or(seen.custom_title)
        .or(end.ai_title)
        .or(seen.ai_title)
        .or(end.summary)
        .or(seen.summary)
        .or(seen.first_prompt);
    Some(TraceInfo {
        agent: agent.clone(),
        id: TraceId(id),
        file,
        bytes: head.len,
        title,
        started: seen.first_time,
        last: end.last_time.or(seen.last_time),
        cwd: Some(started_in),
    })
}

/// What a part of a record says about the session.
#[derive(Default)]
struct Seen {
    cwd: Option<AbsPath>,
    first_time: Option<String>,
    last_time: Option<String>,
    custom_title: Option<String>,
    ai_title: Option<String>,
    summary: Option<String>,
    first_prompt: Option<String>,
}

impl Seen {
    /// `cut_end`: the bytes stop inside the record, so the last line may be cut. `cut_start`: they
    /// start inside it, so the first line may be.
    fn read(&mut self, bytes: &[u8], cut_end: bool, cut_start: bool) {
        let mut lines: Vec<&[u8]> = bytes.split(|&b| b == b'\n').collect();
        if cut_end {
            lines.pop();
        }
        if cut_start && !lines.is_empty() {
            lines.remove(0);
        }
        for raw in lines {
            let Ok(line) = serde_json::from_slice::<Line>(raw) else {
                continue;
            };
            self.line(&line);
        }
    }

    fn line(&mut self, line: &Line) {
        if let Some(time) = get::<String>(line.timestamp) {
            if self.first_time.is_none() {
                self.first_time = Some(time.clone());
            }
            self.last_time = Some(time);
        }
        let ty = line.ty();
        match ty.as_deref() {
            Some("custom-title") => {
                self.custom_title = get(line.custom_title).or(self.custom_title.take())
            }
            Some("ai-title") => self.ai_title = get(line.ai_title).or(self.ai_title.take()),
            Some("summary") => self.summary = get(line.summary).or(self.summary.take()),
            _ => {}
        }
        if self.cwd.is_none() && matches!(ty.as_deref(), Some("user" | "assistant" | "attachment"))
        {
            self.cwd = get::<String>(line.cwd).and_then(|c| AbsPath::parse(&c).ok());
        }
        if self.first_prompt.is_none() && ty.as_deref() == Some("user") && !Line::flag(line.is_meta)
        {
            self.first_prompt = get::<MessageRec>(line.message)
                .and_then(|m| m.content)
                .and_then(|content| prompt_text(&content))
                .map(|text| text.chars().take(TITLE_CHARS).collect());
        }
    }
}

/// The text the operator typed: a string content, or the first text block. Text the CLI wrote
/// in a tag, such as a command or a caveat, is not a prompt.
fn prompt_text(content: &serde_json::Value) -> Option<String> {
    let text = match content {
        serde_json::Value::String(text) => text.as_str(),
        serde_json::Value::Array(blocks) => blocks.iter().find_map(|b| {
            (b.get("type")?.as_str()? == "text")
                .then(|| b.get("text")?.as_str())
                .flatten()
        })?,
        _ => return None,
    };
    let text = text.trim();
    (!text.is_empty() && !text.starts_with('<')).then(|| text.to_owned())
}

/// Spec sections 1.1 and 9: `subagents/agent-<id>.jsonl`, also one folder down, each with its
/// `meta.json`.
fn parts(fs: &ProbeFs, session_dir: &AbsPath) -> Vec<(PartInfo, AbsPath)> {
    let dir = session_dir.join("subagents");
    let limits = TreeLimits {
        depth: 2,
        entries: MAX_LISTED,
    };
    let entries =
        walk::list_tree_with(fs, &dir, limits, |e: &Listed| e.kind == EntryKind::Dir).entries;
    let mut out = Vec::new();
    for entry in entries {
        if entry.kind != EntryKind::File {
            continue;
        }
        let Some(part) = entry
            .name()
            .strip_prefix("agent-")
            .and_then(|n| n.strip_suffix(".jsonl"))
        else {
            continue;
        };
        let meta_file = entry
            .path
            .parent()
            .expect("a listed file has a folder")
            .join(&format!("agent-{part}.meta.json"));
        let meta = fs
            .read(&meta_file)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<SubagentMeta>(&bytes).ok());
        let bytes = fs.stat(&entry.path).map_or(0, |s| s.len);
        out.push((
            PartInfo {
                part: part.to_owned(),
                label: meta.as_ref().and_then(|m| m.agent_type.clone()),
                description: meta.and_then(|m| m.description),
                bytes,
            },
            entry.path.clone(),
        ));
    }
    out
}
