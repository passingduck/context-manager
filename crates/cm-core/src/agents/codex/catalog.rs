//! The skills catalog Codex renders into the `<skills_instructions>` developer message.
//!
//! A port of `codex-rs/ext/skills/src/render.rs`, `catalog_prompt.rs` and `host_aliases.rs`
//! at tag rust-v0.165.0, for the host-only case with the `CoreCompatible` policy (spec 5.1,
//! "In context versus loaded on use"). It is pure: entries in, lines out.

use std::path::{Component, Path, PathBuf};

const DEFAULT_CHAR_BUDGET: usize = 8_000;
const MAX_CONFIGURED_TOKEN_BUDGET: usize = 10_000;
const WINDOW_PERCENT: usize = 2;
const MAX_DESCRIPTION_CHARS: usize = 1_024;
const TRUNCATED_SUFFIX: &str = "...";

const INTRO_UNALIASED: &str = "A skill is a set of instructions provided through a `SKILL.md` source. Below is the list of skills that can be used. Each entry includes a name, description, and source locator. `file` locators are on the host filesystem, `executor package` locators are owned by their execution environment, `cloud package` locators are opaque package identifiers, and `custom resource` locators use their provider's access mechanism.";
const INTRO_HOST_ALIASES: &str = "A skill is a set of local instructions to follow that is stored in a `SKILL.md` file. Below is the list of skills that can be used. Each entry includes a name, description, and a short path that can be expanded into an absolute path using the skill roots table.";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Budget {
    Tokens(usize),
    Characters(usize),
}

impl Budget {
    /// `skill_metadata_budget`: the configured token cap, else 2% of the window, else 8,000
    /// characters.
    pub fn resolve(context_window: Option<i64>, max_context_tokens: Option<usize>) -> Budget {
        if let Some(max) = max_context_tokens.filter(|n| *n > 0) {
            return Budget::Tokens(max.min(MAX_CONFIGURED_TOKEN_BUDGET));
        }
        context_window
            .and_then(|w| usize::try_from(w).ok())
            .filter(|w| *w > 0)
            .map(|w| Budget::Tokens((w.saturating_mul(WINDOW_PERCENT) / 100).max(1)))
            .unwrap_or(Budget::Characters(DEFAULT_CHAR_BUDGET))
    }

    fn limit(self) -> usize {
        match self {
            Budget::Tokens(n) | Budget::Characters(n) => n,
        }
    }

    fn with_limit(self, limit: usize) -> Budget {
        match self {
            Budget::Tokens(_) => Budget::Tokens(limit),
            Budget::Characters(_) => Budget::Characters(limit),
        }
    }

    fn cost(self, text: &str) -> usize {
        match self {
            Budget::Tokens(_) => approx_tokens(text),
            Budget::Characters(_) => text.chars().count(),
        }
    }

    fn cost_from_counts(self, chars: usize, bytes: usize) -> usize {
        match self {
            Budget::Tokens(_) => bytes.saturating_add(3) / 4,
            Budget::Characters(_) => chars,
        }
    }

    fn line_cost(self, line: &str) -> usize {
        self.cost(&format!("{line}\n"))
    }
}

fn approx_tokens(text: &str) -> usize {
    text.len().saturating_add(3) / 4
}

/// The rank `order_entries` sorts by (spec: system, admin, repo, user).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PromptScope {
    System,
    Admin,
    Repo,
    User,
}

impl PromptScope {
    fn rank(self) -> u8 {
        match self {
            PromptScope::System => 0,
            PromptScope::Admin => 1,
            PromptScope::Repo => 2,
            PromptScope::User => 3,
        }
    }
}

/// One model-visible skill.
#[derive(Debug, Clone)]
pub(super) struct Entry {
    pub name: String,
    pub description: String,
    /// The path the line shows when it is not shortened.
    pub path: String,
    /// The canonical root of the skill root that holds it.
    pub root: String,
    pub root_order: usize,
    pub scope: PromptScope,
}

/// What `render` produced for one skill.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Placed {
    Line(String),
    Omitted,
}

pub(super) struct Rendered {
    /// The `### Skill roots` lines, empty when paths are absolute.
    pub root_lines: Vec<String>,
    /// One per input entry, in the sorted order, with `Omitted` for dropped ones.
    pub order: Vec<usize>,
    pub placed: Vec<Placed>,
    pub aliased: bool,
    pub omitted: usize,
    /// Characters or tokens the placed lines and the roots table cost.
    pub cost: usize,
}

impl Rendered {
    pub fn body(&self) -> String {
        let lines: Vec<String> = self
            .placed
            .iter()
            .filter_map(|p| match p {
                Placed::Line(l) => Some(l.clone()),
                Placed::Omitted => None,
            })
            .collect();
        body_text(self.aliased, &self.root_lines, &lines)
    }
}

/// `render_available_skills_body` without the usage instructions.
fn body_text(aliased: bool, root_lines: &[String], skill_lines: &[String]) -> String {
    let mut lines: Vec<String> = vec![
        "## Skills".to_owned(),
        if aliased {
            INTRO_HOST_ALIASES
        } else {
            INTRO_UNALIASED
        }
        .to_owned(),
    ];
    if !root_lines.is_empty() {
        lines.push("### Skill roots".to_owned());
        lines.extend(root_lines.iter().cloned());
    }
    lines.push("### Available skills".to_owned());
    lines.extend(skill_lines.iter().cloned());
    format!("\n{}\n", lines.join("\n"))
}

/// The message text: tags around the body.
pub(super) fn message(rendered: &Rendered) -> String {
    format!(
        "<skills_instructions>{}</skills_instructions>",
        rendered.body()
    )
}

// ---------------------------------------------------------------------------------------------

struct Line<'a> {
    name: &'a str,
    description: String,
    locator: String,
}

impl Line<'_> {
    fn with(&self, description: &str) -> String {
        if description.is_empty() {
            format!("- {}: (file: {})", self.name, self.locator)
        } else {
            format!("- {}: {} (file: {})", self.name, description, self.locator)
        }
    }

    fn full(&self) -> String {
        self.with(&self.description)
    }

    fn minimum(&self) -> String {
        self.with("")
    }

    fn chars(&self) -> usize {
        self.description.chars().count()
    }

    fn with_chars(&self, n: usize) -> String {
        let end = self
            .description
            .char_indices()
            .nth(n)
            .map_or(self.description.len(), |(i, _)| i);
        self.with(&self.description[..end])
    }
}

fn truncate_description(description: &str) -> String {
    if description
        .char_indices()
        .nth(MAX_DESCRIPTION_CHARS)
        .is_none()
    {
        return description.to_owned();
    }
    let prefix = MAX_DESCRIPTION_CHARS - TRUNCATED_SUFFIX.chars().count();
    let end = description
        .char_indices()
        .nth(prefix)
        .map_or(description.len(), |(i, _)| i);
    format!("{}{TRUNCATED_SUFFIX}", &description[..end])
}

#[derive(Clone, Copy)]
enum Alloc {
    Omitted,
    Chars(usize),
}

fn allocate(lines: &[Line], budget: Budget) -> Vec<Alloc> {
    let full: usize = lines.iter().map(|l| budget.line_cost(&l.full())).sum();
    if full <= budget.limit() {
        return lines.iter().map(|l| Alloc::Chars(l.chars())).collect();
    }
    let minimum: usize = lines.iter().map(|l| budget.line_cost(&l.minimum())).sum();
    if minimum <= budget.limit() {
        return allocate_chars(budget, lines, budget.limit() - minimum)
            .into_iter()
            .map(Alloc::Chars)
            .collect();
    }
    let mut used = 0usize;
    lines
        .iter()
        .map(|line| {
            let next = used.saturating_add(budget.line_cost(&line.minimum()));
            if next <= budget.limit() {
                used = next;
                Alloc::Chars(0)
            } else {
                Alloc::Omitted
            }
        })
        .collect()
}

/// Round robin, so no skill takes the remaining space.
fn allocate_chars(budget: Budget, lines: &[Line], limit: usize) -> Vec<usize> {
    struct Costs {
        count: usize,
        extra: Vec<usize>,
    }
    let costs: Vec<Costs> = lines
        .iter()
        .map(|line| {
            let minimum = line.minimum();
            let min_chars = minimum.chars().count() + 1;
            let min_bytes = minimum.len() + 1;
            let min_cost = budget.cost_from_counts(min_chars, min_bytes);
            let mut extra = vec![0usize];
            let (mut pc, mut pb) = (0usize, 0usize);
            for ch in line.description.chars() {
                pc += 1;
                pb += ch.len_utf8();
                let cost = budget
                    .cost_from_counts(min_chars + pc + 1, min_bytes + pb + 1)
                    .saturating_sub(min_cost);
                extra.push(cost);
            }
            Costs {
                count: line.chars(),
                extra,
            }
        })
        .collect();
    let mut alloc = vec![0usize; costs.len()];
    let mut current = vec![0usize; costs.len()];
    let mut remaining = limit;
    loop {
        let mut changed = false;
        for (i, c) in costs.iter().enumerate() {
            if alloc[i] >= c.count {
                continue;
            }
            let next = alloc[i] + 1;
            let delta = c.extra[next].saturating_sub(current[i]);
            if delta <= remaining {
                alloc[i] = next;
                current[i] = c.extra[next];
                remaining -= delta;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    alloc
}

struct Draft {
    placed: Vec<Placed>,
    included: usize,
    omitted: usize,
    truncated_chars: usize,
}

fn render_lines(lines: &[Line], budget: Budget) -> Draft {
    let allocations = allocate(lines, budget);
    let mut placed = Vec::new();
    let (mut omitted, mut truncated) = (0, 0);
    for (line, alloc) in lines.iter().zip(&allocations) {
        match alloc {
            Alloc::Omitted => {
                omitted += 1;
                truncated += line.chars();
                placed.push(Placed::Omitted);
            }
            Alloc::Chars(n) => {
                truncated += line.chars().saturating_sub(*n);
                placed.push(Placed::Line(line.with_chars(*n)));
            }
        }
    }
    Draft {
        included: lines.len() - omitted,
        placed,
        omitted,
        truncated_chars: truncated,
    }
}

// ---------------------------------------------------------------------------------------------
// Aliases (host_aliases.rs)
// ---------------------------------------------------------------------------------------------

fn plugin_marketplace_base(path: &Path) -> Option<PathBuf> {
    let mut candidate = path;
    while let Some(parent) = candidate.parent() {
        if parent.file_name()?.to_str()? == "cache"
            && parent.parent()?.file_name()?.to_str()? == "plugins"
        {
            return Some(candidate.to_path_buf());
        }
        candidate = parent;
    }
    None
}

fn plugin_version_base(path: &Path) -> Option<PathBuf> {
    let base = plugin_marketplace_base(path)?;
    let mut rel = path.strip_prefix(&base).ok()?.components();
    let Component::Normal(plugin) = rel.next()? else {
        return None;
    };
    let Component::Normal(version) = rel.next()? else {
        return None;
    };
    Some(base.join(plugin).join(version))
}

fn shared_roots(entries: &[&Entry]) -> Vec<String> {
    let mut counts = std::collections::HashMap::new();
    for entry in entries {
        if let Some(base) = plugin_version_base(Path::new(&entry.root)) {
            *counts.entry(base).or_insert(0usize) += 1;
        }
    }
    entries
        .iter()
        .map(|entry| {
            let root = Path::new(&entry.root);
            let shared = match plugin_version_base(root) {
                Some(base) if counts.get(&base).copied().unwrap_or(0) <= 1 => {
                    plugin_marketplace_base(root).unwrap_or_else(|| root.to_path_buf())
                }
                _ => root.to_path_buf(),
            };
            shared.to_string_lossy().replace('\\', "/")
        })
        .collect()
}

struct AliasPlan {
    /// `(name, root)` in assignment order.
    roots: Vec<(String, String)>,
}

impl AliasPlan {
    fn build(roots: &[String]) -> Option<AliasPlan> {
        let mut out: Vec<(String, String)> = Vec::new();
        for root in roots {
            if out.iter().any(|(_, r)| r == root) {
                continue;
            }
            out.push((format!("r{}", out.len()), root.clone()));
        }
        (!out.is_empty()).then_some(AliasPlan { roots: out })
    }

    fn shorten(&self, locator: &str) -> Option<String> {
        self.roots
            .iter()
            .filter_map(|(name, root)| {
                let suffix = locator
                    .strip_prefix(root.trim_end_matches('/'))?
                    .strip_prefix('/')?;
                Some((name, root, suffix))
            })
            .max_by_key(|(_, root, _)| root.len())
            .map(|(name, _, suffix)| format!("{name}/{suffix}"))
    }

    /// An owned plan lists every root, used or not (`AliasPlan::root_lines`).
    fn root_lines(&self) -> Vec<String> {
        self.roots
            .iter()
            .map(|(name, root)| format!("- `{name}` = `{root}`"))
            .collect()
    }
}

fn overhead_cost(budget: Budget, root_lines: &[String]) -> usize {
    let absolute = body_text(false, &[], &[]);
    let aliased = body_text(true, root_lines, &[]);
    budget.cost(&aliased).saturating_sub(budget.cost(&absolute))
}

fn cost_of(budget: Budget, root_lines: &[String], placed: &[Placed]) -> usize {
    let table = if root_lines.is_empty() {
        0
    } else {
        overhead_cost(budget, root_lines)
    };
    placed.iter().fold(table, |used, p| match p {
        Placed::Line(l) => used.saturating_add(budget.line_cost(l)),
        Placed::Omitted => used,
    })
}

/// Sorts entries as `CoreCompatible::order_entries` does and renders the better of the absolute
/// and the aliased form.
pub(super) fn render(entries: &[Entry], budget: Budget) -> Rendered {
    let mut order: Vec<usize> = (0..entries.len()).collect();
    order.sort_by(|a, b| {
        let (a, b) = (&entries[*a], &entries[*b]);
        a.scope
            .rank()
            .cmp(&b.scope.rank())
            .then_with(|| a.name.cmp(&b.name))
            .then_with(|| a.path.cmp(&b.path))
    });
    let sorted: Vec<&Entry> = order.iter().map(|i| &entries[*i]).collect();

    // Absolute form.
    let absolute_lines: Vec<Line> = sorted
        .iter()
        .map(|e| Line {
            name: &e.name,
            description: truncate_description(&e.description),
            locator: e.path.clone(),
        })
        .collect();
    let absolute = render_lines(&absolute_lines, budget);

    // Aliased form.
    let mut by_root_order: Vec<&Entry> = sorted.clone();
    by_root_order.sort_by_key(|e| e.root_order);
    let plan = AliasPlan::build(&shared_roots(&by_root_order));
    let mut chosen_aliased = None;
    if let Some(plan) = &plan {
        let aliased_lines: Vec<Line> = sorted
            .iter()
            .map(|e| {
                let locator = plan.shorten(&e.path).unwrap_or_else(|| e.path.clone());
                Line {
                    name: &e.name,
                    description: truncate_description(&e.description),
                    locator,
                }
            })
            .collect();
        let root_lines = plan.root_lines();
        let table_cost = overhead_cost(budget, &root_lines);
        if !root_lines.is_empty() && table_cost < budget.limit() {
            let adjusted = budget.with_limit(budget.limit() - table_cost);
            let draft = render_lines(&aliased_lines, adjusted);
            let better = if draft.included != absolute.included {
                draft.included > absolute.included
            } else if draft.truncated_chars != absolute.truncated_chars {
                draft.truncated_chars < absolute.truncated_chars
            } else {
                cost_of(budget, &root_lines, &draft.placed) < cost_of(budget, &[], &absolute.placed)
            };
            if better {
                chosen_aliased = Some((root_lines, draft));
            }
        }
    }

    let (root_lines, draft, aliased) = match chosen_aliased {
        Some((root_lines, draft)) => (root_lines, draft, true),
        None => (Vec::new(), absolute, false),
    };
    let cost = cost_of(budget, &root_lines, &draft.placed);
    Rendered {
        root_lines,
        order,
        placed: draft.placed,
        aliased,
        omitted: draft.omitted,
        cost,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, root: &str, scope: PromptScope, order: usize) -> Entry {
        Entry {
            name: name.to_owned(),
            description: format!("{name} description"),
            path: format!("{root}/{name}/SKILL.md"),
            root: root.to_owned(),
            root_order: order,
            scope,
        }
    }

    #[test]
    fn a_small_catalog_uses_short_paths_and_sorts_system_before_repo_before_user() {
        let entries = [
            entry("b", "/h/.codex/skills", PromptScope::User, 0),
            entry(
                "imagegen",
                "/h/.codex/skills/.system",
                PromptScope::System,
                1,
            ),
            entry("a", "/p/.agents/skills", PromptScope::Repo, 2),
        ];
        let rendered = render(&entries, Budget::Characters(8_000));
        let names: Vec<_> = rendered
            .placed
            .iter()
            .map(|p| match p {
                Placed::Line(l) => l.split(':').next().unwrap().to_owned(),
                Placed::Omitted => "omitted".to_owned(),
            })
            .collect();
        assert_eq!(names, ["- imagegen", "- a", "- b"]);
        assert!(rendered.aliased);
        assert!(rendered.body().contains("(file: r1/imagegen/SKILL.md)"));
        assert!(rendered.body().contains("- `r0` = `/h/.codex/skills`"));
    }

    #[test]
    fn a_tight_budget_shrinks_descriptions_then_drops_skills() {
        let entries: Vec<_> = (0..10)
            .map(|i| entry(&format!("skill{i}"), "/r", PromptScope::User, 0))
            .collect();
        let rendered = render(&entries, Budget::Characters(150));
        assert!(rendered.omitted > 0 && rendered.omitted < 10);
        assert!(rendered.cost <= 150);
    }
}
