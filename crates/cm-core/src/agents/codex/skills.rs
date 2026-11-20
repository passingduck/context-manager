//! Skills (spec 5.1): the roots, the loader rules, the catalog line and its budget.

use std::collections::HashSet;

use super::catalog::{self, Budget, Entry, Placed, PromptScope};
use super::out::{Block, BlockPart, NewLoad, Out, delivered, discovery};
use super::plugins::Plugin;
use super::setup::{Layer, LayerKind, Setup, get_bool, get_u64, read_lossy};
use crate::agents::ScanCx;
use crate::limits::cut_bytes_lossy;
use crate::model::*;
use crate::probe_fs::EntryKind;
use crate::walk;

pub(super) const RULE_ROOT_PROJECT: &str = "codex.skills.root.project";
pub(super) const RULE_ROOT_USER: &str = "codex.skills.root.user";
pub(super) const RULE_ROOT_AGENTS: &str = "codex.skills.root.agents";
pub(super) const RULE_ROOT_SYSTEM: &str = "codex.skills.root.system";
pub(super) const RULE_ROOT_ADMIN: &str = "codex.skills.root.admin";
pub(super) const RULE_ROOT_PLUGIN: &str = "codex.skills.root.plugin";
pub(super) const RULE_DISABLED: &str = "codex.skills.disabled";
pub(super) const RULE_EXPLICIT_ONLY: &str = "codex.skills.explicit_only";
pub(super) const RULE_REJECTED: &str = "codex.skills.rejected";

/// Spec 5.1: `MAX_SCAN_DEPTH`, `MAX_SKILLS_DIRS_PER_ROOT`. Codex also stops at
/// `MAX_SKILLS_ENTRIES_PER_ROOT` (20,000), which the scan's own entry budget is below.
const MAX_DEPTH: u32 = 6;
const MAX_DIRS: usize = 2000;
const _: () = assert!(walk::MAX_TREE_ENTRIES <= 20_000);
const MAX_NAME_CHARS: usize = 64;
/// `MAX_SKILL_PROMPT_BYTES`: the body Codex injects when the user names a skill.
const MAX_BODY_BYTES: u64 = 8_000;
/// The window most models in a Codex model cache have.
const DEFAULT_WINDOW: i64 = 272_000;
const MANIFEST_DIRS: [&str; 3] = [".codex-plugin", ".claude-plugin", ".cursor-plugin"];

#[derive(Debug, Clone, Copy, PartialEq)]
enum RootScope {
    Repo,
    User,
    System,
    Admin,
}

struct Root {
    path: AbsPath,
    scope: RootScope,
    rule: &'static str,
    plugin: Option<(String, String)>,
    /// The `Scope` shown for skills of this root.
    shown: Scope,
}

/// Spec 5.1, roots in the order `resolve_skill_roots` builds them, de-duplicated by path.
fn roots(cx: &ScanCx, setup: &Setup, plugins: &[Plugin]) -> Vec<Root> {
    let mut roots: Vec<Root> = Vec::new();
    let mut push = |root: Root| {
        if !roots.iter().any(|r| r.path == root.path) {
            roots.push(root);
        }
    };
    // Layers high to low. Project layers count even when disabled (spec 5.1, probe J).
    for layer in setup.layers.iter().rev() {
        match &layer.kind {
            LayerKind::Project { dir } => push(Root {
                path: layer.folder.join("skills"),
                scope: RootScope::Repo,
                rule: RULE_ROOT_PROJECT,
                plugin: None,
                shown: Scope::Project { dir: dir.clone() },
            }),
            LayerKind::User => {
                push(Root {
                    path: layer.folder.join("skills"),
                    scope: RootScope::User,
                    rule: RULE_ROOT_USER,
                    plugin: None,
                    shown: Scope::User,
                });
                push(Root {
                    path: setup.home.join(".agents/skills"),
                    scope: RootScope::User,
                    rule: RULE_ROOT_AGENTS,
                    plugin: None,
                    shown: Scope::User,
                });
                if get_bool(&setup.merged, &["skills", "bundled", "enabled"]).unwrap_or(true) {
                    push(Root {
                        path: layer.folder.join("skills/.system"),
                        scope: RootScope::System,
                        rule: RULE_ROOT_SYSTEM,
                        plugin: None,
                        shown: Scope::Builtin,
                    });
                }
            }
            LayerKind::System => push(Root {
                path: layer.folder.join("skills"),
                scope: RootScope::Admin,
                rule: RULE_ROOT_ADMIN,
                plugin: None,
                shown: Scope::Managed,
            }),
            LayerKind::LegacyManaged => {}
        }
    }
    for plugin in plugins.iter().filter(|p| p.enabled) {
        for path in plugin.skill_roots() {
            push(Root {
                path,
                scope: RootScope::User,
                rule: RULE_ROOT_PLUGIN,
                plugin: Some((plugin.id.clone(), plugin.name.clone())),
                shown: Scope::Plugin {
                    id: plugin.id.clone(),
                },
            });
        }
    }
    // `<dir>/.agents/skills` from the project root down to cwd, root first.
    let mut dirs = Vec::new();
    let mut cursor = Some(setup.cwd.clone());
    while let Some(dir) = cursor {
        let done = !setup.md_root_found || dir == setup.md_root;
        cursor = if done { None } else { dir.parent() };
        dirs.push(dir);
    }
    dirs.reverse();
    for dir in dirs {
        let path = dir.join(".agents/skills");
        let is_dir = cx
            .fs
            .stat(&path)
            .map(|s| s.kind == EntryKind::Dir)
            .unwrap_or(false);
        if is_dir {
            push(Root {
                path,
                scope: RootScope::Repo,
                rule: RULE_ROOT_PROJECT,
                plugin: None,
                shown: Scope::Project { dir },
            });
        }
    }
    roots
}

/// Spec 5.1: recursive `SKILL.md` search, depth 6, at most 2,000 directories, hidden directories
/// skipped. Also returns the directories that hold a plugin manifest directory.
fn discover(cx: &ScanCx, root: &Root) -> (Vec<AbsPath>, Vec<AbsPath>) {
    let mut dirs_listed = 1usize;
    let entries = walk::list_tree(cx.fs, &root.path, |e| {
        if e.name().starts_with('.') || e.depth() >= MAX_DEPTH || dirs_listed >= MAX_DIRS {
            return false;
        }
        // Spec 5.1: system roots ignore directory symlinks.
        if root.scope == RootScope::System && cx.fs.stat(&e.path).is_ok_and(|s| s.is_symlink) {
            return false;
        }
        dirs_listed += 1;
        true
    });
    let mut files = Vec::new();
    let mut plugin_roots = Vec::new();
    for entry in entries {
        match entry.kind {
            EntryKind::File if entry.name() == "SKILL.md" => files.push(entry.path),
            EntryKind::Dir if MANIFEST_DIRS.contains(&entry.name()) => {
                plugin_roots.extend(entry.path.parent());
            }
            _ => {}
        }
    }
    (files, plugin_roots)
}

// ---------------------------------------------------------------------------------------------
// Frontmatter (codex-rs/skills/src/parser.rs)
// ---------------------------------------------------------------------------------------------

struct Parsed {
    name: Option<String>,
    description: String,
}

enum Reject {
    Frontmatter,
    Field(&'static str),
}

fn sanitize(raw: &str) -> String {
    raw.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn extract_frontmatter(contents: &str) -> Option<String> {
    let mut lines = contents.lines();
    if lines.next()?.trim() != "---" {
        return None;
    }
    let mut block = Vec::new();
    let mut closed = false;
    for line in lines {
        if line.trim() == "---" {
            closed = true;
            break;
        }
        block.push(line);
    }
    (!block.is_empty() && closed).then(|| block.join("\n"))
}

/// Quotes plain scalars that are not valid YAML, as Codex does for prose such as
/// `description: Build for AWS: ECS`.
fn repair(frontmatter: &str) -> Option<String> {
    let mut changed = false;
    let mut block_indent: Option<usize> = None;
    let mut out = Vec::new();
    for line in frontmatter.lines() {
        let indent = line.chars().take_while(|c| *c == ' ').count();
        if let Some(block) = block_indent {
            if line.trim().is_empty() || indent > block {
                out.push(line.to_owned());
                continue;
            }
            block_indent = None;
        }
        let Some((key, value)) = line.split_once(':') else {
            out.push(line.to_owned());
            continue;
        };
        if key.trim().is_empty() || !value.chars().next().is_none_or(char::is_whitespace) {
            out.push(line.to_owned());
            continue;
        }
        let trimmed = value.trim_start();
        let leading = &value[..value.len() - trimmed.len()];
        let (mut scalar, mut comment) = (trimmed, "");
        for (index, ch) in trimmed.char_indices() {
            if ch == '#'
                && (index == 0
                    || trimmed[..index]
                        .chars()
                        .next_back()
                        .is_some_and(char::is_whitespace))
            {
                let at = trimmed[..index].trim_end().len();
                scalar = &trimmed[..at];
                comment = &trimmed[at..];
                break;
            }
        }
        let scalar = scalar.trim_end();
        let Some(first) = scalar.chars().next() else {
            out.push(line.to_owned());
            continue;
        };
        if matches!(first, '|' | '>') {
            block_indent = Some(indent);
            out.push(line.to_owned());
            continue;
        }
        if matches!(first, '\'' | '"') {
            out.push(line.to_owned());
            continue;
        }
        let mut colon = false;
        let mut chars = scalar.chars().peekable();
        while let Some(ch) = chars.next() {
            if ch == ':' && chars.peek().is_some_and(|n| n.is_whitespace()) {
                colon = true;
                break;
            }
        }
        let bad_flow = matches!(first, '[' | '{' | '@' | '`')
            && serde_yaml_ng::from_str::<serde_yaml_ng::Value>(scalar).is_err();
        if !colon && !bad_flow {
            out.push(line.to_owned());
            continue;
        }
        out.push(format!(
            "{}:{leading}'{}'{comment}",
            key,
            scalar.replace('\'', "''")
        ));
        changed = true;
    }
    changed.then(|| out.join("\n"))
}

fn parse_skill(contents: &str) -> Result<Parsed, Reject> {
    let block = extract_frontmatter(contents).ok_or(Reject::Frontmatter)?;
    let value: serde_json::Value = match serde_yaml_ng::from_str(&block) {
        Ok(v) => v,
        Err(_) => match repair(&block) {
            Some(fixed) => serde_yaml_ng::from_str(&fixed).map_err(|_| Reject::Frontmatter)?,
            None => return Err(Reject::Frontmatter),
        },
    };
    let text = |key: &str| value.get(key).and_then(|v| v.as_str()).map(sanitize);
    let name = text("name").filter(|n| !n.is_empty());
    let description = text("description").unwrap_or_default();
    if let Some(name) = &name
        && name.chars().count() > MAX_NAME_CHARS
    {
        return Err(Reject::Field("name"));
    }
    if description.is_empty() {
        return Err(Reject::Field("description"));
    }
    Ok(Parsed { name, description })
}

// ---------------------------------------------------------------------------------------------

struct Skill {
    name: String,
    description: String,
    file: AbsPath,
    canonical: AbsPath,
    root: usize,
    scope: RootScope,
    shown: Scope,
    rule: &'static str,
    implicit: bool,
    bytes: u64,
    text: String,
}

struct Rejected {
    file: AbsPath,
    shown: Scope,
    rule: &'static str,
    reject: Reject,
}

/// Spec 7.2: the bundled system skills are rewritten at startup, administrator skills are
/// managed, and plugin skills belong to the plugin cache.
fn skill_policy(scope: RootScope, shown: &Scope) -> EditPolicy {
    match (scope, shown) {
        (_, Scope::Plugin { id }) => EditPolicy::ReadOnly {
            why: ReadOnlyWhy::PluginCache { plugin: id.clone() },
        },
        (RootScope::System, _) => EditPolicy::ReadOnly {
            why: ReadOnlyWhy::Generated,
        },
        (RootScope::Admin, _) => EditPolicy::ReadOnly {
            why: ReadOnlyWhy::Managed,
        },
        _ => EditPolicy::Editable,
    }
}

fn ext_id(kind: &str, file: &AbsPath) -> ExtId {
    ExtId(sha256_hex(format!("codex\0{kind}\0{file}").as_bytes()))
}

fn scope_rank(scope: RootScope) -> u8 {
    match scope {
        RootScope::Repo => 0,
        RootScope::User => 1,
        RootScope::System => 2,
        RootScope::Admin => 3,
    }
}

/// `agents/openai.yaml`: `policy.allow_implicit_invocation: false` hides a skill from the
/// catalog (it can still be named explicitly).
fn allows_implicit(cx: &ScanCx, skill_dir: &AbsPath) -> bool {
    let Ok(bytes) = cx.fs.read(&skill_dir.join("agents/openai.yaml")) else {
        return true;
    };
    serde_yaml_ng::from_slice::<serde_json::Value>(&bytes)
        .ok()
        .and_then(|v| v.get("policy")?.get("allow_implicit_invocation")?.as_bool())
        .unwrap_or(true)
}

/// `[[skills.config]]`, user layer only (spec 5.1). The last matching entry wins.
fn disabled_paths(setup: &Setup, skills: &[Skill]) -> HashSet<AbsPath> {
    let user: &Layer = setup.user_layer();
    let mut disabled = HashSet::new();
    let Some(entries) = user
        .config
        .as_ref()
        .and_then(|c| super::setup::get_path(c, &["skills", "config"]))
        .and_then(toml::Value::as_array)
    else {
        return disabled;
    };
    for entry in entries {
        let Some(enabled) = entry.get("enabled").and_then(toml::Value::as_bool) else {
            continue;
        };
        if let Some(path) = entry.get("path").and_then(toml::Value::as_str) {
            let path = setup.path_in_layer(user, path);
            for skill in skills
                .iter()
                .filter(|s| s.canonical == path || s.file == path)
            {
                apply(&mut disabled, &skill.canonical, enabled);
            }
            if !skills.iter().any(|s| s.canonical == path || s.file == path) {
                apply(&mut disabled, &path, enabled);
            }
        } else if let Some(name) = entry.get("name").and_then(toml::Value::as_str) {
            for skill in skills.iter().filter(|s| s.name == name) {
                apply(&mut disabled, &skill.canonical, enabled);
            }
        }
    }
    disabled
}

fn apply(disabled: &mut HashSet<AbsPath>, path: &AbsPath, enabled: bool) {
    if enabled {
        disabled.remove(path);
    } else {
        disabled.insert(path.clone());
    }
}

/// Which skill-name prefix a plugin manifest above a skill gives it (spec 5.1).
fn nested_namespace(
    cx: &ScanCx,
    root: &Root,
    file: &AbsPath,
    plugin_roots: &[AbsPath],
) -> Option<String> {
    if let Some((_, name)) = &root.plugin {
        return Some(name.clone());
    }
    let mut cursor = file.parent();
    while let Some(dir) = cursor {
        if plugin_roots.contains(&dir) || dir == root.path {
            for manifest in [
                ".codex-plugin/plugin.json",
                ".claude-plugin/plugin.json",
                ".cursor-plugin/plugin.json",
            ] {
                let Ok(bytes) = cx.fs.read(&dir.join(manifest)) else {
                    continue;
                };
                let name = serde_json::from_slice::<serde_json::Value>(&bytes)
                    .ok()
                    .and_then(|v| v.get("name")?.as_str().map(str::to_owned));
                return name.map(|n| {
                    if n.trim().is_empty() {
                        dir.file_name().unwrap_or("").to_owned()
                    } else {
                        n
                    }
                });
            }
        }
        if dir == root.path {
            break;
        }
        cursor = dir.parent();
    }
    None
}

/// The window the catalog budget follows: `model_context_window`, else the window the model
/// cache of the Codex home records for `model`, else the 272,000 that most models in that cache
/// have. A scan cannot read the live model catalog (spec 5.1), so the last step is an assumption.
pub(super) fn context_window(cx: &ScanCx, setup: &Setup) -> Option<i64> {
    if let Some(window) = setup
        .merged
        .get("model_context_window")
        .and_then(toml::Value::as_integer)
    {
        return Some(window);
    }
    let model = super::setup::get_str(&setup.merged, &["model"]);
    let cache = cx.fs.read(&setup.codex_home.join("models_cache.json")).ok();
    if let (Some(model), Some(cache)) = (model, cache)
        && let Ok(json) = serde_json::from_slice::<serde_json::Value>(&cache)
        && let Some(found) = json["models"]
            .as_array()
            .and_then(|ms| ms.iter().find(|m| m["slug"] == model))
        && let Some(window) = found["context_window"]
            .as_i64()
            .or_else(|| found["max_context_window"].as_i64())
    {
        return Some(window);
    }
    Some(DEFAULT_WINDOW)
}

pub(super) fn run(cx: &ScanCx, setup: &Setup, plugins: &[Plugin], out: &mut Out) {
    let roots = roots(cx, setup, plugins);
    let mut skills: Vec<Skill> = Vec::new();
    let mut rejected: Vec<Rejected> = Vec::new();
    let mut seen: HashSet<AbsPath> = HashSet::new();
    let mut root_canonical: Vec<AbsPath> = Vec::new();

    for (index, root) in roots.iter().enumerate() {
        let canon_root = cx
            .fs
            .real_path(&root.path)
            .unwrap_or_else(|_| root.path.clone());
        root_canonical.push(canon_root);
        let (files, plugin_roots) = discover(cx, root);
        for file in files {
            let canonical = cx.fs.real_path(&file).unwrap_or_else(|_| file.clone());
            if !seen.insert(canonical.clone()) {
                continue;
            }
            let Some((bytes, text)) = read_lossy(cx, &file) else {
                continue;
            };
            let dir = file.parent().expect("SKILL.md has a directory");
            match parse_skill(&text) {
                Ok(parsed) => {
                    let base = parsed.name.unwrap_or_else(|| {
                        dir.file_name()
                            .map(sanitize)
                            .filter(|n| !n.is_empty())
                            .unwrap_or_else(|| "skill".to_owned())
                    });
                    let name = match nested_namespace(cx, root, &file, &plugin_roots) {
                        Some(ns) => format!("{ns}:{base}"),
                        None => base,
                    };
                    skills.push(Skill {
                        name,
                        description: parsed.description,
                        implicit: allows_implicit(cx, &dir),
                        file,
                        canonical,
                        root: index,
                        scope: root.scope,
                        shown: root.shown.clone(),
                        rule: root.rule,
                        bytes: bytes.len() as u64,
                        text,
                    });
                }
                Err(reject) => {
                    // System skills reject silently (`SkillScope::System` drops the error).
                    if root.scope != RootScope::System {
                        rejected.push(Rejected {
                            file,
                            shown: root.shown.clone(),
                            rule: root.rule,
                            reject,
                        });
                    }
                }
            }
        }
    }

    // `merge_host_skill_root_snapshots`: repo, user, system, admin, then name, then path.
    skills.sort_by(|a, b| {
        scope_rank(a.scope)
            .cmp(&scope_rank(b.scope))
            .then_with(|| a.name.cmp(&b.name))
            .then_with(|| a.canonical.cmp(&b.canonical))
    });
    let disabled = disabled_paths(setup, &skills);

    // The model-visible entries for the catalog.
    let include_block =
        get_bool(&setup.merged, &["skills", "include_instructions"]).unwrap_or(true);
    let visible: Vec<usize> = skills
        .iter()
        .enumerate()
        .filter(|(_, s)| s.implicit && !disabled.contains(&s.canonical))
        .map(|(i, _)| i)
        .collect();
    let entries: Vec<Entry> = visible
        .iter()
        .map(|i| {
            let s = &skills[*i];
            Entry {
                name: s.name.clone(),
                description: s.description.clone(),
                path: s.file.to_string(),
                root: root_canonical[s.root].to_string(),
                root_order: s.root,
                scope: match s.scope {
                    RootScope::System => PromptScope::System,
                    RootScope::Admin => PromptScope::Admin,
                    RootScope::Repo => PromptScope::Repo,
                    RootScope::User => PromptScope::User,
                },
            }
        })
        .collect();
    let context_window = context_window(cx, setup);
    let max_tokens = get_u64(&setup.merged, &["skills", "max_context_tokens"]).map(|n| n as usize);
    let budget = Budget::resolve(context_window, max_tokens);
    let rendered =
        (include_block && !entries.is_empty()).then(|| catalog::render(&entries, budget));
    // Placement of each visible skill: its line, or omitted.
    let mut line_of: std::collections::HashMap<usize, Placed> = std::collections::HashMap::new();
    if let Some(r) = &rendered {
        for (slot, entry_index) in r.order.iter().enumerate() {
            line_of.insert(visible[*entry_index], r.placed[slot].clone());
        }
    }

    // Loads and extensions, in the catalog's order, then the rest.
    let mut ordered: Vec<usize> = Vec::new();
    if let Some(r) = &rendered {
        ordered.extend(r.order.iter().map(|i| visible[*i]));
    }
    let placed_first: HashSet<usize> = ordered.iter().copied().collect();
    ordered.extend((0..skills.len()).filter(|i| !placed_first.contains(i)));

    let mut block_parts: Vec<(usize, LoadId, String)> = Vec::new();
    for index in ordered {
        let skill = &skills[index];
        let id = ext_id("skill", &skill.file);
        let is_disabled = disabled.contains(&skill.canonical);
        let (listing_status, listing_delivered, line, status) = if !include_block {
            (
                LoadStatus::Skipped {
                    skip: Skip::FeatureOff {
                        name: "skills.include_instructions".to_owned(),
                    },
                },
                None,
                None,
                ExtStatus::Active,
            )
        } else if is_disabled {
            (
                LoadStatus::Skipped {
                    skip: Skip::NotRead {
                        why: RuleRef::new(RULE_DISABLED),
                    },
                },
                None,
                None,
                ExtStatus::Disabled {
                    by: setup.user_config.to_string(),
                },
            )
        } else if !skill.implicit {
            (
                LoadStatus::Skipped {
                    skip: Skip::NotRead {
                        why: RuleRef::new(RULE_EXPLICIT_ONLY),
                    },
                },
                None,
                None,
                ExtStatus::Hidden {
                    why: RuleRef::new(RULE_EXPLICIT_ONLY),
                },
            )
        } else {
            match line_of.get(&index) {
                Some(Placed::Line(line)) => (
                    LoadStatus::Loaded,
                    Some(delivered(line, line.len() as u64 + 1)),
                    Some(line.clone()),
                    ExtStatus::Active,
                ),
                _ => {
                    let limit = Limit {
                        kind: LimitKind::ListingBudgetChars,
                        value: budget_value(budget),
                        unit: budget_unit(budget),
                    };
                    out.diagnose(
                        Severity::Warning,
                        Subject::Extension { ext: id.clone() },
                        Problem::DroppedByBudget {
                            limit: limit.clone(),
                        },
                    );
                    (
                        LoadStatus::Skipped {
                            skip: Skip::BudgetExhausted { limit },
                        },
                        None,
                        None,
                        ExtStatus::Active,
                    )
                }
            }
        };
        let listing = out.push_load(NewLoad {
            file: skill.file.clone(),
            kind: DocKind::Listing,
            scope: skill.shown.clone(),
            via: Via::Listing { ext: id.clone() },
            when: Activation::SessionStart,
            status: listing_status,
            delivered: listing_delivered,
        });
        if let Some(line) = line {
            block_parts.push((index, listing.clone(), line));
        }
        // The body, as Codex injects it when the skill is named.
        let cut = cut_bytes_lossy(skill.text.as_bytes(), MAX_BODY_BYTES);
        let (body_status, body_delivered) = if cut.is_whole() {
            (LoadStatus::Loaded, delivered(&cut.text, skill.bytes))
        } else {
            (
                LoadStatus::Truncated {
                    limit: Limit {
                        kind: LimitKind::MaxFileBytes,
                        value: MAX_BODY_BYTES,
                        unit: Unit::Bytes,
                    },
                    kept: cut.kept,
                    total: cut.total,
                },
                delivered(&cut.text, cut.kept),
            )
        };
        let body = out.push_load(NewLoad {
            file: skill.file.clone(),
            kind: DocKind::SkillBody,
            scope: skill.shown.clone(),
            via: discovery(skill.rule),
            when: Activation::OnInvoke,
            status: body_status,
            delivered: Some(body_delivered),
        });
        out.claim(&skill.file, skill_policy(skill.scope, &skill.shown));
        let policy_file = skill.file.parent().map(|d| d.join("agents/openai.yaml"));
        if let Some(p) = policy_file {
            out.claim(&p, skill_policy(skill.scope, &skill.shown));
        }
        out.extensions.push(Extension {
            id,
            ext_kind: ExtKind::Skill,
            name: skill.name.clone(),
            description: Some(skill.description.clone()),
            file: skill.file.clone(),
            scope: skill.shown.clone(),
            listing: Some(listing),
            body,
            status,
        });
    }

    for rej in rejected {
        let id = ext_id("skill", &rej.file);
        let rej_name = rej
            .file
            .parent()
            .and_then(|d| d.file_name().map(str::to_owned))
            .unwrap_or_else(|| "skill".to_owned());
        let (field, why) = match &rej.reject {
            Reject::Frontmatter => ("frontmatter".to_owned(), RuleRef::new(RULE_REJECTED)),
            Reject::Field(f) => ((*f).to_owned(), RuleRef::new(RULE_REJECTED)),
        };
        out.diagnose(
            Severity::Warning,
            Subject::Extension { ext: id.clone() },
            Problem::ExtensionRejected { why: why.clone() },
        );
        out.diagnose(
            Severity::Warning,
            Subject::File {
                path: rej.file.clone(),
                line: None,
            },
            Problem::FrontmatterInvalid { field },
        );
        let body = out.push_load(NewLoad {
            file: rej.file.clone(),
            kind: DocKind::SkillBody,
            scope: rej.shown.clone(),
            via: discovery(rej.rule),
            when: Activation::OnInvoke,
            status: LoadStatus::Skipped {
                skip: Skip::NotRead {
                    why: RuleRef::new(RULE_REJECTED),
                },
            },
            delivered: None,
        });
        out.claim(&rej.file, EditPolicy::Editable);
        out.extensions.push(Extension {
            id,
            ext_kind: ExtKind::Skill,
            name: rej_name,
            description: None,
            file: rej.file,
            scope: rej.shown,
            listing: None,
            body,
            status: ExtStatus::Rejected { why },
        });
    }

    if let Some(r) = &rendered {
        out.blocks.push(catalog_block(r, &block_parts));
        if r.omitted > 0 {
            let used = r.cost as u64;
            out.diagnose(
                Severity::Warning,
                Subject::Agent,
                Problem::CatalogOverBudget {
                    used,
                    limit: Limit {
                        kind: LimitKind::ListingBudgetChars,
                        value: budget_value(budget),
                        unit: budget_unit(budget),
                    },
                },
            );
        }
        out.assumptions.push(Assumption::Mode {
            name: "skills.catalog_budget".to_owned(),
            value: match budget {
                Budget::Tokens(n) => format!("{n} tokens"),
                Budget::Characters(n) => format!("{n} characters"),
            },
            set_in: None,
        });
    }
}

fn budget_value(budget: Budget) -> u64 {
    match budget {
        Budget::Tokens(n) | Budget::Characters(n) => n as u64,
    }
}

fn budget_unit(budget: Budget) -> Unit {
    match budget {
        Budget::Tokens(_) => Unit::Tokens,
        Budget::Characters(_) => Unit::Chars,
    }
}

/// The developer message of spec 5.1. The skill lines are loads, the rest is glue.
fn catalog_block(rendered: &catalog::Rendered, lines: &[(usize, LoadId, String)]) -> Block {
    let full = catalog::message(rendered);
    let marker = "### Available skills\n";
    let head_end = full.find(marker).map(|i| i + marker.len()).unwrap_or(0);
    let mut parts = vec![BlockPart::Text(full[..head_end].to_owned())];
    for (_, id, line) in lines {
        parts.push(BlockPart::Load(id.clone(), format!("{line}\n")));
    }
    parts.push(BlockPart::Text("</skills_instructions>".to_owned()));
    Block {
        delivery: Delivery::DeveloperMessage,
        label: BlockLabel::SkillCatalog,
        parts,
    }
}
