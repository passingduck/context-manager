//! Generic diagnostics: problems that look the same for every agent. An agent's own rules are
//! reported by its adapter. Each check here skips what an adapter already reported.

use std::collections::{BTreeMap, BTreeSet};

use crate::agents::{Agent, ScanCx};
use crate::md::spans;
use crate::model::*;
use crate::probe_fs::EntryKind;
use crate::program::{self, Program};
use crate::walk;

/// Files over this size are not parsed for syntax errors.
const MAX_PARSE_BYTES: u64 = 2 * 1024 * 1024;

pub struct Input<'a> {
    pub cx: &'a ScanCx<'a>,
    pub agents: &'a [&'a dyn Agent],
    /// `views[i]` belongs to `agents[i]`.
    pub views: &'a [AgentView],
    pub files: &'a BTreeMap<AbsPath, FileEntry>,
    pub project_root: &'a AbsPath,
    pub walk_capped: bool,
    /// What the adapters reported.
    pub existing: &'a [Diagnostic],
}

pub fn check(input: &Input) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    missing_links(input, &mut out);
    hook_programs(input, &mut out);
    parse_errors(input, &mut out);
    if input.walk_capped
        && !input.existing.iter().any(|d| {
            matches!(&d.problem, Problem::WalkCapped { entries } if *entries as usize == walk::MAX_ENTRIES)
        })
    {
        out.push(Diagnostic {
            severity: Severity::Info,
            agent: None,
            at: Subject::Agent,
            problem: Problem::WalkCapped {
                entries: walk::MAX_ENTRIES as u32,
            },
            fix: None,
        });
    }
    out
}

/// A markdown link in a file the model reads, whose target does not exist.
fn missing_links(input: &Input, out: &mut Vec<Diagnostic>) {
    let mut readers: BTreeMap<&AbsPath, Vec<&AgentId>> = BTreeMap::new();
    for view in input.views {
        for load in &view.loads {
            let file_entry = input.files.get(&load.file);
            if load.delivered.is_some()
                && !matches!(load.via, Via::Listing { .. })
                && file_entry.is_some_and(|entry| entry.format == Format::Markdown)
            {
                let agents = readers.entry(&load.file).or_default();
                if !agents.contains(&&view.agent) {
                    agents.push(&view.agent);
                }
            }
        }
    }
    for (file, agents) in readers {
        let Ok(bytes) = input.cx.fs.read(file) else {
            continue;
        };
        let text = String::from_utf8_lossy(&bytes);
        let found = spans::scan(&text);
        let skip_lines = import_lines(input, file);
        let dir = file.parent().unwrap_or_else(AbsPath::root);
        let mut reported = BTreeSet::new();
        for link in &found.links {
            if found.in_code_or_comment(link.span.range.start)
                || skip_lines.contains(&link.span.line)
            {
                continue;
            }
            let Some(candidates) = link_candidates(&link.target, &dir, input.project_root) else {
                continue;
            };
            if candidates.iter().any(|path| input.cx.fs.stat(path).is_ok())
                || !reported.insert((link.span.line, link.target.clone()))
            {
                continue;
            }
            // The operator cannot fix a reference in a file the tool may not write.
            let severity = match input.files.get(file).map(|entry| &entry.policy) {
                Some(EditPolicy::ReadOnly { .. }) => Severity::Info,
                _ => Severity::Warning,
            };
            out.push(Diagnostic {
                severity,
                agent: match agents.as_slice() {
                    [only] => Some((*only).clone()),
                    _ => None,
                },
                at: Subject::File {
                    path: file.clone(),
                    line: Some(link.span.line),
                },
                problem: Problem::ReferenceMissing {
                    raw: link.target.clone(),
                },
                fix: None,
            });
        }
    }
}

/// The lines of `file` on which any agent found an import.
fn import_lines(input: &Input, file: &AbsPath) -> BTreeSet<u32> {
    let mut lines = BTreeSet::new();
    for view in input.views {
        for load in &view.loads {
            if let Via::Import { host, line, .. } = &load.via
                && view
                    .loads
                    .get(host.0 as usize)
                    .is_some_and(|host| host.file == *file)
            {
                lines.insert(*line);
            }
        }
    }
    lines
}

/// Where a link target would be: beside the file, then from the project root. `None` for a
/// target that does not look like a file path: it has whitespace, or has no `/` and no markdown
/// extension (`[roster](its link)` is prose), or is not local at all.
fn link_candidates(target: &str, dir: &AbsPath, root: &AbsPath) -> Option<Vec<AbsPath>> {
    let path = target.split(['#', '?']).next().unwrap_or("");
    let markdown_name = [".md", ".markdown", ".mdx"]
        .iter()
        .any(|ext| path.to_ascii_lowercase().ends_with(ext));
    if path.is_empty()
        || path.contains(char::is_whitespace)
        || !(path.contains('/') || markdown_name)
        || path.contains("://")
        || path.contains(['{', '}', '$', '*', '<', '>', '|'])
        || path
            .split_once(':')
            .is_some_and(|(scheme, _)| scheme.chars().all(|c| c.is_ascii_alphabetic()))
    {
        return None;
    }
    let path = path.replace("%20", " ");
    let candidates = if let Some(from_root) = path.strip_prefix('/') {
        let mut list = vec![root.join(from_root)];
        list.extend(AbsPath::parse(&path).ok());
        list
    } else {
        vec![dir.join(&path), root.join(&path)]
    };
    Some(candidates)
}

/// A command hook whose program is on no `PATH` entry and is no file, or is a file that cannot
/// run.
fn hook_programs(input: &Input, out: &mut Vec<Diagnostic>) {
    for (agent, view) in input.agents.iter().zip(input.views) {
        for hook in &view.hooks {
            if hook.handler.handler_kind != "command"
                || matches!(hook.state, HookState::Disabled { .. })
            {
                continue;
            }
            let Ok(spec) = agent.exec_spec(hook, &serde_json::Value::Null, input.cx) else {
                continue;
            };
            let command = match spec.argv.as_slice() {
                [shell, flag, text, ..] if shell == "sh" && flag == "-c" => {
                    program::first_word(text, input.cx.env, &spec.env)
                }
                [first, ..] => Some(first.clone()),
                [] => None,
            };
            let Some(word) = command else { continue };
            let problem = match program::resolve(input.cx.env, input.cx.fs, &spec.cwd, &word) {
                Program::NotFound => Problem::HookProgramMissing { program: word },
                Program::Found {
                    path,
                    executable: false,
                } => Problem::HookNotExecutable { path },
                Program::Found { .. } => continue,
            };
            out.push(Diagnostic {
                severity: Severity::Warning,
                agent: Some(view.agent.clone()),
                at: Subject::Hook {
                    hook: hook.id.clone(),
                },
                problem,
                fix: None,
            });
        }
    }
}

/// A JSON, JSONC or TOML file an agent uses that does not parse. The adapters report the files
/// they read themselves, so a path with a `ParseError` already is skipped.
fn parse_errors(input: &Input, out: &mut Vec<Diagnostic>) {
    let reported: BTreeSet<&AbsPath> = input
        .existing
        .iter()
        .filter(|d| matches!(d.problem, Problem::ParseError { .. }))
        .filter_map(|d| match &d.at {
            Subject::File { path, .. } => Some(path),
            _ => None,
        })
        .collect();
    for (path, entry) in input.files {
        if entry.agents.is_empty() || reported.contains(path) {
            continue;
        }
        let FileState::Present { bytes, .. } = entry.state else {
            continue;
        };
        if bytes > MAX_PARSE_BYTES
            || !matches!(entry.format, Format::Json | Format::Jsonc | Format::Toml)
        {
            continue;
        }
        if input
            .cx
            .fs
            .stat(path)
            .is_ok_and(|stat| stat.kind != EntryKind::File)
        {
            continue;
        }
        let Ok(raw) = input.cx.fs.read(path) else {
            continue;
        };
        let Ok(text) = std::str::from_utf8(&raw) else {
            continue;
        };
        let failure = match entry.format {
            Format::Toml => crate::cfg::toml::get(text, &[])
                .err()
                .map(|e| e.to_string()),
            _ => crate::cfg::jsonc::get(text, "")
                .err()
                .map(|e| e.to_string()),
        };
        let Some(detail) = failure else { continue };
        out.push(Diagnostic {
            severity: Severity::Warning,
            agent: match entry.agents.as_slice() {
                [only] => Some(only.clone()),
                _ => None,
            },
            at: Subject::File {
                path: path.clone(),
                line: None,
            },
            problem: Problem::ParseError {
                format: entry.format,
                detail,
                fatal_for_agent: false,
            },
            fix: None,
        });
    }
}
