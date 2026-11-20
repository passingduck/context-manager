//! Skills, custom commands and subagents (spec 6).

use std::collections::HashMap;

use serde_json::Value;

use super::context::Builder;
use super::util::{cmp_utf16, files_with_ext, is_dir, is_file, read_lossy, rule};
use super::world::World;
use crate::agents::FileClaim;
use crate::md::frontmatter;
use crate::model::*;
use crate::probe_fs::EntryKind;
use crate::tokens;
use crate::walk::{self, Listed};

pub(super) struct Exts {
    pub items: Vec<Extension>,
    pub claims: Vec<FileClaim>,
    pub diagnostics: Vec<Diagnostic>,
    /// Active skills in catalog order, with their listing load.
    pub catalog: Vec<(LoadId, String, String, AbsPath)>,
}

fn ext_id(kind: &str, file: &AbsPath) -> ExtId {
    ExtId(format!(
        "gemini:{}",
        &sha256_hex(format!("gemini\0{kind}\0{file}").as_bytes())[..24]
    ))
}

/// What a discovered item is, before the settings and the trust decide its status.
struct Found {
    name: String,
    description: Option<String>,
    file: AbsPath,
    scope: Scope,
    /// Text that loads on invoke.
    body: String,
    /// The tool's own copy under `~/.gemini/extensions`: `gemini extensions update` replaces it.
    plugin: Option<String>,
    problem: Option<&'static str>,
}

pub(super) fn scan(b: &mut Builder) -> Exts {
    let mut out = Exts {
        items: Vec::new(),
        claims: Vec::new(),
        diagnostics: Vec::new(),
        catalog: Vec::new(),
    };
    skills(b, &mut out);
    commands(b, &mut out);
    agents(b, &mut out);
    out
}

fn plugin_claim(out: &mut Exts, found: &Found) {
    if let Some(plugin) = &found.plugin {
        out.claims.push(FileClaim {
            path: found.file.clone(),
            policy: EditPolicy::ReadOnly {
                why: ReadOnlyWhy::PluginCache {
                    plugin: plugin.clone(),
                },
            },
        });
    }
}

fn ext_plugin(ext: &super::extension::ExtInfo) -> Option<String> {
    (!ext.linked).then(|| ext.name.clone())
}

// ---------------------------------------------------------------------------------------------
// Skills
// ---------------------------------------------------------------------------------------------

fn skills(b: &mut Builder, out: &mut Exts) {
    let w = b.w;
    let skills_on = w.settings.bool("skills.enabled", true);
    let disabled: Vec<String> = w
        .settings
        .strings("skills.disabled")
        .iter()
        .map(|n| n.to_lowercase())
        .collect();

    // Lowest precedence first (spec 6, "Agent Skills"). `true` marks a tier that needs trust.
    let mut tiers: Vec<(AbsPath, Scope, bool, Option<String>)> = Vec::new();
    for ext in w.exts.iter().filter(|e| e.active()) {
        tiers.push((
            ext.root.join("skills"),
            Scope::Plugin {
                id: ext.name.clone(),
            },
            false,
            ext_plugin(ext),
        ));
    }
    tiers.push((w.gdir.join("skills"), Scope::User, false, None));
    tiers.push((w.home.join(".agents/skills"), Scope::User, false, None));
    let project = Scope::Project { dir: w.cwd.clone() };
    tiers.push((w.cwd.join(".gemini/skills"), project.clone(), true, None));
    tiers.push((w.cwd.join(".agents/skills"), project, true, None));

    struct Entry {
        found: Found,
        hidden: bool,
        shadowed_by: Option<usize>,
    }
    let mut entries: Vec<Entry> = Vec::new();
    let mut winner: HashMap<String, usize> = HashMap::new();
    let mut order: Vec<String> = Vec::new();
    for (dir, scope, needs_trust, plugin) in tiers {
        for file in skill_files(w, &dir) {
            if entries.iter().any(|e| e.found.file == file) {
                continue;
            }
            let found = read_skill(w, &file, scope.clone(), plugin.clone());
            let hidden = needs_trust && !w.trusted();
            let index = entries.len();
            if !hidden && found.problem.is_none() {
                match winner.insert(found.name.clone(), index) {
                    Some(old) => entries[old].shadowed_by = Some(index),
                    None => order.push(found.name.clone()),
                }
            }
            entries.push(Entry {
                found,
                hidden,
                shadowed_by: None,
            });
        }
    }

    let ids: Vec<ExtId> = entries
        .iter()
        .map(|e| ext_id("skill", &e.found.file))
        .collect();
    for (index, entry) in entries.iter().enumerate() {
        let found = &entry.found;
        plugin_claim(out, found);
        let (status, skip): (ExtStatus, Option<Skip>) = if entry.hidden {
            (
                ExtStatus::Hidden {
                    why: rule("gemini.untrusted_project"),
                },
                Some(Skip::Untrusted),
            )
        } else if let Some(problem) = found.problem {
            (
                ExtStatus::Rejected { why: rule(problem) },
                Some(Skip::Invalid {
                    why: Invalid::Frontmatter,
                }),
            )
        } else if let Some(by) = entry.shadowed_by {
            (
                ExtStatus::Shadowed {
                    by: ids[by].clone(),
                },
                Some(Skip::Shadowed {
                    by: entries[by].found.file.clone(),
                    how: Shadow::NameCollision,
                }),
            )
        } else if !skills_on {
            (
                ExtStatus::Disabled {
                    by: "skills.enabled".to_owned(),
                },
                Some(Skip::FeatureOff {
                    name: "skills.enabled".to_owned(),
                }),
            )
        } else if disabled.contains(&found.name.to_lowercase()) {
            (
                ExtStatus::Disabled {
                    by: "skills.disabled".to_owned(),
                },
                Some(Skip::FeatureOff {
                    name: "skills.disabled".to_owned(),
                }),
            )
        } else {
            (ExtStatus::Active, None)
        };
        let snippet = format!(
            "  <skill>\n    <name>{}</name>\n    <description>{}</description>\n    <location>{}</location>\n  </skill>",
            found.name,
            found.description.clone().unwrap_or_default(),
            found.file
        );
        let listing = found.problem.is_none().then(|| {
            let id = b.next_id();
            b.push(Load {
                id: LoadId(id.0),
                file: found.file.clone(),
                kind: DocKind::Listing,
                scope: found.scope.clone(),
                via: Via::Listing {
                    ext: ids[index].clone(),
                },
                when: Activation::SessionStart,
                status: match &skip {
                    Some(skip) => LoadStatus::Skipped { skip: skip.clone() },
                    None => LoadStatus::Loaded,
                },
                delivered: skip.is_none().then(|| Delivered {
                    bytes: snippet.len() as u64,
                    tokens: tokens::count(&snippet),
                    transformed: true,
                }),
            })
        });
        let body = push_body(b, found, DocKind::SkillBody, "gemini.skill", &skip, true);
        out.items.push(Extension {
            id: ids[index].clone(),
            ext_kind: ExtKind::Skill,
            name: found.name.clone(),
            description: found.description.clone(),
            file: found.file.clone(),
            scope: found.scope.clone(),
            listing,
            body,
            status,
        });
    }
    for name in order {
        let item = &out.items[winner[&name]];
        if let (ExtStatus::Active, Some(listing)) = (&item.status, &item.listing) {
            out.catalog.push((
                listing.clone(),
                item.name.clone(),
                item.description.clone().unwrap_or_default(),
                item.file.clone(),
            ));
        }
    }
}

fn skill_files(w: &World, dir: &AbsPath) -> Vec<AbsPath> {
    let mut files = Vec::new();
    let top = dir.join("SKILL.md");
    if is_file(w.cx, &top) {
        files.push(top);
    }
    for entry in walk::list_dir(w.cx.fs, dir) {
        if entry.kind == EntryKind::Dir && entry.name() != "node_modules" && entry.name() != ".git"
        {
            let file = entry.path.join("SKILL.md");
            if is_file(w.cx, &file) {
                files.push(file);
            }
        }
    }
    files
}

fn read_skill(w: &World, file: &AbsPath, scope: Scope, plugin: Option<String>) -> Found {
    let text = read_lossy(w.cx, file).unwrap_or_default();
    let fallback = file
        .parent()
        .and_then(|p| p.file_name().map(str::to_owned))
        .unwrap_or_default();
    let mut found = Found {
        name: fallback,
        description: None,
        file: file.clone(),
        scope,
        body: String::new(),
        plugin,
        problem: Some("gemini.skill.invalid_frontmatter"),
    };
    let fm = frontmatter::parse(&text);
    if !fm.is_present() {
        return found;
    }
    let pair = match &fm.map {
        Some(map) => match (
            map.get("name").and_then(Value::as_str),
            map.get("description").and_then(Value::as_str),
        ) {
            (Some(n), Some(d)) => Some((n.to_owned(), d.to_owned())),
            _ => None,
        },
        None => fm
            .block
            .as_ref()
            .and_then(|block| simple_frontmatter(&text[block.clone()])),
    };
    let Some((name, description)) = pair else {
        return found;
    };
    found.name = name.replace([':', '\\', '/', '<', '>', '*', '?', '"', '|'], "-");
    found.description = Some(description);
    found.body = text[fm.body].trim().to_owned();
    found.problem = None;
    found
}

/// `parseSimpleFrontmatter`: a fallback for a description that holds a colon.
fn simple_frontmatter(block: &str) -> Option<(String, String)> {
    let mut name = None;
    let mut description = None;
    let lines: Vec<&str> = block.lines().collect();
    let inner = lines.get(1..lines.len().saturating_sub(1))?;
    let mut i = 0;
    while i < inner.len() {
        let line = inner[i];
        let lower = line.trim_start().to_lowercase();
        if let Some(rest) = lower.strip_prefix("name")
            && rest.trim_start().starts_with(':')
        {
            let value = line.trim_start()[4..]
                .trim_start()
                .trim_start_matches(':')
                .trim();
            name = Some(unquote(value));
        } else if let Some(rest) = lower.strip_prefix("description")
            && rest.trim_start().starts_with(':')
        {
            let value = line.trim_start()[11..]
                .trim_start()
                .trim_start_matches(':')
                .trim();
            let mut parts = vec![value.to_owned()];
            while let Some(next) = inner.get(i + 1) {
                let key_like = next.trim_start().split(':').next().is_some_and(|k| {
                    !k.is_empty()
                        && k.chars()
                            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
                        && next.contains(':')
                });
                if next.trim().is_empty() || next.trim_start().starts_with('#') || key_like {
                    break;
                }
                parts.push(next.trim().to_owned());
                i += 1;
            }
            description = Some(unquote(
                &parts
                    .into_iter()
                    .filter(|p| !p.is_empty())
                    .collect::<Vec<_>>()
                    .join(" "),
            ));
        }
        i += 1;
    }
    Some((name?, description?))
}

fn unquote(value: &str) -> String {
    let quoted = value.len() >= 2
        && ((value.starts_with('"') && value.ends_with('"'))
            || (value.starts_with('\'') && value.ends_with('\'')));
    if quoted {
        value[1..value.len() - 1].to_owned()
    } else {
        value.to_owned()
    }
}

fn push_body(
    b: &mut Builder,
    found: &Found,
    doc: DocKind,
    rule_id: &'static str,
    skip: &Option<Skip>,
    transformed: bool,
) -> LoadId {
    let id = b.next_id();
    b.push(Load {
        id,
        file: found.file.clone(),
        kind: doc,
        scope: found.scope.clone(),
        via: Via::Discovery {
            rule: rule(rule_id),
        },
        when: Activation::OnInvoke,
        status: match skip {
            Some(skip) => LoadStatus::Skipped { skip: skip.clone() },
            None => LoadStatus::Loaded,
        },
        delivered: skip.is_none().then(|| Delivered {
            bytes: found.body.len() as u64,
            tokens: tokens::count(&found.body),
            transformed,
        }),
    })
}

// ---------------------------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------------------------

/// `FileCommandLoader.parseAndAdaptFile`: path segments joined by `:`, characters outside
/// `[A-Za-z0-9_.-]` become `_`, and a segment over 50 characters is cut to 47 plus `...`.
pub(super) fn command_name(relative: &str) -> String {
    relative
        .trim_end_matches(".toml")
        .split('/')
        .map(|segment| {
            let clean: String = segment
                .chars()
                .map(|c| {
                    if c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.') {
                        c
                    } else {
                        '_'
                    }
                })
                .collect();
            if clean.chars().count() > 50 {
                format!("{}...", clean.chars().take(47).collect::<String>())
            } else {
                clean
            }
        })
        .collect::<Vec<_>>()
        .join(":")
}

fn commands(b: &mut Builder, out: &mut Exts) {
    let w = b.w;
    struct Source {
        dir: AbsPath,
        scope: Scope,
        needs_trust: bool,
        ext: Option<String>,
        plugin: Option<String>,
    }
    let mut sources = vec![Source {
        dir: w.gdir.join("commands"),
        scope: Scope::User,
        needs_trust: false,
        ext: None,
        plugin: None,
    }];
    if !w.cwd_is_home {
        sources.push(Source {
            dir: w.cwd.join(".gemini/commands"),
            scope: Scope::Project { dir: w.cwd.clone() },
            needs_trust: true,
            ext: None,
            plugin: None,
        });
    }
    let mut active: Vec<_> = w.exts.iter().filter(|e| e.active()).collect();
    active.sort_by(|a, b| a.name.cmp(&b.name));
    for ext in active {
        sources.push(Source {
            dir: ext.root.join("commands"),
            scope: Scope::Plugin {
                id: ext.name.clone(),
            },
            needs_trust: true,
            ext: Some(ext.name.clone()),
            plugin: ext_plugin(ext),
        });
    }

    let mut taken: HashMap<String, usize> = HashMap::new();
    for source in sources {
        if !is_dir(w.cx, &source.dir) {
            continue;
        }
        for file in files_with_ext(w.cx, &source.dir, ".toml") {
            let relative = &file.as_str()[source.dir.as_str().len() + 1..];
            let mut name = command_name(relative);
            let text = read_lossy(w.cx, &file).unwrap_or_default();
            let parsed = toml::from_str::<toml::Table>(&text).ok();
            let prompt = parsed
                .as_ref()
                .and_then(|t| t.get("prompt")?.as_str().map(str::to_owned));
            let description = parsed
                .as_ref()
                .and_then(|t| t.get("description")?.as_str().map(str::to_owned));
            let found = Found {
                name: name.clone(),
                description,
                file: file.clone(),
                scope: source.scope.clone(),
                body: prompt.clone().unwrap_or_default(),
                plugin: source.plugin.clone(),
                problem: prompt.is_none().then_some("gemini.command.invalid"),
            };
            plugin_claim(out, &found);
            let hidden = source.needs_trust && !w.trusted();
            if let (Some(ext), None, false) = (&source.ext, found.problem, hidden)
                && taken.contains_key(&name)
            {
                name = format!("{ext}.{name}");
            }
            let id = ext_id("command", &file);
            let (status, skip) = if hidden {
                (
                    ExtStatus::Hidden {
                        why: rule("gemini.untrusted_project"),
                    },
                    Some(Skip::Untrusted),
                )
            } else if let Some(problem) = found.problem {
                (
                    ExtStatus::Rejected { why: rule(problem) },
                    Some(Skip::Invalid { why: Invalid::Toml }),
                )
            } else {
                (ExtStatus::Active, None)
            };
            let index = out.items.len();
            if status == ExtStatus::Active
                && let Some(old) = taken.insert(name.clone(), index)
            {
                // Project replaces user (spec 6, custom commands).
                out.items[old].status = ExtStatus::Shadowed { by: id.clone() };
            }
            let body = push_body(
                b,
                &found,
                DocKind::CommandBody,
                "gemini.command",
                &skip,
                false,
            );
            out.items.push(Extension {
                id,
                ext_kind: ExtKind::Command,
                name,
                description: found.description.clone(),
                file,
                scope: source.scope.clone(),
                listing: None,
                body,
                status,
            });
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Subagents
// ---------------------------------------------------------------------------------------------

fn agents(b: &mut Builder, out: &mut Exts) {
    let w = b.w;
    let agents_on = w.settings.bool("experimental.enableAgents", true);
    let ack_file = w.gdir.join("acknowledgments/agents.json");
    let ack: Value = read_lossy(w.cx, &ack_file)
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or(Value::Null);
    if is_file(w.cx, &ack_file) {
        out.claims.push(FileClaim {
            path: ack_file,
            policy: EditPolicy::ReadOnly {
                why: ReadOnlyWhy::AgentState,
            },
        });
    }
    struct Source {
        dir: AbsPath,
        scope: Scope,
        project: bool,
        plugin: Option<String>,
    }
    let mut sources = Vec::new();
    if !w.cwd_is_home {
        sources.push(Source {
            dir: w.cwd.join(".gemini/agents"),
            scope: Scope::Project { dir: w.cwd.clone() },
            project: true,
            plugin: None,
        });
    }
    sources.push(Source {
        dir: w.gdir.join("agents"),
        scope: Scope::User,
        project: false,
        plugin: None,
    });
    for ext in w.exts.iter().filter(|e| e.active()) {
        sources.push(Source {
            dir: ext.root.join("agents"),
            scope: Scope::Plugin {
                id: ext.name.clone(),
            },
            project: false,
            plugin: ext_plugin(ext),
        });
    }
    for source in sources {
        let entries = walk::list_dir(w.cx.fs, &source.dir);
        let mut names: Vec<&str> = entries
            .iter()
            .filter(|e| {
                e.kind == EntryKind::File && !e.name().starts_with('_') && e.name().ends_with(".md")
            })
            .map(Listed::name)
            .collect();
        names.sort_by(|a, b| cmp_utf16(a, b));
        for file_name in names {
            let file = source.dir.join(file_name);
            let text = read_lossy(w.cx, &file).unwrap_or_default();
            let fm = frontmatter::parse(&text);
            let fields = fm.map.as_ref().and_then(|m| {
                Some((
                    m.get("name")?.as_str()?.to_owned(),
                    m.get("description")?.as_str()?.to_owned(),
                ))
            });
            let found = Found {
                name: fields.as_ref().map_or_else(
                    || file_name.trim_end_matches(".md").to_owned(),
                    |(n, _)| n.clone(),
                ),
                description: fields.as_ref().map(|(_, d)| d.clone()),
                file: file.clone(),
                scope: source.scope.clone(),
                body: text[fm.body].trim().to_owned(),
                plugin: source.plugin.clone(),
                problem: fields
                    .is_none()
                    .then_some("gemini.agent.invalid_frontmatter"),
            };
            plugin_claim(out, &found);
            let hash = sha256_hex(text.as_bytes());
            let acknowledged = ack
                .get(w.cwd.as_str())
                .and_then(|m| m.get(&found.name))
                .and_then(Value::as_str)
                == Some(hash.as_str());
            let overridden_off = w
                .settings
                .get("agents.overrides")
                .and_then(|o| o.get(&found.name)?.get("enabled")?.as_bool())
                == Some(false);
            let (status, skip) = if source.project && !w.trusted() {
                (
                    ExtStatus::Hidden {
                        why: rule("gemini.untrusted_project"),
                    },
                    Some(Skip::Untrusted),
                )
            } else if let Some(problem) = found.problem {
                out.diagnostics.push(Diagnostic {
                    severity: Severity::Warning,
                    agent: None,
                    at: Subject::File {
                        path: file.clone(),
                        line: None,
                    },
                    problem: Problem::FrontmatterInvalid {
                        field: "name".to_owned(),
                    },
                    fix: None,
                });
                (
                    ExtStatus::Rejected { why: rule(problem) },
                    Some(Skip::Invalid {
                        why: Invalid::Frontmatter,
                    }),
                )
            } else if !agents_on {
                (
                    ExtStatus::Disabled {
                        by: "experimental.enableAgents".to_owned(),
                    },
                    Some(Skip::FeatureOff {
                        name: "experimental.enableAgents".to_owned(),
                    }),
                )
            } else if source.project && !acknowledged {
                (
                    ExtStatus::Hidden {
                        why: rule("gemini.agent.unacknowledged"),
                    },
                    Some(Skip::NotRead {
                        why: rule("gemini.agent.unacknowledged"),
                    }),
                )
            } else if overridden_off {
                (
                    ExtStatus::Disabled {
                        by: "agents.overrides".to_owned(),
                    },
                    Some(Skip::FeatureOff {
                        name: "agents.overrides".to_owned(),
                    }),
                )
            } else {
                (ExtStatus::Active, None)
            };
            let body = push_body(
                b,
                &found,
                DocKind::SubagentPrompt,
                "gemini.agent",
                &skip,
                true,
            );
            out.items.push(Extension {
                id: ext_id("subagent", &file),
                ext_kind: ExtKind::Subagent,
                name: found.name,
                description: found.description,
                file,
                scope: source.scope.clone(),
                listing: None,
                body,
                status,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_names_follow_the_path_with_colons() {
        assert_eq!(command_name("gcs/sync.toml"), "gcs:sync");
        assert_eq!(command_name("a b/c@d.toml"), "a_b:c_d");
        let long = format!("{}.toml", "x".repeat(60));
        assert_eq!(command_name(&long), format!("{}...", "x".repeat(47)));
    }

    #[test]
    fn the_simple_frontmatter_parser_keeps_a_description_with_a_colon() {
        let block = "---\nname: pdf\ndescription: Read PDFs: all of them\n---\n";
        assert_eq!(
            simple_frontmatter(block),
            Some(("pdf".to_owned(), "Read PDFs: all of them".to_owned()))
        );
    }
}
