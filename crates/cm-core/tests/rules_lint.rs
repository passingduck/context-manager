//! Every rule id the code names resolves in a rules file, and every rule in a rules file is
//! explained in both languages, backed by evidence, and used (`AGENTS.md` rule 3).
//!
//! The scan reads the production source of every crate. A rule id reaches `RuleRef::new` through
//! helpers such as `invalid`, `rule` and `discovery`, so the scan does not follow calls. It reads
//! every string literal of the shape `<namespace>.<name>` for the namespaces below.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use cm_core::RuleSpec;
use cm_core::agents::{core_rules, registry};
use regex::Regex;

/// The first segment of every rule id, and the rules file that defines the ids under it.
const NAMESPACES: &[(&str, &str)] = &[
    ("claude", "crates/cm-core/src/agents/claude/rules.toml"),
    ("codex", "crates/cm-core/src/agents/codex/rules.toml"),
    ("gemini", "crates/cm-core/src/agents/gemini/rules.toml"),
    ("edit", "crates/cm-core/src/core_rules.toml"),
    ("search", "crates/cm-core/src/core_rules.toml"),
    ("hook", "crates/cm-core/src/core_rules.toml"),
    ("host", "crates/cm-core/src/core_rules.toml"),
];

/// Literals in a rule namespace that are not rule ids.
const NOT_RULES: &[&str] = &[
    // Memory store ids.
    "claude.auto",
    "gemini.private",
    "gemini.private.sandbox",
    // File names.
    "claude.json",
];

/// Rules that no code names yet. Delete an entry when code that emits it lands.
const NOT_USED_YET: &[&str] = &[
    // Explains where the catalogs sit in a request. A session block has no field for a rule.
    "claude.catalogs",
];

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("the crate sits two levels below the workspace root")
        .to_path_buf()
}

fn production_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).expect("a source directory reads") {
        let path = entry.expect("a directory entry reads").path();
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if path.is_dir() {
            if name != "tests" {
                production_files(&path, out);
            }
        } else if name.ends_with(".rs") && !name.ends_with("tests.rs") && name != "testing.rs" {
            out.push(path);
        }
    }
}

/// The text of a file up to its inline `#[cfg(test)] mod tests {` block.
fn production_text(path: &Path) -> String {
    let text = fs::read_to_string(path).expect("a source file reads");
    match text.find("#[cfg(test)]\nmod tests {") {
        Some(at) => text[..at].to_owned(),
        None => text,
    }
}

fn sources() -> Vec<(PathBuf, String)> {
    let root = workspace_root();
    let mut files = Vec::new();
    for krate in fs::read_dir(root.join("crates")).expect("crates reads") {
        let src = krate.expect("a crate entry reads").path().join("src");
        if src.is_dir() {
            production_files(&src, &mut files);
        }
    }
    files.sort();
    files
        .into_iter()
        .map(|path| {
            let text = production_text(&path);
            (path, text)
        })
        .collect()
}

fn all_rules() -> Vec<(String, &'static RuleSpec)> {
    let mut rules: Vec<_> = core_rules()
        .iter()
        .map(|r| ("core".to_owned(), r))
        .collect();
    for agent in registry() {
        let owner = agent.id().as_str().to_owned();
        rules.extend(agent.rules().iter().map(|r| (owner.clone(), r)));
    }
    rules
}

/// The rule ids a source text names. Comments are dropped. A slot id or a memory store id has the
/// same shape as a rule id, so the literal in the `id` position of `slot(...)` or of a store is
/// not a rule.
fn literal_ids(text: &str) -> BTreeSet<String> {
    let namespaces = NAMESPACES
        .iter()
        .map(|(n, _)| *n)
        .collect::<Vec<_>>()
        .join("|");
    let id = format!(r#""((?:{namespaces})(?:\.[a-z0-9_]+)+)""#);
    let other_kind = Regex::new(&format!(
        r#"(?:\bslot\(\s*|\bid:\s*)"{}""#,
        &id[1..id.len() - 1]
    ))
    .unwrap();
    let code: String = text
        .lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n");
    let not_rules: BTreeSet<String> = other_kind
        .captures_iter(&code)
        .map(|c| c[1].to_owned())
        .collect();
    Regex::new(&id)
        .unwrap()
        .captures_iter(&code)
        .map(|c| c[1].to_owned())
        .filter(|id| !NOT_RULES.contains(&id.as_str()) && !not_rules.contains(id))
        .collect()
}

#[test]
fn every_rule_id_the_code_names_resolves_in_a_rules_file() {
    let defined: BTreeSet<&str> = all_rules().iter().map(|(_, r)| r.id.0.as_str()).collect();
    let root = workspace_root();
    let mut seen = 0;
    let mut missing = Vec::new();
    for (path, text) in sources() {
        for id in literal_ids(&text) {
            seen += 1;
            if !defined.contains(id.as_str()) {
                let namespace = id.split('.').next().unwrap();
                let file = NAMESPACES
                    .iter()
                    .find(|(n, _)| *n == namespace)
                    .map(|(_, file)| *file)
                    .unwrap();
                missing.push(format!(
                    "`{id}` is named in {} and should be defined in {file}",
                    path.strip_prefix(&root).unwrap().display()
                ));
            }
        }
    }
    assert!(seen > 100, "the scan found only {seen} rule ids");
    assert!(missing.is_empty(), "\n{}", missing.join("\n"));
}

#[test]
fn every_rule_is_explained_backed_by_evidence_and_used() {
    let used: BTreeSet<String> = sources()
        .iter()
        .flat_map(|(_, text)| literal_ids(text))
        .collect();
    let mut seen = BTreeSet::new();
    for (owner, rule) in all_rules() {
        let id = rule.id.0.as_str();
        assert!(seen.insert(id), "`{id}` is defined twice");
        if owner != "core" {
            assert!(
                id.starts_with(&format!("{owner}.")),
                "`{id}` is in the {owner} rules file"
            );
        }
        for (what, text) in [
            ("English", &rule.en),
            ("Korean", &rule.ko),
            ("evidence", &rule.evidence),
        ] {
            assert!(!text.trim().is_empty(), "`{id}` has no {what}");
        }
        for symbol in ['→', '←', '↔', '⇒', '·', '・', '‧', '∙'] {
            assert!(
                !rule.ko.contains(symbol),
                "the Korean of `{id}` contains {symbol}"
            );
        }
        assert!(
            used.contains(id) || NOT_USED_YET.contains(&id),
            "`{id}` is never named in the code"
        );
    }
}
