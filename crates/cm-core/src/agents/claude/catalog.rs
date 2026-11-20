//! The subagent listing and the skill listing of the session (spec 4, "What reaches the model").
//! Claude Code 2.1.329 sends them in one system-role message after the first user message, the
//! subagent listing first. Both blocks are `Approximate`: the built-in subagents and the bundled
//! skills sit in the same listings and a scan cannot see their text. What is exact: each entry
//! of a disk-defined item, its cut at `skillListingMaxDescChars`, the order of the entries, and
//! the character budget of the whole skill listing.

use std::cmp::Ordering;

use serde_json::Value;

use super::extensions::{listing_budget_chars, listing_entry, max_desc_chars};
use super::instructions::ASSUMED_WINDOW_TOKENS;
use super::render::{Built, built};
use super::setup::Setup;
use crate::agents::ScanCx;
use crate::md::frontmatter;
use crate::model::*;

const SKILL_INTRO: &str = "The following skills are available for use with the Skill tool:";
const AGENT_INTRO: &str = "Available agent types for the Agent tool:";
/// The sentence that follows the subagent entries.
const AGENT_OUTRO: &str = "When you launch multiple agents for independent work, send them in a single message with multiple tool uses so they run concurrently.";

struct Entry {
    load: LoadId,
    name: String,
    scope: Scope,
    kind: ExtKind,
    /// `name: description`, as the cut leaves it.
    text: String,
}

/// The order of a disk-defined entry in the skill listing: user skills, then project skills,
/// each sorted by name without regard to case, then every command sorted by name. The capture
/// shows nothing about the other scopes, so they keep their scan order after the project ones.
fn skill_order(a: &Entry, b: &Entry) -> Ordering {
    let rank = |e: &Entry| match (e.kind, &e.scope) {
        (ExtKind::Command, _) => 3,
        (_, Scope::User) => 0,
        (_, Scope::Project { .. } | Scope::Local) => 1,
        _ => 2,
    };
    rank(a).cmp(&rank(b)).then_with(|| {
        if rank(a) == 2 {
            Ordering::Equal
        } else {
            by_name(a, b)
        }
    })
}

fn by_name(a: &Entry, b: &Entry) -> Ordering {
    a.name.to_lowercase().cmp(&b.name.to_lowercase())
}

/// Skill and command entries share one listing and one budget. Subagents have their own.
pub(super) fn build(cx: &ScanCx, setup: &Setup, extensions: &[Extension]) -> Vec<Built> {
    let max_desc = max_desc_chars(setup);
    let mut skills: Vec<Entry> = Vec::new();
    let mut agents: Vec<Entry> = Vec::new();
    for ext in extensions {
        let Some(load) = &ext.listing else { continue };
        let fm = frontmatter_of(cx, &ext.file);
        let (text, _) = listing_entry(
            ext.ext_kind,
            &ext.name,
            ext.description.as_deref().unwrap_or(""),
            &fm,
            max_desc,
        );
        let entry = Entry {
            load: load.clone(),
            name: ext.name.clone(),
            scope: ext.scope.clone(),
            kind: ext.ext_kind,
            text,
        };
        match ext.ext_kind {
            ExtKind::Subagent => agents.push(entry),
            _ => skills.push(entry),
        }
    }
    agents.sort_by(by_name);
    skills.sort_by(skill_order);
    fit_to_budget(
        &mut skills,
        listing_budget_chars(cx, setup, ASSUMED_WINDOW_TOKENS),
        setup,
    );
    let mut out: Vec<Built> = Vec::new();
    for (label, intro, outro, entries) in [
        (
            BlockLabel::AgentCatalog,
            AGENT_INTRO,
            Some(AGENT_OUTRO),
            agents,
        ),
        (BlockLabel::SkillCatalog, SKILL_INTRO, None, skills),
    ] {
        if entries.is_empty() {
            continue;
        }
        let pieces: Vec<(LoadId, String)> = entries
            .into_iter()
            .map(|e| (e.load, format!("- {}", e.text)))
            .collect();
        let body: Vec<&str> = pieces.iter().map(|(_, text)| text.as_str()).collect();
        // The agent intro is followed directly by its entries, the skill intro by a blank line.
        let text = match outro {
            Some(outro) => format!("{intro}\n{}\n\n{outro}", body.join("\n")),
            None => format!("{intro}\n\n{}", body.join("\n")),
        };
        let mut block = built(
            label,
            text,
            pieces
                .iter()
                .map(|(id, t)| (id.clone(), t.as_str()))
                .collect(),
            Fidelity::Approximate,
        );
        block.rendered.delivery = Delivery::SystemMessage;
        block.block.delivery = Delivery::SystemMessage;
        out.push(block);
    }
    // Spec 4: the first prompt and the context around it sit between the instruction reminder
    // and this message.
    if let Some(first) = out.first_mut() {
        first.block.preceded_by = vec![Unsized::FirstPrompt];
    }
    out
}

fn frontmatter_of(cx: &ScanCx, file: &AbsPath) -> Option<serde_json::Map<String, Value>> {
    let bytes = cx.fs.read(file).ok()?;
    frontmatter::parse(&String::from_utf8_lossy(&bytes)).map
}

/// Spec 4: when the entries pass the budget, every name stays and descriptions go from the
/// least-used skills first. `skillUsage` of `~/.claude.json` holds a count and a last-used time
/// per skill. The real ranking halves a count every 7 days from now. A scan has no clock, so
/// the age is measured from the newest use on record.
fn fit_to_budget(entries: &mut [Entry], budget: u64, setup: &Setup) {
    let chars = |e: &Entry| e.text.chars().count() as u64;
    let mut total: u64 = entries.iter().map(chars).sum();
    if total <= budget {
        return;
    }
    let usage = setup.global.as_ref().and_then(|g| g.get("skillUsage"));
    let stat = |name: &str| -> (f64, f64) {
        let rec = usage.and_then(|u| u.get(name));
        let count = rec
            .and_then(|r| r.get("usageCount"))
            .and_then(Value::as_f64)
            .unwrap_or(0.0);
        let last = rec
            .and_then(|r| r.get("lastUsedAt"))
            .and_then(Value::as_f64)
            .unwrap_or(0.0);
        (count, last)
    };
    let newest = entries
        .iter()
        .map(|e| stat(&e.name).1)
        .fold(0.0_f64, f64::max);
    const DAY_MS: f64 = 86_400_000.0;
    let score = |e: &Entry| {
        let (count, last) = stat(&e.name);
        if count == 0.0 {
            return 0.0;
        }
        count * 0.5_f64.powf((newest - last).max(0.0) / DAY_MS / 7.0)
    };
    let mut order: Vec<usize> = (0..entries.len()).collect();
    order.sort_by(|&a, &b| {
        score(&entries[a])
            .total_cmp(&score(&entries[b]))
            .then_with(|| entries[a].name.cmp(&entries[b].name))
    });
    for at in order {
        if total <= budget {
            break;
        }
        let entry = &mut entries[at];
        let before = chars(entry);
        entry.text = entry.name.clone();
        total -= before - chars(entry);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::Agent;
    use crate::agents::claude::Claude;
    use crate::probe_fs::ProbeFs;
    use crate::testing::Fixture;

    fn skill(fx: &Fixture, name: &str, front: &str) {
        fx.write(
            &fx.home_path(&format!(".claude/skills/{name}/SKILL.md")),
            &format!("---\nname: {name}\n{front}---\n\nbody\n"),
        );
    }

    fn blocks(fx: &Fixture) -> (AgentView, Vec<RenderedBlock>) {
        let env = fx.env();
        let fs = ProbeFs::new();
        let cx = ScanCx::new(&env, &fs);
        let mut view = Claude.scan(&cx).view;
        crate::passes::budget(&mut view);
        let rendered = Claude.render(&cx, &view);
        (view, rendered)
    }

    fn block(rendered: &[RenderedBlock], label: BlockLabel) -> &RenderedBlock {
        rendered
            .iter()
            .find(|b| b.label == label)
            .unwrap_or_else(|| panic!("no {label:?} block"))
    }

    #[test]
    fn listings_carry_each_entry_and_count_in_the_session_budget() {
        // Spec 4: a skill entry is `description - when_to_use`, a subagent entry the description.
        let fx = Fixture::new();
        skill(
            &fx,
            "deploy",
            "description: Ship it\nwhen_to_use: after a green build\n",
        );
        skill(&fx, "plain", "description: Just a skill\n");
        fx.write(
            &fx.home_path(".claude/agents/reviewer.md"),
            "---\nname: reviewer\ndescription: Reviews diffs\n---\n\nPrompt\n",
        );
        fx.write(&fx.home_path(".claude/CLAUDE.md"), "be kind\n");
        let (view, rendered) = blocks(&fx);

        let skills = block(&rendered, BlockLabel::SkillCatalog);
        assert_eq!(skills.fidelity, Fidelity::Approximate);
        assert!(
            skills
                .text
                .contains("- deploy: Ship it - after a green build")
        );
        assert!(skills.text.contains("- plain: Just a skill"));
        let agents = block(&rendered, BlockLabel::AgentCatalog);
        assert!(agents.text.contains("- reviewer: Reviews diffs"));
        assert_eq!(
            block(&rendered, BlockLabel::Instructions).fidelity,
            Fidelity::Exact
        );

        let counted: u32 = rendered.iter().map(|b| b.tokens).sum();
        assert_eq!(view.budget.session_start, counted);
        assert!(
            view.budget
                .by_label
                .iter()
                .any(|(l, t)| *l == BlockLabel::SkillCatalog && *t == skills.tokens)
        );
        assert_eq!(view.session.len(), rendered.len());
    }

    #[test]
    fn an_entry_is_cut_at_the_listing_limit() {
        let fx = Fixture::new();
        skill(
            &fx,
            "wordy",
            &format!("description: {}\n", "x".repeat(2_000)),
        );
        let (_, rendered) = blocks(&fx);
        let line = block(&rendered, BlockLabel::SkillCatalog)
            .text
            .lines()
            .find(|l| l.starts_with("- wordy: "))
            .unwrap()
            .to_owned();
        assert_eq!(line.len(), "- wordy: ".len() + 1_536);
    }

    #[test]
    fn past_the_budget_descriptions_go_from_the_least_used_skill_first() {
        // 8,000 characters at the assumed 200k window. 30 entries of about 330 characters are over.
        let fx = Fixture::new();
        for n in 0..30 {
            skill(
                &fx,
                &format!("s{n:02}"),
                &format!("description: {}\n", "d".repeat(320)),
            );
        }
        fx.write(
            &fx.home_path(".claude.json"),
            r#"{"skillUsage":{"s29":{"usageCount":9,"lastUsedAt":1000}}}"#,
        );
        let (_, rendered) = blocks(&fx);
        let text = &block(&rendered, BlockLabel::SkillCatalog).text;
        let names: Vec<&str> = text.lines().filter(|l| l.starts_with("- s")).collect();
        assert_eq!(names.len(), 30, "every name stays");
        assert!(
            names.contains(&"- s00"),
            "an unused skill keeps only its name"
        );
        assert!(
            names.iter().any(|l| l.starts_with("- s29: ddd")),
            "the used skill keeps its description"
        );
        let body_chars: usize = names.iter().map(|l| l.len() - 2).sum();
        assert!(body_chars <= 8_000, "{body_chars}");
    }

    fn agent(fx: &Fixture, dir_home: bool, name: &str, front: &str) {
        let path = if dir_home {
            fx.home_path(&format!(".claude/agents/{name}.md"))
        } else {
            fx.project_path(&format!(".claude/agents/{name}.md"))
        };
        fx.write(
            &path,
            &format!("---\nname: {name}\ndescription: d-{name}\n{front}---\n\nPrompt\n"),
        );
    }

    #[test]
    fn the_subagent_listing_comes_first_in_a_system_message() {
        // Spec 4, capture of 2.1.329: one system-role message, agents then skills.
        let fx = Fixture::new();
        skill(&fx, "one", "description: d\n");
        agent(&fx, true, "helper", "");
        let (view, rendered) = blocks(&fx);
        let labels: Vec<BlockLabel> = view.session.iter().map(|b| b.label).collect();
        assert_eq!(labels, [BlockLabel::AgentCatalog, BlockLabel::SkillCatalog]);
        assert!(
            view.session
                .iter()
                .all(|b| b.delivery == Delivery::SystemMessage
                    && b.fidelity == Fidelity::Approximate)
        );
        assert_eq!(view.session[0].preceded_by, [Unsized::FirstPrompt]);
        assert!(view.session[1].preceded_by.is_empty());
        assert_eq!(
            block(&rendered, BlockLabel::AgentCatalog).text,
            "Available agent types for the Agent tool:\n- helper: d-helper (Tools: All tools)\n\nWhen you launch multiple agents for independent work, send them in a single message with multiple tool uses so they run concurrently."
        );
        assert_eq!(
            block(&rendered, BlockLabel::SkillCatalog).text,
            "The following skills are available for use with the Skill tool:\n\n- one: d"
        );
    }

    #[test]
    fn a_subagent_entry_ends_in_its_tools() {
        // The capture: tools minus disallowedTools, `All tools [except ..]` when `tools` is unset.
        let fx = Fixture::new();
        agent(&fx, false, "a-none", "");
        agent(&fx, false, "b-list", "tools: Read, Grep\n");
        agent(&fx, false, "c-seq", "tools:\n  - Read\n  - Grep\n");
        agent(&fx, false, "d-deny", "disallowedTools: Write, Edit\n");
        agent(
            &fx,
            false,
            "e-both",
            "tools: Read, Grep, Bash\ndisallowedTools: Bash\n",
        );
        agent(&fx, false, "f-gone", "tools: Read\ndisallowedTools: Read\n");
        agent(&fx, false, "g-star", "tools: '*'\n");
        let (_, rendered) = blocks(&fx);
        let suffixes: Vec<&str> = block(&rendered, BlockLabel::AgentCatalog)
            .text
            .lines()
            .filter_map(|l| l.strip_prefix("- "))
            .filter_map(|l| l.split_once("(Tools: "))
            .map(|(_, t)| t.trim_end_matches(')'))
            .collect();
        assert_eq!(
            suffixes,
            [
                "All tools",
                "Read, Grep",
                "Read, Grep",
                "All tools except Write, Edit",
                "Read, Grep",
                "None",
                "All tools"
            ]
        );
    }

    #[test]
    fn entries_are_ordered_as_the_capture_shows() {
        // Agents by name across scopes, skills user then project, commands last, all ignoring case.
        let fx = Fixture::new();
        agent(&fx, true, "ua-zz", "");
        agent(&fx, true, "ua-AA", "");
        agent(&fx, false, "aa-proj", "");
        skill(&fx, "u-Zed", "description: d\n");
        skill(&fx, "u-alpha", "description: d\n");
        fx.write(
            &fx.project_path(".claude/skills/p-first/SKILL.md"),
            "---\nname: p-first\ndescription: d\n---\n\nbody\n",
        );
        fx.write(
            &fx.project_path(".claude/commands/acmd.md"),
            "---\ndescription: d\n---\n\nbody\n",
        );
        fx.write(
            &fx.home_path(".claude/commands/zcmd.md"),
            "---\ndescription: d\n---\n\nbody\n",
        );
        let (_, rendered) = blocks(&fx);
        let names = |label| -> Vec<String> {
            block(&rendered, label)
                .text
                .lines()
                .filter_map(|l| l.strip_prefix("- "))
                .map(|l| l.split(':').next().unwrap().to_owned())
                .collect()
        };
        assert_eq!(
            names(BlockLabel::AgentCatalog),
            ["aa-proj", "ua-AA", "ua-zz"]
        );
        assert_eq!(
            names(BlockLabel::SkillCatalog),
            ["u-alpha", "u-Zed", "p-first", "acmd", "zcmd"]
        );
    }
}
