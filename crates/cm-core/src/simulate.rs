//! The path simulator (design section 3, "The path simulator reuses the rules"). It evaluates
//! the `OnAccess` loads of a snapshot for one target path. For the directories between `cwd` and
//! the target that the project walk did not enter, such as ignored ones, it asks the adapter's
//! `on_access`, the one implementation of each agent's conditional rules.

use std::collections::HashSet;

use crate::agents::{Agent, ScanCx};
use crate::glob::RuleGlobs;
use crate::model::*;
use crate::probe_fs::EntryKind;
use crate::walk;

pub fn simulate(
    agents: &[&dyn Agent],
    cx: &ScanCx,
    snapshot: &Snapshot,
    target: &AbsPath,
) -> Vec<Simulation> {
    let touched = match cx.fs.stat(target) {
        Ok(stat) if stat.kind == EntryKind::Dir => target.clone(),
        _ => target.parent().unwrap_or_else(AbsPath::root),
    };
    let unwalked = unwalked_dirs(cx, &touched);
    agents
        .iter()
        .filter_map(|agent| {
            let view = snapshot.agents.iter().find(|v| v.agent == agent.id())?;
            Some(for_agent(*agent, cx, view, target, &touched, &unwalked))
        })
        .collect()
}

/// The directories from just below `cwd` down to `touched` that the walk never entered.
fn unwalked_dirs(cx: &ScanCx, touched: &AbsPath) -> Vec<AbsPath> {
    let cwd = &cx.env.cwd;
    let mut chain = Vec::new();
    if touched.starts_with(cwd) {
        let mut at = touched.clone();
        while at != *cwd {
            chain.push(at.clone());
            match at.parent() {
                Some(parent) => at = parent,
                None => break,
            }
        }
        chain.reverse();
    } else {
        chain.push(touched.clone());
    }
    let walked: HashSet<AbsPath> = walk::walk(cwd, cx.fs).dirs().cloned().collect();
    chain.retain(|dir| !walked.contains(dir));
    chain
}

fn for_agent(
    agent: &dyn Agent,
    cx: &ScanCx,
    view: &AgentView,
    target: &AbsPath,
    touched: &AbsPath,
    unwalked: &[AbsPath],
) -> Simulation {
    let mut fired: Vec<LoadId> = view
        .loads
        .iter()
        .filter(|load| matches!(&load.when, Activation::OnAccess { rule } if fires(rule, target, touched)))
        .map(|load| load.id.clone())
        .collect();
    // An import is active exactly when its host is.
    loop {
        let before = fired.len();
        for load in &view.loads {
            if let Via::Import { host, .. } = &load.via
                && fired.contains(host)
                && !fired.contains(&load.id)
            {
                fired.push(load.id.clone());
            }
        }
        if fired.len() == before {
            break;
        }
    }
    fired.sort_by_key(|id| id.0);

    let mut extra: Vec<Load> = Vec::new();
    let mut seen: HashSet<AbsPath> = view.loads.iter().map(|load| load.file.clone()).collect();
    for dir in unwalked {
        for candidate in agent.on_access(cx, dir) {
            if fires(&candidate.access, target, touched) && seen.insert(candidate.file.clone()) {
                let id = LoadId((view.loads.len() + extra.len()) as u32);
                extra.push(extra_load(cx, id, candidate));
            }
        }
    }

    let tokens_of = |load: &Load| load.delivered.as_ref().map_or(0, |d| d.tokens);
    let added_tokens = fired
        .iter()
        .filter_map(|id| view.loads.get(id.0 as usize))
        .map(tokens_of)
        .sum::<u32>()
        + extra.iter().map(tokens_of).sum::<u32>();

    Simulation {
        agent: view.agent.clone(),
        fired,
        extra,
        added_tokens,
        notes: notes(view, touched),
    }
}

fn fires(rule: &AccessRule, target: &AbsPath, touched: &AbsPath) -> bool {
    match rule {
        AccessRule::UnderDir { dir } => touched.starts_with(dir),
        AccessRule::Glob { base, patterns } => RuleGlobs::new(patterns).matches(base, target),
    }
}

fn extra_load(cx: &ScanCx, id: LoadId, candidate: crate::agents::Candidate) -> Load {
    let (status, delivered) = match cx.fs.read(&candidate.file) {
        Ok(bytes) if bytes.iter().all(u8::is_ascii_whitespace) => {
            (LoadStatus::Skipped { skip: Skip::Empty }, None)
        }
        Ok(bytes) => (
            LoadStatus::Loaded,
            Some(Delivered {
                bytes: bytes.len() as u64,
                tokens: crate::tokens::count_cached(&Revision::of(&bytes), &bytes),
                transformed: false,
            }),
        ),
        Err(err) if err.is_not_found() => (
            LoadStatus::Skipped {
                skip: Skip::Missing,
            },
            None,
        ),
        Err(err) => (
            LoadStatus::Skipped {
                skip: Skip::Invalid { detail: err.detail },
            },
            None,
        ),
    };
    Load {
        id,
        file: candidate.file,
        kind: candidate.kind,
        scope: candidate.scope,
        via: Via::Discovery {
            rule: candidate.rule,
        },
        when: Activation::OnAccess {
            rule: candidate.access,
        },
        status,
        delivered,
    }
}

/// Facts the loads carry but the access does not use: a file in the touched directory tree that
/// the agent leaves to the model, with the rule that says so.
fn notes(view: &AgentView, touched: &AbsPath) -> Vec<RuleRef> {
    let mut out: Vec<RuleRef> = Vec::new();
    for load in &view.loads {
        let Via::Discovery { rule } = &load.via else {
            continue;
        };
        let Scope::Project { dir } = &load.scope else {
            continue;
        };
        if load.when == Activation::ModelMayRead && touched.starts_with(dir) && !out.contains(rule)
        {
            out.push(rule.clone());
        }
    }
    out
}
