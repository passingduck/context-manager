//! The skill listing and the subagent listing of the session (spec 4, "What reaches the model").
//! Claude Code sends them as attachments of the first turn. The literal framing is not pinned
//! by an oracle run, so both blocks are `Approximate`. What is exact: each entry, its cut at
//! `skillListingMaxDescChars`, and the character budget of the whole skill listing.

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

struct Entry {
    load: LoadId,
    name: String,
    /// `name: description`, as the cut leaves it.
    text: String,
}

/// Skill and command entries share one listing and one budget. Subagents have their own.
pub(super) fn build(cx: &ScanCx, setup: &Setup, extensions: &[Extension]) -> Vec<Built> {
    let max_desc = max_desc_chars(setup);
    let mut skills: Vec<Entry> = Vec::new();
    let mut agents: Vec<Entry> = Vec::new();
    for ext in extensions {
        let Some(load) = &ext.listing else { continue };
        let when_to_use = when_to_use(cx, &ext.file);
        let (text, _) = listing_entry(
            ext.ext_kind,
            &ext.name,
            ext.description.as_deref().unwrap_or(""),
            when_to_use.as_deref(),
            max_desc,
        );
        let entry = Entry {
            load: load.clone(),
            name: ext.name.clone(),
            text,
        };
        match ext.ext_kind {
            ExtKind::Subagent => agents.push(entry),
            _ => skills.push(entry),
        }
    }
    fit_to_budget(
        &mut skills,
        listing_budget_chars(cx, setup, ASSUMED_WINDOW_TOKENS),
        setup,
    );
    let mut out = Vec::new();
    for (label, intro, entries) in [
        (BlockLabel::SkillCatalog, SKILL_INTRO, skills),
        (BlockLabel::AgentCatalog, AGENT_INTRO, agents),
    ] {
        if entries.is_empty() {
            continue;
        }
        let pieces: Vec<(LoadId, String)> = entries
            .into_iter()
            .map(|e| (e.load, format!("- {}", e.text)))
            .collect();
        let body: Vec<&str> = pieces.iter().map(|(_, text)| text.as_str()).collect();
        let text = format!(
            "<system-reminder>\n{intro}\n\n{}\n</system-reminder>",
            body.join("\n")
        );
        out.push(built(
            label,
            text,
            pieces
                .iter()
                .map(|(id, t)| (id.clone(), t.as_str()))
                .collect(),
            Fidelity::Approximate,
        ));
    }
    out
}

fn when_to_use(cx: &ScanCx, file: &AbsPath) -> Option<String> {
    let bytes = cx.fs.read(file).ok()?;
    let fm = frontmatter::parse(&String::from_utf8_lossy(&bytes));
    let value = fm.map?.get("when_to_use")?.as_str()?.to_owned();
    (!value.trim().is_empty()).then_some(value)
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
        let cx = ScanCx {
            env: &env,
            fs: &fs,
            parked_dir: None,
        };
        let view = Claude.scan(&cx).view;
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
}
