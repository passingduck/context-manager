//! Auto memory (spec 2): the directory, the project key, the cut of `MEMORY.md`, topic files
//! and subagent memory stores.

use std::collections::BTreeSet;

use serde_json::Value;

use super::extensions::ExtOut;
use super::instructions::{Collector, instructions_off};
use super::setup::{SettingsKind, Setup};
use crate::agents::{FileClaim, ScanCx};
use crate::md::frontmatter;
use crate::model::*;
use crate::probe_fs::EntryKind;
use crate::tokens;

/// Spec 2.4: `HW = 200`, `Rce = 25000`.
pub(super) const INDEX_MAX_LINES: u64 = 200;
pub(super) const INDEX_MAX_CHARS: u64 = 25_000;

// ---------------------------------------------------------------------------------------------
// The project key (spec 2.3)
// ---------------------------------------------------------------------------------------------

/// Spec 2.3: every UTF-16 unit outside `[A-Za-z0-9]` becomes `-`. Over 200 units, the first
/// 200 plus `-` plus the base 36 of the absolute 32-bit Java string hash of the raw path.
pub(super) fn project_key(path: &str) -> String {
    let units: Vec<u16> = path.encode_utf16().collect();
    let sanitized: String = units
        .iter()
        .map(|&u| match u {
            0x30..=0x39 | 0x41..=0x5a | 0x61..=0x7a => u as u8 as char,
            _ => '-',
        })
        .collect();
    if units.len() <= 200 {
        return sanitized;
    }
    let mut hash: i32 = 0;
    for &u in &units {
        hash = hash
            .wrapping_shl(5)
            .wrapping_sub(hash)
            .wrapping_add(i32::from(u));
    }
    format!("{}-{}", &sanitized[..200], base36(hash.unsigned_abs()))
}

fn base36(mut n: u32) -> String {
    if n == 0 {
        return "0".to_owned();
    }
    let digits = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let mut out = Vec::new();
    while n > 0 {
        out.push(digits[(n % 36) as usize]);
        n /= 36;
    }
    out.reverse();
    String::from_utf8(out).expect("ASCII digits")
}

// ---------------------------------------------------------------------------------------------
// The cut (spec 2.4)
// ---------------------------------------------------------------------------------------------

/// What Claude Code puts in the context for a `MEMORY.md` text, and how much it cut.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct IndexCut {
    pub content: String,
    pub line_count: u64,
    pub char_count: u64,
    /// `Some` when it cut: which limit and the kept and total in that limit's unit.
    pub cut: Option<(Limit, u64, u64)>,
}

/// Spec 2.4, a port of `wno`. Lengths are UTF-16 units, as in the JavaScript.
pub(super) fn cut_index(text: &str) -> IndexCut {
    let trimmed = text.trim();
    let units: Vec<u16> = trimmed.encode_utf16().collect();
    let newline = u16::from(b'\n');
    let count_nl = |s: &[u16]| s.iter().filter(|&&u| u == newline).count() as u64;
    let line_count = count_nl(&units) + 1;
    let char_count = units.len() as u64;
    let too_many_lines = line_count > INDEX_MAX_LINES;
    let too_long = char_count > INDEX_MAX_CHARS;
    if !too_many_lines && !too_long {
        return IndexCut {
            content: trimmed.to_owned(),
            line_count,
            char_count,
            cut: None,
        };
    }
    let mut keep: &[u16] = &units;
    if too_many_lines {
        let mut seen = 0;
        let end = units
            .iter()
            .position(|&u| {
                if u == newline {
                    seen += 1;
                }
                seen == INDEX_MAX_LINES
            })
            .unwrap_or(units.len());
        keep = &units[..end];
    }
    if keep.len() as u64 > INDEX_MAX_CHARS {
        let from = INDEX_MAX_CHARS as usize;
        let at = keep[..=from.min(keep.len() - 1)]
            .iter()
            .rposition(|&u| u == newline);
        keep = &keep[..at.filter(|&z| z > 0).unwrap_or(from)];
    }
    let boundary = units.get(keep.len()) == Some(&newline);
    let kept_lines = if boundary { count_nl(keep) + 1 } else { 0 };
    let after = keep.len() + 1;
    let first_cut: &[u16] = if after <= units.len() {
        let end = units[after..]
            .iter()
            .position(|&u| u == newline)
            .map_or(units.len(), |d| after + d);
        &units[after..end]
    } else {
        &[]
    };
    let first_cut = String::from_utf16_lossy(first_cut).trim().to_owned();
    let what = if kept_lines == 0 {
        format!(
            "everything after the first {} characters of line 1 was cut off",
            keep.len()
        )
    } else {
        let hint = if first_cut.is_empty() {
            String::new()
        } else {
            format!(" (\"{}\")", shorten(&first_cut, 80))
        };
        format!(
            "{} of {line_count} lines were cut off, starting at line {}{hint}",
            line_count - kept_lines,
            kept_lines + 1
        )
    };
    let size = if too_long && !too_many_lines {
        format!(
            "{} (limit: {}) \u{2014} index entries are too long",
            format_size(char_count),
            format_size(INDEX_MAX_CHARS)
        )
    } else if too_many_lines && !too_long {
        format!("{line_count} lines (limit: {INDEX_MAX_LINES})")
    } else {
        format!("{line_count} lines and {}", format_size(char_count))
    };
    let warning = format!(
        "MEMORY.md is {size}. Only part of it was loaded: {what}. Keep index entries to one line under ~200 chars; move detail into topic files."
    );
    let (limit, kept, total) = if too_many_lines {
        (
            Limit {
                kind: LimitKind::IndexLines,
                value: INDEX_MAX_LINES,
                unit: Unit::Lines,
            },
            if boundary { kept_lines } else { 1 },
            line_count,
        )
    } else {
        (
            Limit {
                kind: LimitKind::IndexBytes,
                value: INDEX_MAX_CHARS,
                unit: Unit::Bytes,
            },
            keep.len() as u64,
            char_count,
        )
    };
    IndexCut {
        content: format!("{}\n\n> WARNING: {warning}", String::from_utf16_lossy(keep)),
        line_count,
        char_count,
        cut: Some((limit, kept, total)),
    }
}

/// `an` of the binary: a byte count in bytes, KB, MB or GB, one decimal, `.0` dropped.
fn format_size(n: u64) -> String {
    let kb = n as f64 / 1024.0;
    let fixed = |v: f64, unit: &str| format!("{}{unit}", format!("{v:.1}").trim_end_matches(".0"));
    if kb < 1.0 {
        return format!("{n} bytes");
    }
    if kb < 1024.0 {
        return fixed(kb, "KB");
    }
    let mb = kb / 1024.0;
    if mb < 1024.0 {
        return fixed(mb, "MB");
    }
    fixed(mb / 1024.0, "GB")
}

/// `_U` of the binary: at most `max` characters, cut at a word when that keeps over half.
fn shorten(text: &str, max: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= max {
        return text.to_owned();
    }
    let head: String = chars[..max - 1].iter().collect();
    // `search(/\s\S*$/)`: the last whitespace followed by only non-space characters.
    let cut_at = head
        .char_indices()
        .filter(|(_, c)| c.is_whitespace())
        .map(|(i, _)| i)
        .rfind(|&i| !head[i..].trim_start().contains(char::is_whitespace));
    let word = cut_at.map(|i| head[..i].trim_end().to_owned());
    let kept = match word {
        Some(w) if w.chars().count() > max / 2 => w,
        _ => head.trim_end().to_owned(),
    };
    format!("{kept}\u{2026}")
}

// ---------------------------------------------------------------------------------------------
// The directory (spec 2.2)
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub(super) struct MemDir {
    pub dir: AbsPath,
    /// The settings file that chose it, when one did.
    pub set_in: Option<AbsPath>,
}

/// Spec 2.2. `autoMemoryDirectory` from policy, then local, project (unless the folder is
/// refused), then user. The first source that sets it wins, and a bad value means the default.
pub(super) fn memdir(cx: &ScanCx, setup: &Setup) -> MemDir {
    if let Some(over) = cx
        .env
        .var("CLAUDE_COWORK_MEMORY_PATH_OVERRIDE")
        .and_then(|raw| AbsPath::parse(raw).ok())
    {
        return MemDir {
            dir: over,
            set_in: None,
        };
    }
    let trusted = setup.trust().level != TrustLevel::Untrusted;
    let chosen = setup.settings.iter().find(|file| {
        let allowed = match file.kind {
            SettingsKind::Project | SettingsKind::Local => trusted,
            _ => true,
        };
        allowed
            && file
                .get("autoMemoryDirectory")
                .is_some_and(|v| !v.is_null())
    });
    if let Some(file) = chosen {
        let raw = file.get("autoMemoryDirectory").and_then(Value::as_str);
        if let Some(dir) = raw.and_then(|raw| valid_memdir(raw, &setup.home)) {
            return MemDir {
                dir,
                set_in: Some(file.path.clone()),
            };
        }
    }
    let base = cx
        .env
        .var("CLAUDE_CODE_REMOTE_MEMORY_DIR")
        .and_then(|raw| AbsPath::parse(raw).ok())
        .unwrap_or_else(|| setup.config.clone());
    MemDir {
        dir: base
            .join("projects")
            .join(&project_name(cx, setup))
            .join("memory"),
        set_in: None,
    }
}

/// Spec 2.2: absolute or `~/`; not relative, not `..`, not a bare drive, over 2 characters.
fn valid_memdir(raw: &str, home: &AbsPath) -> Option<AbsPath> {
    if raw.len() < 3 || raw.contains('\0') || raw.split('/').any(|part| part == "..") {
        return None;
    }
    if let Some(rest) = raw.strip_prefix("~/") {
        return Some(home.join(rest));
    }
    AbsPath::parse(raw).ok()
}

/// The `<key>` of spec 2.3: the canonical root of the working copy, sanitized.
pub(super) fn project_name(cx: &ScanCx, setup: &Setup) -> String {
    if setup.config_from_env.is_some()
        && let Some(name) = cx.env.var("CLAUDE_CODE_PROJECT_DIR_NAME")
        && (1..=64).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        return name.to_owned();
    }
    let root = setup
        .project
        .main_root
        .clone()
        .unwrap_or_else(|| setup.cwd.clone());
    let canonical = cx.fs.real_path(&root).unwrap_or(root);
    project_key(canonical.as_str())
}

// ---------------------------------------------------------------------------------------------
// On and off (spec 2.7)
// ---------------------------------------------------------------------------------------------

/// `None` when auto memory is on, else the switch that turned it off.
pub(super) fn memory_off(cx: &ScanCx, setup: &Setup) -> Option<String> {
    match setup.flag(cx, "CLAUDE_CODE_DISABLE_AUTO_MEMORY") {
        Some(true) => return Some("CLAUDE_CODE_DISABLE_AUTO_MEMORY".to_owned()),
        // `=0` forces it on, beating `--bare` and `autoMemoryEnabled: false`.
        Some(false) => {
            return ["CLAUDE_CODE_DISABLE_CLAUDE_MDS", "CLAUDE_CODE_SAFE_MODE"]
                .into_iter()
                .find(|n| setup.flag(cx, n) == Some(true))
                .map(str::to_owned);
        }
        None => {}
    }
    if let Some(name) = instructions_off(setup, cx) {
        return Some(name.to_owned());
    }
    if let Some((Value::Bool(false), _)) = setup.first("autoMemoryEnabled") {
        return Some("autoMemoryEnabled".to_owned());
    }
    if setup.flag(cx, "CLAUDE_CODE_REMOTE") == Some(true)
        && cx.env.var("CLAUDE_CODE_REMOTE_MEMORY_DIR").is_none()
    {
        return Some("CLAUDE_CODE_REMOTE".to_owned());
    }
    None
}

// ---------------------------------------------------------------------------------------------
// The store (spec 2.1, 2.6)
// ---------------------------------------------------------------------------------------------

pub(super) struct MemoryOut {
    pub stores: Vec<MemoryStore>,
    pub diagnostics: Vec<Diagnostic>,
    pub files: Vec<FileClaim>,
    pub off: Option<String>,
    pub dir: MemDir,
}

fn claude_id() -> Option<AgentId> {
    Some(AgentId::builtin("claude"))
}

/// Adds the `MEMORY.md` load and the topic loads of the main store to the collector, right
/// after the instruction files that load at start (spec 1.2: last).
pub(super) fn collect_main(c: &mut Collector, cx: &ScanCx, setup: &Setup) -> MemoryOut {
    let dir = memdir(cx, setup);
    let off = memory_off(cx, setup);
    let mut out = MemoryOut {
        stores: Vec::new(),
        diagnostics: Vec::new(),
        files: Vec::new(),
        off: off.clone(),
        dir: dir.clone(),
    };
    let mut push = |a: &AbsPath, b, c2, d, e, f, g| c.push_load(a, b, c2, d, e, f, g);
    let store = build_store(
        &mut push,
        cx,
        &mut out.diagnostics,
        &mut out.files,
        StoreSpec {
            id: "claude.auto".to_owned(),
            label: "claude.memory.auto",
            dir: dir.dir.clone(),
            scope: Scope::User,
            off: off.clone(),
            when: Activation::SessionStart,
            topics: true,
        },
    );
    out.stores.push(store);
    out
}

struct StoreSpec {
    id: String,
    label: &'static str,
    dir: AbsPath,
    scope: Scope,
    off: Option<String>,
    when: Activation,
    topics: bool,
}

/// Pushes one load and returns its id: the collector's, or a plain list for stores that come
/// after the extensions.
pub(super) type PushLoad<'a> = dyn FnMut(&AbsPath, DocKind, Scope, Via, Activation, LoadStatus, Option<Delivered>) -> LoadId
    + 'a;

fn build_store(
    push: &mut PushLoad,
    cx: &ScanCx,
    diagnostics: &mut Vec<Diagnostic>,
    files: &mut Vec<FileClaim>,
    spec: StoreSpec,
) -> MemoryStore {
    let index_path = spec.dir.join("MEMORY.md");
    files.push(FileClaim {
        path: index_path.clone(),
        policy: EditPolicy::Editable,
    });
    let index_bytes = cx.fs.read(&index_path).ok();
    let index_text = index_bytes
        .as_ref()
        .map(|b| String::from_utf8_lossy(b).into_owned());
    let mut entries = Vec::new();
    let mut index_load = None;

    if let Some(text) = &index_text {
        entries.push(MemoryEntry {
            file: index_path.clone(),
            role: MemoryRole::Index,
            indexed: None,
            title: "MEMORY.md".to_owned(),
            description: None,
        });
        let cut = cut_index(text);
        let via = Via::Discovery {
            rule: RuleRef::new("claude.discovery.memory_index"),
        };
        if let Some(by) = &spec.off {
            push(
                &index_path,
                DocKind::MemoryIndex,
                spec.scope.clone(),
                via,
                spec.when.clone(),
                LoadStatus::Skipped {
                    skip: Skip::FeatureOff { name: by.clone() },
                },
                None,
            );
        } else if cut.content.is_empty() {
            push(
                &index_path,
                DocKind::MemoryIndex,
                spec.scope.clone(),
                via,
                spec.when.clone(),
                LoadStatus::Skipped { skip: Skip::Empty },
                None,
            );
        } else {
            let status = match &cut.cut {
                Some((limit, kept, total)) => LoadStatus::Truncated {
                    limit: limit.clone(),
                    kept: *kept,
                    total: *total,
                },
                None => LoadStatus::Loaded,
            };
            if let Some((limit, _, _)) = &cut.cut {
                diagnostics.push(Diagnostic {
                    severity: Severity::Warning,
                    agent: claude_id(),
                    at: Subject::File {
                        path: index_path.clone(),
                        line: None,
                    },
                    problem: Problem::IndexOverLimit {
                        limit: limit.clone(),
                    },
                    fix: None,
                });
            }
            let delivered = Delivered {
                bytes: cut.content.len() as u64,
                tokens: tokens::count(&cut.content),
                transformed: false,
            };
            index_load = Some(push(
                &index_path,
                DocKind::MemoryIndex,
                spec.scope.clone(),
                via,
                spec.when.clone(),
                status,
                Some(delivered),
            ));
        }
    }
    let _ = index_load;

    // Topic files: every other `.md` of the directory (spec 2.1). The model reads them when it
    // decides to.
    let linked = index_text.as_deref().map(link_targets).unwrap_or_default();
    let mut topic_names = BTreeSet::new();
    if let Ok(list) = cx.fs.list(&spec.dir) {
        for entry in list.iter() {
            if entry.kind == EntryKind::File
                && entry.name.ends_with(".md")
                && entry.name != "MEMORY.md"
            {
                topic_names.insert(entry.name.clone());
            }
        }
    }
    for name in &topic_names {
        let path = spec.dir.join(name);
        let Ok(bytes) = cx.fs.read(&path) else {
            continue;
        };
        let text = String::from_utf8_lossy(&bytes);
        let fm = frontmatter::parse(&text);
        let get = |key: &str| {
            fm.map
                .as_ref()
                .and_then(|m| m.get(key))
                .and_then(Value::as_str)
                .map(str::to_owned)
        };
        let title = get("name").unwrap_or_else(|| name.trim_end_matches(".md").to_owned());
        let description = get("description");
        let indexed = index_text
            .is_some()
            .then(|| linked.iter().any(|t| t.target == *name));
        if spec.topics {
            let delivered = Delivered {
                bytes: bytes.len() as u64,
                tokens: tokens::count(&text),
                transformed: false,
            };
            push(
                &path,
                DocKind::MemoryTopic,
                spec.scope.clone(),
                Via::Discovery {
                    rule: RuleRef::new("claude.discovery.memory_topic"),
                },
                Activation::ModelMayRead,
                match &spec.off {
                    Some(by) => LoadStatus::Skipped {
                        skip: Skip::FeatureOff { name: by.clone() },
                    },
                    None => LoadStatus::Loaded,
                },
                spec.off.is_none().then_some(delivered),
            );
        }
        if spec.topics && indexed != Some(true) {
            diagnostics.push(not_in_index(
                cx,
                &spec.dir,
                &index_path,
                index_bytes.as_deref(),
                name,
                &title,
                description.as_deref(),
                &path,
            ));
        }
        entries.push(MemoryEntry {
            file: path,
            role: MemoryRole::Topic,
            indexed,
            title,
            description,
        });
    }

    if spec.topics
        && let (Some(text), Some(bytes)) = (&index_text, &index_bytes)
    {
        for link in &linked {
            if !link.local || topic_names.contains(&link.target) {
                continue;
            }
            let target_path = spec.dir.join(&link.target);
            if cx.fs.stat(&target_path).is_ok() {
                continue;
            }
            diagnostics.push(Diagnostic {
                severity: Severity::Warning,
                agent: claude_id(),
                at: Subject::File {
                    path: index_path.clone(),
                    line: Some(link.line),
                },
                problem: Problem::IndexLinkMissing {
                    target: link.target.clone(),
                },
                fix: Some(Edit::Save {
                    path: index_path.clone(),
                    base: Base::Rev {
                        rev: Revision::of(bytes),
                    },
                    text: without_line(text, link.line),
                }),
            });
        }
    }

    MemoryStore {
        id: spec.id,
        label: RuleRef::new(spec.label),
        root: spec.dir,
        enabled: match spec.off {
            Some(by) => Enabled::Off { by },
            None => Enabled::On,
        },
        index: Some(index_path),
        entries,
        write: MemoryWrite::Direct,
    }
}

#[allow(clippy::too_many_arguments)]
fn not_in_index(
    _cx: &ScanCx,
    _dir: &AbsPath,
    index_path: &AbsPath,
    index_bytes: Option<&Vec<u8>>,
    file_name: &str,
    title: &str,
    description: Option<&str>,
    topic: &AbsPath,
) -> Diagnostic {
    let hook = description.filter(|d| !d.is_empty()).unwrap_or(title);
    let line = format!("- [{title}]({file_name}) \u{2014} {hook}\n");
    let (base, text) = match index_bytes {
        Some(bytes) => {
            let mut text = String::from_utf8_lossy(bytes).into_owned();
            if !text.is_empty() && !text.ends_with('\n') {
                text.push('\n');
            }
            text.push_str(&line);
            (
                Base::Rev {
                    rev: Revision::of(bytes),
                },
                text,
            )
        }
        None => (Base::Absent, line),
    };
    Diagnostic {
        severity: Severity::Info,
        agent: claude_id(),
        at: Subject::File {
            path: topic.clone(),
            line: None,
        },
        problem: Problem::MemoryNotInIndex {
            index: index_path.clone(),
        },
        fix: Some(Edit::Save {
            path: index_path.clone(),
            base,
            text,
        }),
    }
}

/// A link in the index and the line it is on.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Link {
    target: String,
    line: u32,
    /// A relative path, not a URL or an anchor.
    local: bool,
}

/// `[Title](target.md)` links of an index, by line.
fn link_targets(text: &str) -> Vec<Link> {
    let mut out = Vec::new();
    for (n, line) in text.lines().enumerate() {
        let mut rest = line;
        while let Some(at) = rest.find("](") {
            let after = &rest[at + 2..];
            let Some(end) = after.find(')') else { break };
            let raw = after[..end].split('#').next().unwrap_or("").trim();
            let local = !raw.is_empty()
                && !raw.contains("://")
                && !raw.starts_with("mailto:")
                && !raw.starts_with('/')
                && raw.ends_with(".md");
            if local {
                out.push(Link {
                    target: raw.trim_start_matches("./").to_owned(),
                    line: n as u32 + 1,
                    local,
                });
            }
            rest = &after[end..];
        }
    }
    out
}

fn without_line(text: &str, line: u32) -> String {
    text.split_inclusive('\n')
        .enumerate()
        .filter(|(i, _)| *i as u32 + 1 != line)
        .map(|(_, l)| l)
        .collect()
}

// ---------------------------------------------------------------------------------------------
// Subagent memory (spec 2.5)
// ---------------------------------------------------------------------------------------------

/// `memory: user | project | local` in an agent's frontmatter gives it a store. Stores are
/// listed even when the directory does not exist yet.
pub(super) fn agent_stores(
    push: &mut PushLoad,
    cx: &ScanCx,
    setup: &Setup,
    ext: &ExtOut,
    off: &Option<String>,
) -> (Vec<MemoryStore>, Vec<Diagnostic>, Vec<FileClaim>) {
    let mut stores = Vec::new();
    let mut diagnostics = Vec::new();
    let mut files = Vec::new();
    let base = cx
        .env
        .var("CLAUDE_CODE_REMOTE_MEMORY_DIR")
        .and_then(|raw| AbsPath::parse(raw).ok())
        .unwrap_or_else(|| setup.config.clone());
    let mut seen = BTreeSet::new();
    for extension in &ext.extensions {
        if extension.ext_kind != ExtKind::Subagent
            || matches!(
                extension.status,
                ExtStatus::Rejected { .. }
                    | ExtStatus::Shadowed { .. }
                    | ExtStatus::Disabled { .. }
            )
        {
            continue;
        }
        let Ok(bytes) = cx.fs.read(&extension.file) else {
            continue;
        };
        let fm = frontmatter::parse(&String::from_utf8_lossy(&bytes));
        let Some(kind) = fm
            .map
            .as_ref()
            .and_then(|m| m.get("memory"))
            .and_then(Value::as_str)
        else {
            continue;
        };
        let name = sanitize_agent(&extension.name);
        let (dir, label) = match kind {
            "user" => (
                base.join("agent-memory").join(&name),
                "claude.memory.agent_user",
            ),
            "project" => (
                setup.cwd.join(".claude/agent-memory").join(&name),
                "claude.memory.agent_project",
            ),
            "local" => (
                setup.cwd.join(".claude/agent-memory-local").join(&name),
                "claude.memory.agent_local",
            ),
            _ => continue,
        };
        if !seen.insert(dir.clone()) {
            continue;
        }
        let store = build_store(
            push,
            cx,
            &mut diagnostics,
            &mut files,
            StoreSpec {
                id: format!("claude.agent:{}", extension.name),
                label,
                dir,
                scope: extension.scope.clone(),
                off: off.clone(),
                when: Activation::OnInvoke,
                topics: false,
            },
        );
        stores.push(store);
    }
    (stores, diagnostics, files)
}

/// Spec 2.5: characters outside `[A-Za-z0-9_-]` become `-`, an empty name is `unknown`.
fn sanitize_agent(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect();
    if cleaned.is_empty() {
        "unknown".to_owned()
    } else {
        cleaned
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_replaces_every_non_alphanumeric_unit() {
        assert_eq!(project_key("/Users/me/my_app"), "-Users-me-my-app");
        // One dash per UTF-16 unit: a letter outside the BMP is two units.
        assert_eq!(project_key("/a/\u{00fc}"), "-a--");
        assert_eq!(project_key("/a/\u{1F600}"), "-a---");
    }

    #[test]
    fn key_of_a_long_path_ends_in_the_java_hash() {
        let long = format!("/{}", "ab/".repeat(80));
        let key = project_key(&long);
        assert_eq!(&key[..200], &long.replace('/', "-")[..200]);
        assert_eq!(key.as_bytes()[200], b'-');
        assert!(key.len() > 201);
    }

    #[test]
    fn the_index_is_cut_by_lines_with_the_binarys_warning() {
        let text: String = (1..=250)
            .map(|i| format!("- MARK-MEM-LINE-{i:03}\n"))
            .collect();
        let cut = cut_index(&text);
        assert!(cut.content.starts_with("- MARK-MEM-LINE-001\n"));
        assert!(cut.content.ends_with(
            "\n\n> WARNING: MEMORY.md is 250 lines (limit: 200). Only part of it was loaded: 50 of 250 lines were cut off, starting at line 201 (\"- MARK-MEM-LINE-201\"). Keep index entries to one line under ~200 chars; move detail into topic files."
        ));
        let (limit, kept, total) = cut.cut.unwrap();
        assert_eq!((limit.kind, kept, total), (LimitKind::IndexLines, 200, 250));
    }

    #[test]
    fn the_index_is_cut_by_bytes_at_a_line_boundary() {
        let text: String = (1..=150)
            .map(|i| format!("- MARK-BYTES-{i:03} {}\n", "x".repeat(298 - 18)))
            .collect();
        let cut = cut_index(&text);
        let kept_lines = cut
            .content
            .split("\n\n> WARNING")
            .next()
            .unwrap()
            .lines()
            .count();
        assert_eq!(kept_lines, 83);
        assert!(cut.content.contains("MEMORY.md is 43.7KB (limit: 24.4KB) \u{2014} index entries are too long. Only part of it was loaded: 67 of 150 lines were cut off, starting at line 84 (\"- MARK-BYTES-084 xxx"));
        assert_eq!(cut.cut.unwrap().0.kind, LimitKind::IndexBytes);
    }

    #[test]
    fn an_index_under_both_limits_is_only_trimmed() {
        let cut = cut_index("\n- a\n- b\n\n");
        assert_eq!(cut.content, "- a\n- b");
        assert!(cut.cut.is_none());
    }

    #[test]
    fn index_links_name_local_markdown_files_only() {
        let links = link_targets(
            "- [A](a.md) \u{2014} x\n- [B](https://e.com/b.md)\n- [C](./c.md#top) y\n- [D](d.txt)\n",
        );
        let named: Vec<_> = links.iter().map(|l| (l.target.as_str(), l.line)).collect();
        assert_eq!(named, [("a.md", 1), ("c.md", 3)]);
    }

    /// The port is checked against `tools/oracle/claude/project_key.py`, the tested reference of
    /// the research. The test skips when `python3` is missing.
    #[test]
    fn the_key_equals_the_python_reference_on_the_research_cases() {
        let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tools/oracle/claude/project_key.py");
        let long = format!(
            "/Users/me/{} \u{fc}n\u{ef}c\u{f8}d\u{e9}/{}",
            "very long directory ".repeat(9),
            "x".repeat(10)
        );
        let cases = [
            "/Users/me/my_app".to_owned(),
            "/private/tmp/claude-501/-Users-x/ccm-exp/base/top/repo".to_owned(),
            "/a/\u{1F600}/b".to_owned(),
            long,
            format!("/{}", "y".repeat(200)),
            format!("/{}", "y".repeat(199)),
            format!("/{}", "\u{1F600}".repeat(120)),
        ];
        for case in cases {
            let Ok(out) = std::process::Command::new("python3")
                .args(["-I"])
                .arg(&script)
                .arg(&case)
                .output()
            else {
                eprintln!("SKIP: python3 is not installed");
                return;
            };
            assert!(out.status.success(), "reference failed on {case:?}");
            let want = String::from_utf8(out.stdout).unwrap();
            assert_eq!(project_key(&case), want.trim(), "{case:?}");
        }
    }
}
