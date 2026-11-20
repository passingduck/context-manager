//! Claude Code skills, commands, subagents and plugins (spec 4). Output styles and MCP server
//! configuration are not extensions in the model and are not read here.

use std::collections::BTreeMap;

use serde_json::Value;

use super::setup::{SettingsKind, Setup};
use crate::agents::{FileClaim, ScanCx};
use crate::limits;
use crate::md::frontmatter;
use crate::model::*;
use crate::probe_fs::EntryKind;
use crate::tokens;
use crate::walk;

pub(super) mod plugins;

use plugins::Plugin;

/// Spec 4, "What reaches the model": a skill, command or agent file over this size is skipped.
const MAX_EXT_FILE_BYTES: u64 = 1_048_576;
/// Spec 4: `skillListingMaxDescChars`, `skillListingBudgetFraction`, 4 chars per token.
const LISTING_MAX_DESC_CHARS: u64 = 1_536;
const LISTING_BUDGET_FRACTION: f64 = 0.01;
const CHARS_PER_TOKEN: f64 = 4.0;
/// Spec 4: agent descriptions over this many tokens give a startup warning.
const AGENT_CATALOG_WARN_TOKENS: u64 = 15_000;
/// Spec 4: `UEs=200000`, the window the catalog budget assumes when none is given.
#[cfg(test)]
pub(super) const DEFAULT_WINDOW_TOKENS: u32 = 200_000;

pub(super) struct ExtOut {
    pub extensions: Vec<Extension>,
    /// Listing and body loads of the extensions. `Load.id` and every `LoadId` inside
    /// `extensions` already count from the `first_load_id` the caller passed.
    pub loads: Vec<Load>,
    pub diagnostics: Vec<Diagnostic>,
    pub files: Vec<FileClaim>,
    /// The plugins found, enabled or not. The hooks half reads their hook files.
    pub plugins: Vec<Plugin>,
}

/// Every discovery location of spec 4. `first_load_id` is the index the first returned load
/// will have in `AgentView.loads`.
#[cfg(test)]
pub(super) fn scan(cx: &ScanCx, setup: &Setup, first_load_id: u32) -> ExtOut {
    scan_with_window(cx, setup, first_load_id, DEFAULT_WINDOW_TOKENS)
}

/// [`scan`] with the context window that sets the skill catalog budget.
pub(super) fn scan_with_window(
    cx: &ScanCx,
    setup: &Setup,
    first_load_id: u32,
    window_tokens: u32,
) -> ExtOut {
    let mut out = ExtOut {
        extensions: Vec::new(),
        loads: Vec::new(),
        diagnostics: Vec::new(),
        files: Vec::new(),
        plugins: Vec::new(),
    };
    // Spec 4, shared loader rules: `--bare` skips discovery of skills, commands, agents and plugins.
    if setup.flag(cx, "CLAUDE_CODE_SIMPLE") == Some(true) {
        return out;
    }
    out.plugins = plugins::discover(cx, setup);
    let cands = collect(cx, setup, &out.plugins);
    let mut builder = Builder {
        cx,
        setup,
        first: first_load_id,
        out,
        window_tokens,
    };
    builder.build(cands);
    builder.out
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Source {
    Managed,
    User,
    Synced,
    Project,
    /// A skills directory below cwd: loads when Claude first touches a file inside it.
    Nested,
    Plugin,
}

struct Cand {
    kind: ExtKind,
    name: String,
    file: AbsPath,
    scope: Scope,
    source: Source,
    /// Lower wins a name clash.
    prio: u32,
    plugin: Option<usize>,
    /// For commands and agents: `name` may not come from frontmatter (commands) or must
    /// (agents outside plugins).
    plugin_prefix: Option<String>,
    nested_dir: Option<AbsPath>,
}

// ---------------------------------------------------------------------------------------------
// Discovery
// ---------------------------------------------------------------------------------------------

fn list_dirs(cx: &ScanCx, dir: &AbsPath) -> Vec<String> {
    walk::list_dir(cx.fs, dir)
        .into_iter()
        .filter(|e| e.kind == EntryKind::Dir)
        .map(|e| e.name().to_owned())
        .collect()
}

fn is_file(cx: &ScanCx, path: &AbsPath) -> bool {
    cx.fs
        .stat(path)
        .map(|s| s.kind == EntryKind::File)
        .unwrap_or(false)
}

/// `.md` files below `dir`, recursive, with the path segments between `dir` and the file.
fn md_files(cx: &ScanCx, dir: &AbsPath) -> Vec<(AbsPath, Vec<String>)> {
    walk::list_tree(cx.fs, dir, |_| true)
        .into_iter()
        .filter(|e| e.kind == EntryKind::File && e.name().ends_with(".md"))
        .map(|mut e| {
            let last = e.rel.pop().unwrap_or_default();
            e.rel.push(last.trim_end_matches(".md").to_owned());
            (e.path, e.rel)
        })
        .collect()
}

/// Spec 4, "Locations" of skills: `<dir>/<name>/SKILL.md`. The folder names `synced` and
/// `anthropic-skills*` are reserved.
fn skill_dirs(cx: &ScanCx, dir: &AbsPath) -> Vec<(String, AbsPath)> {
    list_dirs(cx, dir)
        .into_iter()
        .filter(|n| n != "synced" && !n.starts_with("anthropic-skills") && !n.starts_with('.'))
        .map(|n| (n.clone(), dir.join(&n).join("SKILL.md")))
        .filter(|(_, file)| is_file(cx, file))
        .collect()
}

/// Spec 4, shared loader rules: each `<dir>/.claude/<kind>` from cwd up to the repo root. The
/// walk never includes `$HOME`. Outside git it continues up to home (exclusive) or `/`.
fn project_dirs(cx: &ScanCx, setup: &Setup) -> Vec<AbsPath> {
    let automount: Vec<AbsPath> = cx
        .env
        .var("CLAUDE_CODE_AUTOMOUNT_ROOTS")
        .map(|v| {
            v.split(':')
                .filter_map(|p| AbsPath::parse(p).ok())
                .collect()
        })
        .unwrap_or_default();
    let mut out = Vec::new();
    let mut dir = Some(setup.cwd.clone());
    while let Some(current) = dir {
        if current == AbsPath::root() || current == setup.home {
            break;
        }
        if !automount.contains(&current) {
            out.push(current.clone());
        }
        if setup.project.git_root.as_ref() == Some(&current) {
            break;
        }
        dir = current.parent();
    }
    out
}

fn manifest_paths(value: Option<&Value>) -> Vec<String> {
    match value {
        Some(Value::String(s)) => vec![s.clone()],
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(|v| v.as_str().map(str::to_owned))
            .collect(),
        _ => Vec::new(),
    }
}

fn collect(cx: &ScanCx, setup: &Setup, plugins: &[Plugin]) -> Vec<Cand> {
    let mut cands = Vec::new();
    let safe_mode = setup.flag(cx, "CLAUDE_CODE_SAFE_MODE") == Some(true);
    let project_dirs = if safe_mode {
        Vec::new()
    } else {
        project_dirs(cx, setup)
    };
    let sources: Vec<(Source, Scope, AbsPath, u32)> = {
        let mut v = vec![(
            Source::Managed,
            Scope::Managed,
            setup.managed.join(".claude"),
            0,
        )];
        v.push((Source::User, Scope::User, setup.config.clone(), 50));
        for (i, dir) in project_dirs.iter().enumerate() {
            v.push((
                Source::Project,
                Scope::Project { dir: dir.clone() },
                dir.join(".claude"),
                10 + i as u32,
            ));
        }
        v
    };

    // Skills (spec 4, Skills / Locations).
    let mut project_skills = false;
    for (source, scope, base, prio) in &sources {
        // User beats project for skills and commands, the reverse of agents.
        let prio = if *source == Source::User { &5 } else { prio };
        for (name, file) in skill_dirs(cx, &base.join("skills")) {
            if *source == Source::Project {
                project_skills = true;
            }
            cands.push(Cand {
                kind: ExtKind::Skill,
                name,
                file,
                scope: scope.clone(),
                source: *source,
                prio: *prio,
                plugin: None,
                plugin_prefix: None,
                nested_dir: None,
            });
        }
    }
    // A linked worktree without `.claude/skills` falls back to the main checkout (v2.1.277).
    if !project_skills
        && !safe_mode
        && let (Some(root), Some(main)) = (&setup.project.git_root, &setup.project.main_root)
        && root != main
    {
        for (name, file) in skill_dirs(cx, &main.join(".claude/skills")) {
            cands.push(Cand {
                kind: ExtKind::Skill,
                name,
                file,
                scope: Scope::Project { dir: main.clone() },
                source: Source::Project,
                prio: 40,
                plugin: None,
                plugin_prefix: None,
                nested_dir: None,
            });
        }
    }
    // Synced skills: `<config>/skills/synced/<bucket>/<name>/SKILL.md`, never wins a clash.
    let synced = setup.config.join("skills/synced");
    for bucket in list_dirs(cx, &synced) {
        for (name, file) in skill_dirs(cx, &synced.join(&bucket)) {
            cands.push(Cand {
                kind: ExtKind::Skill,
                name,
                file,
                scope: Scope::User,
                source: Source::Synced,
                prio: 90,
                plugin: None,
                // The full name is `anthropic-skills:<name>`.
                plugin_prefix: Some("anthropic-skills".to_owned()),
                nested_dir: None,
            });
        }
    }
    // Nested skills below cwd load on demand (spec 4, Skills / Locations).
    if !safe_mode {
        let index = walk::walk(&setup.cwd, cx.fs);
        let marker = "/.claude/skills/";
        for file in index.files() {
            let text = file.as_str();
            let Some(at) = text.find(marker) else {
                continue;
            };
            let rest = &text[at + marker.len()..];
            let Some(name) = rest.strip_suffix("/SKILL.md") else {
                continue;
            };
            let host = AbsPath::parse(&text[..at]).unwrap_or_else(|_| setup.cwd.clone());
            if name.contains('/') || host == setup.cwd || !host.starts_with(&setup.cwd) {
                continue;
            }
            cands.push(Cand {
                kind: ExtKind::Skill,
                name: name.to_owned(),
                file: file.clone(),
                scope: Scope::Project { dir: host.clone() },
                source: Source::Nested,
                prio: 100,
                plugin: None,
                plugin_prefix: None,
                nested_dir: Some(host),
            });
        }
    }

    // Commands (spec 4, Custom commands): the name is the path with `:` between segments.
    for (source, scope, base, prio) in &sources {
        let prio = if *source == Source::User { &5 } else { prio };
        for (file, segs) in md_files(cx, &base.join("commands")) {
            cands.push(Cand {
                kind: ExtKind::Command,
                name: segs.join(":"),
                file,
                scope: scope.clone(),
                source: *source,
                prio: *prio,
                plugin: None,
                plugin_prefix: None,
                nested_dir: None,
            });
        }
    }

    // Subagents (spec 4, Subagents / Locations): managed, project closest first, user.
    for (source, scope, base, prio) in &sources {
        for (file, _) in md_files(cx, &base.join("agents")) {
            cands.push(Cand {
                kind: ExtKind::Subagent,
                name: String::new(),
                file,
                scope: scope.clone(),
                source: *source,
                prio: *prio,
                plugin: None,
                plugin_prefix: None,
                nested_dir: None,
            });
        }
    }

    // Plugin components (spec 4, Manifest and component locations).
    for (index, plugin) in plugins.iter().enumerate() {
        let manifest = plugin.manifest.as_ref();
        let root = &plugin.root;
        let mut push = |kind: ExtKind, name: String, file: AbsPath, prefix: Option<String>| {
            cands.push(Cand {
                kind,
                name,
                file,
                scope: plugin.scope(),
                source: Source::Plugin,
                prio: 60,
                plugin: Some(index),
                plugin_prefix: prefix,
                nested_dir: None,
            });
        };
        let mut skill_bases = vec![root.join("skills")];
        for raw in manifest_paths(manifest.and_then(|m| m.get("skills"))) {
            skill_bases.push(root.join(&raw));
        }
        for base in skill_bases {
            if is_file(cx, &base.join("SKILL.md")) {
                let name = if &base == root {
                    plugin.name.clone()
                } else {
                    base.file_name().unwrap_or("skill").to_owned()
                };
                push(
                    ExtKind::Skill,
                    name,
                    base.join("SKILL.md"),
                    Some(plugin.name.clone()),
                );
            } else {
                for (name, file) in skill_dirs(cx, &base) {
                    push(ExtKind::Skill, name, file, Some(plugin.name.clone()));
                }
            }
        }
        // A manifest `commands` key replaces the `commands/` scan.
        let command_entries: Vec<(AbsPath, Vec<String>)> =
            match manifest.and_then(|m| m.get("commands")) {
                None => md_files(cx, &root.join("commands")),
                Some(Value::Object(map)) => map
                    .iter()
                    .filter_map(|(key, v)| {
                        let source = v.get("source")?.as_str()?;
                        Some((root.join(source), vec![key.clone()]))
                    })
                    .collect(),
                Some(other) => manifest_paths(Some(other))
                    .into_iter()
                    .flat_map(|raw| {
                        let path = root.join(&raw);
                        if is_file(cx, &path) {
                            let stem = path
                                .file_name()
                                .unwrap_or("command")
                                .trim_end_matches(".md")
                                .to_owned();
                            vec![(path, vec![stem])]
                        } else {
                            md_files(cx, &path)
                        }
                    })
                    .collect(),
            };
        for (file, segs) in command_entries {
            push(
                ExtKind::Command,
                segs.join(":"),
                file,
                Some(plugin.name.clone()),
            );
        }
        let agent_files: Vec<(AbsPath, Vec<String>)> = match manifest.and_then(|m| m.get("agents"))
        {
            None => md_files(cx, &root.join("agents")),
            Some(other) => manifest_paths(Some(other))
                .into_iter()
                .map(|raw| {
                    let path = root.join(&raw);
                    let stem = path
                        .file_name()
                        .unwrap_or("agent")
                        .trim_end_matches(".md")
                        .to_owned();
                    (path, vec![stem])
                })
                .filter(|(p, _)| is_file(cx, p))
                .collect(),
        };
        for (file, segs) in agent_files {
            push(
                ExtKind::Subagent,
                segs.join(":"),
                file,
                Some(plugin.name.clone()),
            );
        }
    }
    cands
}

// ---------------------------------------------------------------------------------------------
// Building the extensions and their loads
// ---------------------------------------------------------------------------------------------

struct Parsed {
    cand: Cand,
    id: ExtId,
    text: String,
    bytes: u64,
    fm: frontmatter::Frontmatter,
    name: String,
    description: Option<String>,
    rejected: Option<(&'static str, String)>,
}

struct Builder<'a, 'b> {
    cx: &'a ScanCx<'b>,
    setup: &'a Setup,
    first: u32,
    out: ExtOut,
    window_tokens: u32,
}

fn ext_id(kind: ExtKind, file: &AbsPath) -> ExtId {
    let tag = match kind {
        ExtKind::Skill => "skill",
        ExtKind::Command => "command",
        ExtKind::Subagent => "subagent",
    };
    ExtId(sha256_hex(format!("claude\0{tag}\0{file}").as_bytes())[..16].to_owned())
}

/// Spec 4: what a skill or command body and a subagent prompt send when invoked: the file
/// without its frontmatter. The scan counts this text and `load_texts` returns it.
pub(super) fn body_of(text: &str) -> &str {
    &text[frontmatter::parse(text).body]
}

/// Spec 4, "What reaches the model": one catalog line and what the cut did to it. A skill or
/// command entry is `description - when_to_use`, cut at `max_desc` characters. A subagent entry
/// is the whole description and the `(Tools: ...)` suffix.
pub(super) fn listing_entry(
    kind: ExtKind,
    name: &str,
    description: &str,
    fm: &Option<serde_json::Map<String, Value>>,
    max_desc: u64,
) -> (String, LoadStatus) {
    if kind == ExtKind::Subagent {
        return (
            format!("{name}: {description} (Tools: {})", agent_tools(fm)),
            LoadStatus::Loaded,
        );
    }
    let full = match fm_str(fm, "when_to_use") {
        Some(w) => format!("{description} - {w}"),
        None => description.to_owned(),
    };
    let cut = limits::cut_chars(&full, max_desc);
    let status = if cut.is_whole() {
        LoadStatus::Loaded
    } else {
        LoadStatus::Truncated {
            limit: Limit {
                kind: LimitKind::ListingEntryChars,
                value: max_desc,
                unit: Unit::Chars,
            },
            kept: cut.kept,
            total: cut.total,
        }
    };
    (format!("{name}: {}", cut.text), status)
}

/// A frontmatter list that is a comma separated string or a YAML sequence, trimmed, without
/// empty items.
fn fm_names(map: &Option<serde_json::Map<String, Value>>, key: &str) -> Vec<String> {
    let items: Vec<String> = match map.as_ref().and_then(|m| m.get(key)) {
        Some(Value::String(s)) => s.split(',').map(str::to_owned).collect(),
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect(),
        _ => Vec::new(),
    };
    items
        .into_iter()
        .map(|i| i.trim().to_owned())
        .filter(|i| !i.is_empty())
        .collect()
}

/// Spec 4, subagent entry: the text after `Tools:`, as the 2.1.329 capture shows it. No `tools`
/// (or an empty list, or `*`) is `All tools`, with `except` and the `disallowedTools` when
/// those are set. A `tools` list loses the names `disallowedTools` holds, and an empty
/// remainder is `None`.
fn agent_tools(map: &Option<serde_json::Map<String, Value>>) -> String {
    let allowed = fm_names(map, "tools");
    let denied = fm_names(map, "disallowedTools");
    if allowed.is_empty() || allowed == ["*"] {
        return if denied.is_empty() {
            "All tools".to_owned()
        } else {
            format!("All tools except {}", denied.join(", "))
        };
    }
    let kept: Vec<String> = allowed
        .into_iter()
        .filter(|t| !denied.contains(t))
        .collect();
    if kept.is_empty() {
        "None".to_owned()
    } else {
        kept.join(", ")
    }
}

/// The cut length that `skillListingMaxDescChars` sets.
pub(super) fn max_desc_chars(setup: &Setup) -> u64 {
    setup
        .first("skillListingMaxDescChars")
        .and_then(|(v, _)| v.as_u64())
        .unwrap_or(LISTING_MAX_DESC_CHARS)
}

/// The skill listing's character budget: `SLASH_COMMAND_TOOL_CHAR_BUDGET` when set, else
/// `window x 4 x skillListingBudgetFraction`.
pub(super) fn listing_budget_chars(cx: &ScanCx, setup: &Setup, window_tokens: u32) -> u64 {
    if let Some(fixed) = cx
        .env
        .var("SLASH_COMMAND_TOOL_CHAR_BUDGET")
        .and_then(|raw| raw.trim().parse::<u64>().ok())
    {
        return fixed;
    }
    let fraction = setup
        .first("skillListingBudgetFraction")
        .and_then(|(v, _)| v.as_f64())
        .unwrap_or(LISTING_BUDGET_FRACTION);
    (f64::from(window_tokens) * CHARS_PER_TOKEN * fraction) as u64
}

fn fm_str<'a>(map: &'a Option<serde_json::Map<String, Value>>, key: &str) -> Option<&'a str> {
    map.as_ref()?
        .get(key)?
        .as_str()
        .filter(|s| !s.trim().is_empty())
}

/// Spec 4 frontmatter: booleans also accept `yes`, `no`, `on`, `off`, `1` and `0`.
fn fm_bool(map: &Option<serde_json::Map<String, Value>>, key: &str) -> bool {
    match map.as_ref().and_then(|m| m.get(key)) {
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_i64() == Some(1),
        Some(Value::String(s)) => {
            matches!(s.to_ascii_lowercase().as_str(), "yes" | "on" | "1" | "true")
        }
        _ => false,
    }
}

fn fm_list(map: &Option<serde_json::Map<String, Value>>, key: &str) -> Vec<String> {
    match map.as_ref().and_then(|m| m.get(key)) {
        Some(Value::String(s)) => crate::glob::split_paths_string(s),
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(|v| v.as_str().map(str::to_owned))
            .collect(),
        _ => Vec::new(),
    }
}

/// Spec 4: with no `description`, the first non-empty body line, `#` marks stripped, cut at 100.
fn first_line_description(body: &str) -> Option<String> {
    let line = body.lines().map(str::trim).find(|l| !l.is_empty())?;
    let line = line.trim_start_matches('#').trim();
    if line.chars().count() > 100 {
        Some(format!("{}...", line.chars().take(97).collect::<String>()))
    } else {
        Some(line.to_owned())
    }
}

impl Builder<'_, '_> {
    fn next_id(&self) -> LoadId {
        LoadId(self.first + self.out.loads.len() as u32)
    }

    #[allow(clippy::too_many_arguments)]
    fn push_load(
        &mut self,
        file: &AbsPath,
        kind: DocKind,
        scope: &Scope,
        via: Via,
        when: Activation,
        status: LoadStatus,
        delivered: Option<Delivered>,
    ) -> LoadId {
        let id = self.next_id();
        self.out.loads.push(Load {
            id: id.clone(),
            file: file.clone(),
            kind,
            scope: scope.clone(),
            via,
            when,
            status,
            delivered,
        });
        id
    }

    fn lock(&self, key: &str) -> bool {
        // Spec 4: managed `strictPluginOnlyCustomization` blocks user, project, add-dir and synced sources.
        let found = self.setup.first_in(
            "strictPluginOnlyCustomization",
            &[SettingsKind::Managed, SettingsKind::Remote],
        );
        match found.map(|(v, _)| v) {
            Some(Value::Bool(b)) => *b,
            Some(Value::Array(a)) => a.iter().any(|v| v.as_str() == Some(key)),
            _ => false,
        }
    }

    fn parse(&mut self, cand: Cand) -> Parsed {
        let id = ext_id(cand.kind, &cand.file);
        let stat = self.cx.fs.stat(&cand.file).ok();
        let bytes = stat.map_or(0, |s| s.len);
        let mut rejected = None;
        let text = if bytes > MAX_EXT_FILE_BYTES {
            rejected = Some(("claude.ext.rejected_too_large", format!("{bytes} bytes")));
            String::new()
        } else {
            self.cx
                .fs
                .read(&cand.file)
                .ok()
                .map(|b| String::from_utf8_lossy(&b).into_owned())
                .unwrap_or_default()
        };
        let fm = frontmatter::parse(&text);
        let body = &text[fm.body.clone()];
        let map = &fm.map;
        let in_plugin = cand.plugin.is_some();
        // Names (spec 4: how a skill, command and agent gets its name).
        let own = match cand.kind {
            ExtKind::Skill => fm_str(map, "name").map_or_else(|| cand.name.clone(), str::to_owned),
            ExtKind::Command => cand.name.clone(),
            ExtKind::Subagent => fm_str(map, "name").map(str::to_owned).unwrap_or_else(|| {
                if in_plugin {
                    cand.name.rsplit(':').next().unwrap_or("").to_owned()
                } else {
                    String::new()
                }
            }),
        };
        let name = match (&cand.plugin_prefix, cand.kind) {
            (Some(prefix), ExtKind::Subagent) => {
                // `<plugin>:<subfolder>:...:<name>`: the folders come from the path in `cand.name`.
                let mut segs: Vec<&str> = cand.name.split(':').collect();
                segs.pop();
                segs.push(&own);
                format!("{prefix}:{}", segs.join(":"))
            }
            (Some(prefix), _) => format!("{prefix}:{own}"),
            (None, _) => own.clone(),
        };
        let description = fm_str(map, "description").map(str::to_owned).or_else(|| {
            (cand.kind != ExtKind::Subagent)
                .then(|| first_line_description(body))
                .flatten()
        });
        // Spec 4, Subagents: files Claude Code skips outside plugins.
        if rejected.is_none() && cand.kind == ExtKind::Subagent && !in_plugin {
            let raw = fm_str(map, "name").unwrap_or("");
            rejected = if fm.error.is_some() {
                Some(("claude.ext.rejected_yaml", "frontmatter".to_owned()))
            } else if raw.is_empty() || raw.starts_with('-') || raw.contains(':') || raw.len() > 256
            {
                Some(("claude.ext.rejected_agent_name", "name".to_owned()))
            } else if fm_str(map, "description").is_none() {
                Some((
                    "claude.ext.rejected_agent_description",
                    "description".to_owned(),
                ))
            } else {
                None
            };
        }
        Parsed {
            id,
            text,
            bytes,
            fm,
            name,
            description,
            rejected,
            cand,
        }
    }

    fn build(&mut self, cands: Vec<Cand>) {
        let parsed: Vec<Parsed> = cands.into_iter().map(|c| self.parse(c)).collect();

        // Name precedence (spec 4, Name collisions): skills beat commands, then managed, user,
        // project (closest to cwd first). Agents: managed, project, user, plugin.
        let mut winner: BTreeMap<(bool, String), usize> = BTreeMap::new();
        let rank = |p: &Parsed| (p.cand.kind == ExtKind::Command, p.cand.prio);
        for (i, p) in parsed.iter().enumerate() {
            if p.rejected.is_some() {
                continue;
            }
            let key = (p.cand.kind == ExtKind::Subagent, p.name.clone());
            match winner.get(&key) {
                Some(&w) if rank(&parsed[w]) <= rank(p) => {}
                _ => {
                    winner.insert(key, i);
                }
            }
        }

        let max_desc = max_desc_chars(self.setup);
        let overrides = self
            .setup
            .first("skillOverrides")
            .and_then(|(v, _)| v.as_object().cloned())
            .unwrap_or_default();
        let plugins = self.out.plugins.clone();
        let skills_locked = self.lock("skills");
        let agents_locked = self.lock("agents");

        let mut skill_chars: u64 = 0;
        let mut agent_tokens: u64 = 0;
        let mut exts: Vec<Extension> = Vec::new();
        let mut diagnostics = Vec::new();
        let mut files = Vec::new();

        for i in 0..parsed.len() {
            let p = &parsed[i];
            let cand = &p.cand;
            let plugin = cand.plugin.map(|idx| &plugins[idx]);
            let policy = match (cand.source, plugin) {
                (Source::Managed, _) => EditPolicy::ReadOnly {
                    why: ReadOnlyWhy::Managed,
                },
                (Source::Synced, _) => EditPolicy::ReadOnly {
                    why: ReadOnlyWhy::Synced,
                },
                (_, Some(pl)) => pl.policy(),
                _ => EditPolicy::Editable,
            };
            if policy != EditPolicy::Editable {
                files.push(FileClaim {
                    path: cand.file.clone(),
                    policy: policy.clone(),
                });
            }
            let rule = RuleRef::new(match cand.kind {
                ExtKind::Skill => "claude.skill",
                ExtKind::Command => "claude.command",
                ExtKind::Subagent => "claude.agent",
            });
            let body_kind = match cand.kind {
                ExtKind::Skill => DocKind::SkillBody,
                ExtKind::Command => DocKind::CommandBody,
                ExtKind::Subagent => DocKind::SubagentPrompt,
            };
            let via = Via::Discovery { rule: rule.clone() };
            let body_text = body_of(&p.text);

            if let Some((why, _)) = &p.rejected {
                let status = if *why == "claude.ext.rejected_too_large" {
                    LoadStatus::Skipped {
                        skip: Skip::TooLarge {
                            limit: Limit {
                                kind: LimitKind::MaxFileBytes,
                                value: MAX_EXT_FILE_BYTES,
                                unit: Unit::Bytes,
                            },
                        },
                    }
                } else {
                    LoadStatus::Skipped {
                        skip: Skip::NotRead {
                            why: RuleRef::new(why),
                        },
                    }
                };
                let body = self.push_load(
                    &cand.file,
                    body_kind,
                    &cand.scope,
                    via,
                    Activation::OnInvoke,
                    status,
                    None,
                );
                diagnostics.push(Diagnostic {
                    severity: Severity::Warning,
                    agent: Some(AgentId::builtin("claude")),
                    at: Subject::Extension { ext: p.id.clone() },
                    problem: Problem::ExtensionRejected {
                        why: RuleRef::new(why),
                    },
                    fix: None,
                });
                exts.push(Extension {
                    id: p.id.clone(),
                    ext_kind: cand.kind,
                    name: if p.name.is_empty() {
                        cand.file
                            .file_name()
                            .unwrap_or("")
                            .trim_end_matches(".md")
                            .to_owned()
                    } else {
                        p.name.clone()
                    },
                    description: p.description.clone(),
                    file: cand.file.clone(),
                    scope: cand.scope.clone(),
                    listing: None,
                    body,
                    status: ExtStatus::Rejected {
                        why: RuleRef::new(why),
                    },
                });
                continue;
            }
            if p.fm.error.is_some() {
                diagnostics.push(Diagnostic {
                    severity: Severity::Warning,
                    agent: Some(AgentId::builtin("claude")),
                    at: Subject::File {
                        path: cand.file.clone(),
                        line: Some(1),
                    },
                    problem: Problem::FrontmatterInvalid {
                        field: "frontmatter".to_owned(),
                    },
                    fix: None,
                });
            }

            let key = (cand.kind == ExtKind::Subagent, p.name.clone());
            let win = winner.get(&key).copied();
            let map = &p.fm.map;
            let patterns = if cand.kind == ExtKind::Command {
                Vec::new()
            } else {
                fm_list(map, "paths")
            };
            let locked = match cand.source {
                Source::User | Source::Project | Source::Nested | Source::Synced => {
                    if cand.kind == ExtKind::Subagent {
                        agents_locked
                    } else {
                        skills_locked
                    }
                }
                _ => false,
            };
            let hidden =
                if fm_bool(map, "disable-model-invocation") && cand.kind != ExtKind::Subagent {
                    Some("claude.ext.hidden_model_invocation_off")
                } else if matches!(
                    overrides.get(&p.name).and_then(Value::as_str),
                    Some("user-invocable-only" | "off")
                ) && cand.kind != ExtKind::Subagent
                    && plugin.is_none()
                {
                    Some("claude.ext.hidden_override")
                } else {
                    None
                };
            let status = if let Some(pl) = plugin.filter(|pl| !pl.enabled) {
                ExtStatus::Disabled { by: pl.id.clone() }
            } else if locked {
                ExtStatus::Disabled {
                    by: "strictPluginOnlyCustomization".to_owned(),
                }
            } else if win.is_some_and(|w| w != i) {
                ExtStatus::Shadowed {
                    by: parsed[win.expect("checked")].id.clone(),
                }
            } else if let Some(dir) = &cand.nested_dir {
                let rel = dir
                    .as_str()
                    .strip_prefix(self.setup.cwd.as_str())
                    .unwrap_or("");
                ExtStatus::Conditional {
                    patterns: vec![format!("{}/**", rel.trim_start_matches('/'))],
                }
            } else if !patterns.is_empty() {
                ExtStatus::Conditional { patterns }
            } else if let Some(why) = hidden {
                ExtStatus::Hidden {
                    why: RuleRef::new(why),
                }
            } else {
                ExtStatus::Active
            };

            // The always-listed catalog line (spec 4, What reaches the model).
            let mut listing = None;
            if status == ExtStatus::Active {
                let desc = p.description.clone().unwrap_or_default();
                let (entry, load_status) = listing_entry(cand.kind, &p.name, &desc, map, max_desc);
                let toks = tokens::count(&entry);
                if cand.kind == ExtKind::Subagent {
                    agent_tokens += u64::from(toks);
                } else {
                    skill_chars += entry.chars().count() as u64;
                }
                listing = Some(self.push_load(
                    &cand.file,
                    DocKind::Listing,
                    &cand.scope,
                    Via::Listing { ext: p.id.clone() },
                    Activation::SessionStart,
                    load_status,
                    Some(Delivered {
                        bytes: entry.len() as u64,
                        tokens: toks,
                        transformed: false,
                    }),
                ));
            }
            let body = self.push_load(
                &cand.file,
                body_kind,
                &cand.scope,
                via,
                Activation::OnInvoke,
                LoadStatus::Loaded,
                Some(Delivered {
                    bytes: body_text.len() as u64,
                    tokens: tokens::count(body_text),
                    transformed: p.fm.is_present(),
                }),
            );
            exts.push(Extension {
                id: p.id.clone(),
                ext_kind: cand.kind,
                name: p.name.clone(),
                description: p.description.clone(),
                file: cand.file.clone(),
                scope: cand.scope.clone(),
                listing,
                body,
                status,
            });
            let _ = p.bytes;
        }

        // Catalog budgets (spec 4: `contextWindowTokens x 4 x skillListingBudgetFraction`).
        let budget = listing_budget_chars(self.cx, self.setup, self.window_tokens);
        if skill_chars > budget {
            diagnostics.push(Diagnostic {
                severity: Severity::Warning,
                agent: Some(AgentId::builtin("claude")),
                at: Subject::Agent,
                problem: Problem::CatalogOverBudget {
                    used: skill_chars,
                    limit: Limit {
                        kind: LimitKind::ListingBudgetChars,
                        value: budget,
                        unit: Unit::Chars,
                    },
                },
                fix: None,
            });
        }
        if agent_tokens > AGENT_CATALOG_WARN_TOKENS {
            diagnostics.push(Diagnostic {
                severity: Severity::Warning,
                agent: Some(AgentId::builtin("claude")),
                at: Subject::Agent,
                problem: Problem::CatalogOverBudget {
                    used: agent_tokens,
                    limit: Limit {
                        kind: LimitKind::ListingBudgetChars,
                        value: AGENT_CATALOG_WARN_TOKENS,
                        unit: Unit::Tokens,
                    },
                },
                fix: None,
            });
        }

        // Plugin state files are app-managed (spec 4, On-disk state).
        files.push(FileClaim {
            path: self.setup.plugins.join("installed_plugins.json"),
            policy: EditPolicy::ReadOnly {
                why: ReadOnlyWhy::AgentState,
            },
        });
        for pl in &plugins {
            files.push(FileClaim {
                path: pl.manifest_file.clone(),
                policy: pl.policy(),
            });
        }
        self.out.extensions = exts;
        self.out.diagnostics = diagnostics;
        self.out.files = files;
    }
}

#[cfg(test)]
mod tests;
