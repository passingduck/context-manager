//! Fixture tests for the instruction, memory and guard rules that the oracle run does not
//! cover. Expected values come from `docs/agents/claude-code.md`, not from the code.

use super::*;
use crate::agents::{AgentScan, ScanCx};
use crate::env::ScanEnv;
use crate::probe_fs::ProbeFs;
use crate::testing::Fixture;

fn scan_with(env: &ScanEnv) -> (AgentScan, ProbeFs) {
    let fs = ProbeFs::new();
    let scan = {
        let cx = ScanCx { env, fs: &fs };
        Claude.scan(&cx)
    };
    (scan, fs)
}

fn scan_fx(fx: &Fixture) -> AgentScan {
    scan_with(&fx.env()).0
}

fn load_of<'a>(scan: &'a AgentScan, file: &AbsPath) -> Vec<&'a Load> {
    scan.view.loads.iter().filter(|l| &l.file == file).collect()
}

fn status_of(scan: &AgentScan, file: &AbsPath) -> LoadStatus {
    load_of(scan, file)
        .first()
        .unwrap_or_else(|| panic!("no load for {file}"))
        .status
        .clone()
}

fn skip_of(scan: &AgentScan, file: &AbsPath) -> Skip {
    match status_of(scan, file) {
        LoadStatus::Skipped { skip } => skip,
        other => panic!("{file}: {other:?}"),
    }
}

fn session_files(scan: &AgentScan) -> Vec<String> {
    scan.view
        .loads
        .iter()
        .filter(|l| {
            matches!(l.status, LoadStatus::Loaded | LoadStatus::Truncated { .. })
                && matches!(l.when, Activation::SessionStart | Activation::WithHost)
                && !matches!(l.kind, DocKind::MemoryTopic)
        })
        .map(|l| l.file.to_string())
        .collect()
}

// ---------------------------------------------------------------------------------------------
// Order and skips of spec 1.2
// ---------------------------------------------------------------------------------------------

#[test]
fn session_start_order_is_managed_user_ancestors_then_memory() {
    let fx = Fixture::new();
    let project = fx.project();
    let sub = fx.project_path("pkg");
    fx.write(&fx.home_path(".claude/CLAUDE.md"), "user");
    fx.write(&fx.home_path(".claude/rules/u.md"), "user rule");
    fx.write(&project.join("CLAUDE.md"), "root");
    fx.write(&project.join(".claude/CLAUDE.md"), "dot");
    fx.write(&project.join(".claude/rules/r.md"), "rule");
    fx.write(&project.join("CLAUDE.local.md"), "local");
    fx.write(&sub.join("CLAUDE.md"), "pkg");
    let key = memory::project_key(sub.as_str());
    let memdir = fx.home_path(".claude/projects").join(&key).join("memory");
    fx.write(&memdir.join("MEMORY.md"), "- index");
    let env = fx.env().with_cwd(sub.clone());
    let (scan, _) = scan_with(&env);
    let rel: Vec<String> = session_files(&scan)
        .iter()
        .map(|p| p.replace(fx.root().as_str(), "$"))
        .collect();
    assert_eq!(
        rel,
        [
            "$/home/.claude/CLAUDE.md".to_owned(),
            "$/home/.claude/rules/u.md".to_owned(),
            "$/project/CLAUDE.md".to_owned(),
            "$/project/.claude/CLAUDE.md".to_owned(),
            "$/project/.claude/rules/r.md".to_owned(),
            "$/project/CLAUDE.local.md".to_owned(),
            "$/project/pkg/CLAUDE.md".to_owned(),
            format!("$/home/.claude/projects/{key}/memory/MEMORY.md"),
        ]
    );
}

#[test]
fn skips_say_why_a_file_did_not_load() {
    let fx = Fixture::new();
    let p = fx.project();
    fx.write(&p.join("CLAUDE.md"), "   \n");
    fx.write(&p.join(".claude/CLAUDE.md"), "<!-- only a comment -->\n");
    fx.write(&p.join("CLAUDE.local.md"), "kept");
    fx.write(
        &fx.home_path(".claude/settings.json"),
        r#"{"claudeMdExcludes":["**/CLAUDE.local.md"]}"#,
    );
    let scan = scan_fx(&fx);
    assert_eq!(skip_of(&scan, &p.join("CLAUDE.md")), Skip::Empty);
    assert_eq!(skip_of(&scan, &p.join(".claude/CLAUDE.md")), Skip::Empty);
    assert_eq!(
        skip_of(&scan, &p.join("CLAUDE.local.md")),
        Skip::Excluded {
            pattern: "**/CLAUDE.local.md".into()
        }
    );
}

#[test]
fn a_file_over_four_mebibytes_is_skipped_whole() {
    let fx = Fixture::new();
    let big = fx.project_path("CLAUDE.md");
    fx.write(&big, &"word ".repeat(4 * 1024 * 1024 / 5 + 1));
    let ok = fx.project_path(".claude/CLAUDE.md");
    fx.write(&ok, &"word ".repeat(4 * 1024 * 1024 / 5));
    let scan = scan_fx(&fx);
    assert_eq!(
        skip_of(&scan, &big),
        Skip::TooLarge {
            limit: Limit {
                kind: LimitKind::MaxFileBytes,
                value: 4_194_304,
                unit: Unit::Bytes
            }
        }
    );
    assert_eq!(status_of(&scan, &ok), LoadStatus::Loaded);
}

#[test]
fn a_scratchpad_shaped_folder_is_refused() {
    let fx = Fixture::new();
    let cwd = fx.root().join("claude-501/proj/sess/scratchpad/work");
    fx.write(&cwd.join("CLAUDE.md"), "refused");
    let env = fx.env().with_cwd(cwd.clone());
    let (scan, _) = scan_with(&env);
    assert_eq!(
        skip_of(&scan, &cwd.join("CLAUDE.md")),
        Skip::Invalid {
            detail: "scratchpad".into()
        }
    );
}

#[test]
fn the_disable_variable_turns_every_file_and_memory_off() {
    let fx = Fixture::new();
    fx.write(&fx.project_path("CLAUDE.md"), "x");
    let env = fx.env().with_var("CLAUDE_CODE_DISABLE_CLAUDE_MDS", "1");
    let (scan, _) = scan_with(&env);
    assert_eq!(
        skip_of(&scan, &fx.project_path("CLAUDE.md")),
        Skip::FeatureOff {
            name: "CLAUDE_CODE_DISABLE_CLAUDE_MDS".into()
        }
    );
    assert!(scan.view.session.is_empty());
    assert!(matches!(
        scan.view.memory[0].enabled,
        Enabled::Off { ref by } if by == "CLAUDE_CODE_DISABLE_CLAUDE_MDS"
    ));
}

// ---------------------------------------------------------------------------------------------
// AGENTS.md (spec 1.6)
// ---------------------------------------------------------------------------------------------

#[test]
fn agents_md_is_ignored_when_a_claude_md_exists_and_says_which() {
    let fx = Fixture::new();
    let p = fx.project();
    fx.write(&p.join("CLAUDE.md"), "c");
    fx.write(&p.join("AGENTS.md"), "a");
    let scan = scan_fx(&fx);
    assert_eq!(
        skip_of(&scan, &p.join("AGENTS.md")),
        Skip::Shadowed {
            by: p.join("CLAUDE.md"),
            how: Shadow::ClaudeMdPresent
        }
    );
    assert!(scan.diagnostics.iter().any(|d| matches!(
        &d.problem,
        Problem::AgentsMdIgnored { because } if *because == p.join("CLAUDE.md")
    )));
}

#[test]
fn the_instruction_files_mode_comes_from_user_settings() {
    let fx = Fixture::new();
    let p = fx.project();
    fx.write(&p.join("CLAUDE.md"), "c");
    fx.write(&p.join("AGENTS.md"), "a");
    fx.write(
        &fx.home_path(".claude/settings.json"),
        r#"{"pluginConfigs":{"cc-plugin-agents-md@builtin":{"options":{"instructionFiles":"claude-md-and-agents-md"}}}}"#,
    );
    let scan = scan_fx(&fx);
    assert_eq!(status_of(&scan, &p.join("AGENTS.md")), LoadStatus::Loaded);
    let order = session_files(&scan);
    let at = |name: &str| order.iter().position(|f| f.ends_with(name)).unwrap();
    assert!(at("project/CLAUDE.md") < at("project/AGENTS.md"));
}

// ---------------------------------------------------------------------------------------------
// Imports (spec 1.5)
// ---------------------------------------------------------------------------------------------

#[test]
fn imports_load_after_their_host_and_failures_are_diagnosed() {
    let fx = Fixture::new();
    let p = fx.project();
    fx.write(&p.join("CLAUDE.md"), "top\n@docs/a.md\n@docs/missing.md\n");
    fx.write(&p.join("docs/a.md"), "A\n@b.md\n");
    fx.write(&p.join("docs/b.md"), "B\n");
    let scan = scan_fx(&fx);
    let order = session_files(&scan);
    let rel: Vec<_> = order
        .iter()
        .map(|f| f.replace(fx.root().as_str(), "$"))
        .collect();
    assert_eq!(
        rel[..3],
        [
            "$/project/CLAUDE.md",
            "$/project/docs/a.md",
            "$/project/docs/b.md"
        ]
    );
    let b = load_of(&scan, &p.join("docs/b.md"));
    assert!(matches!(b[0].via, Via::Import { depth: 2, .. }));
    assert_eq!(b[0].when, Activation::WithHost);
    assert_eq!(skip_of(&scan, &p.join("docs/missing.md")), Skip::Missing);
    assert!(scan.diagnostics.iter().any(
        |d| matches!(&d.problem, Problem::ImportMissing { raw } if raw == "@docs/missing.md")
    ));
}

#[test]
fn an_import_outside_cwd_needs_approval_until_the_record_says_so() {
    let fx = Fixture::new();
    let sub = fx.project_path("app");
    fx.write(&fx.project_path("CLAUDE.md"), "@docs/x.md\n");
    fx.write(&fx.project_path("docs/x.md"), "X\n");
    fx.mkdir(&sub);
    let env = fx.env().with_cwd(sub.clone());
    let (scan, _) = scan_with(&env);
    assert_eq!(
        skip_of(&scan, &fx.project_path("docs/x.md")),
        Skip::NeedsApproval
    );
    assert!(
        scan.diagnostics
            .iter()
            .any(|d| matches!(&d.problem, Problem::ImportNeedsApproval { .. }))
    );

    let json = format!(
        r#"{{"projects":{{"{}":{{"hasClaudeMdExternalIncludesApproved":true}}}}}}"#,
        sub
    );
    fx.write(&fx.home_path(".claude.json"), &json);
    let (scan, _) = scan_with(&env);
    assert_eq!(
        status_of(&scan, &fx.project_path("docs/x.md")),
        LoadStatus::Loaded
    );
}

#[test]
fn imports_nest_to_four_hops_and_the_fifth_is_reported() {
    let fx = Fixture::new();
    let p = fx.project();
    fx.write(&p.join("CLAUDE.md"), "@d/i1.md\n");
    for n in 1..=5 {
        fx.write(
            &p.join(format!("d/i{n}.md").as_str()),
            &format!("I{n}\n@i{}.md\n", n + 1),
        );
    }
    fx.write(&p.join("d/i6.md"), "I6\n");
    let scan = scan_fx(&fx);
    for n in 1..=4 {
        assert_eq!(
            status_of(&scan, &p.join(format!("d/i{n}.md").as_str())),
            LoadStatus::Loaded
        );
    }
    assert!(matches!(
        skip_of(&scan, &p.join("d/i5.md")),
        Skip::TooLarge { .. }
    ));
    assert!(
        scan.diagnostics
            .iter()
            .any(|d| matches!(d.problem, Problem::ImportTooDeep { max: 4 }))
    );
}

// ---------------------------------------------------------------------------------------------
// Rules and on-demand loading (spec 1.3, 1.4)
// ---------------------------------------------------------------------------------------------

#[test]
fn nested_files_and_path_rules_load_on_access_with_the_on_demand_cut() {
    let fx = Fixture::new();
    let p = fx.project();
    fx.write(&p.join("sub/CLAUDE.md"), "nested");
    fx.write(&p.join("sub/big/CLAUDE.md"), &"z".repeat(50_000));
    fx.write(
        &p.join(".claude/rules/ts.md"),
        "---\npaths:\n  - \"src/**/*.ts\"\n---\nTS rule\n",
    );
    fx.write(
        &p.join("sub/.claude/rules/txt.md"),
        "---\npaths: \"*.txt\"\n---\nT\n",
    );
    fx.write(&p.join("sub/.claude/rules/plain.md"), "plain\n");
    let scan = scan_fx(&fx);

    let nested = &load_of(&scan, &p.join("sub/CLAUDE.md"))[0];
    assert_eq!(
        nested.when,
        Activation::OnAccess {
            rule: AccessRule::UnderDir { dir: p.join("sub") }
        }
    );
    assert_eq!(nested.status, LoadStatus::Loaded);

    let ts = &load_of(&scan, &p.join(".claude/rules/ts.md"))[0];
    assert_eq!(
        ts.when,
        Activation::OnAccess {
            rule: AccessRule::Glob {
                base: p.clone(),
                patterns: vec!["src/**/*.ts".into()]
            }
        }
    );
    let txt = &load_of(&scan, &p.join("sub/.claude/rules/txt.md"))[0];
    assert!(
        matches!(&txt.when, Activation::OnAccess { rule: AccessRule::Glob { base, .. } } if *base == p.join("sub"))
    );
    let plain = &load_of(&scan, &p.join("sub/.claude/rules/plain.md"))[0];
    assert_eq!(
        plain.when,
        Activation::OnAccess {
            rule: AccessRule::UnderDir { dir: p.join("sub") }
        }
    );

    // Spec 1.3: cut at max(40000, 5% of the window x chars per token).
    let big = &load_of(&scan, &p.join("sub/big/CLAUDE.md"))[0];
    match &big.status {
        LoadStatus::Truncated { limit, kept, total } => {
            assert_eq!(
                (limit.kind, limit.value, limit.unit),
                (LimitKind::OnDemandChars, 40_000, Unit::Chars)
            );
            assert_eq!((*kept, *total), (40_000, 50_000));
        }
        other => panic!("{other:?}"),
    }
    // None of them is in the session-start text.
    let session: Vec<_> = scan
        .view
        .session
        .iter()
        .flat_map(|b| b.parts.iter())
        .collect();
    assert!(session.iter().all(|part| match part {
        Part::Load { load, .. } =>
            scan.view.loads[load.0 as usize].when == Activation::SessionStart
                || scan.view.loads[load.0 as usize].when == Activation::WithHost,
        Part::Glue { .. } => true,
    }));
}

#[test]
fn on_access_answers_for_a_directory_the_walk_skipped() {
    let fx = Fixture::new();
    let p = fx.project();
    fx.write(&p.join(".gitignore"), "ignored/\n");
    fx.write(&p.join("ignored/deep/CLAUDE.md"), "hidden from the walk");
    fx.write(&p.join("ignored/CLAUDE.md"), "also hidden");
    let scan = scan_fx(&fx);
    assert!(load_of(&scan, &p.join("ignored/deep/CLAUDE.md")).is_empty());

    let env = fx.env();
    let fs = ProbeFs::new();
    let cx = ScanCx { env: &env, fs: &fs };
    let got = Claude.on_access(&cx, &p.join("ignored/deep"));
    let files: Vec<_> = got.iter().map(|c| c.file.clone()).collect();
    assert_eq!(
        files,
        [
            p.join("ignored/CLAUDE.md"),
            p.join("ignored/deep/CLAUDE.md")
        ]
    );
    assert!(
        got.iter()
            .all(|c| matches!(c.access, AccessRule::UnderDir { .. }))
    );
}

#[test]
fn a_rule_whose_frontmatter_does_not_parse_never_loads_and_is_diagnosed() {
    let fx = Fixture::new();
    let rule = fx.project_path(".claude/rules/bad.md");
    fx.write(&rule, "---\npaths: [unclosed\n---\nbody\n");
    let scan = scan_fx(&fx);
    assert_eq!(
        skip_of(&scan, &rule),
        Skip::Invalid {
            detail: "frontmatter".into()
        }
    );
    assert!(scan.diagnostics.iter().any(|d| matches!(
        &d.problem,
        Problem::FrontmatterInvalid { field } if field == "paths"
    ) && matches!(&d.at, Subject::File { path, .. } if *path == rule)));
}

#[test]
fn a_rule_with_all_star_paths_loads_at_start_and_frontmatter_is_stripped() {
    let fx = Fixture::new();
    let rule = fx.project_path(".claude/rules/star.md");
    fx.write(&rule, "---\npaths: \"**\"\n---\nSTAR\n");
    let scan = scan_fx(&fx);
    let load = &load_of(&scan, &rule)[0];
    assert_eq!(load.when, Activation::SessionStart);
    assert!(load.delivered.as_ref().unwrap().transformed);
    let fs = ProbeFs::new();
    let env = fx.env();
    let cx = ScanCx { env: &env, fs: &fs };
    let text = &Claude.render(&cx, &scan.view)[0].text;
    assert!(text.contains("STAR") && !text.contains("---"));
}

#[test]
fn nested_worktree_dirs_above_the_worktree_keep_only_their_local_file() {
    let fx = Fixture::new();
    let main = fx.root().join("main");
    let wt = main.join(".claude/worktrees/wt");
    fx.write(&main.join(".git/worktrees/wt/commondir"), "../..\n");
    fx.write(
        &wt.join(".git"),
        &format!("gitdir: {}/.git/worktrees/wt\n", main),
    );
    fx.write(&main.join("CLAUDE.md"), "main");
    fx.write(&main.join("CLAUDE.local.md"), "main local");
    fx.write(&wt.join("CLAUDE.md"), "wt");
    let env = fx.env().with_cwd(wt.clone());
    let (scan, _) = scan_with(&env);
    assert_eq!(
        skip_of(&scan, &main.join("CLAUDE.md")),
        Skip::Invalid {
            detail: "nested_worktree".into()
        }
    );
    assert_eq!(
        status_of(&scan, &main.join("CLAUDE.local.md")),
        LoadStatus::Loaded
    );
    assert_eq!(status_of(&scan, &wt.join("CLAUDE.md")), LoadStatus::Loaded);
}

// ---------------------------------------------------------------------------------------------
// Auto memory (spec 2)
// ---------------------------------------------------------------------------------------------

#[test]
fn a_worktree_shares_the_memory_directory_of_the_main_repository() {
    let fx = Fixture::new();
    let main = fx.root().join("main");
    let wt = fx.root().join("wt");
    fx.write(&main.join(".git/worktrees/wt/commondir"), "../..\n");
    fx.write(
        &wt.join(".git"),
        &format!("gitdir: {}/.git/worktrees/wt\n", main),
    );
    fx.mkdir(&wt.join("app"));
    let env = fx.env().with_cwd(wt.join("app"));
    let (scan, _) = scan_with(&env);
    let store = &scan.view.memory[0];
    let key = memory::project_key(main.as_str());
    assert_eq!(
        store.root,
        fx.home_path(".claude/projects").join(&key).join("memory")
    );
    assert_eq!(
        key,
        main.as_str()
            .replace(|c: char| !c.is_ascii_alphanumeric(), "-")
    );
}

#[test]
fn a_path_over_200_characters_gets_the_hash_suffix() {
    let fx = Fixture::new();
    let mut dir = fx.project();
    while dir.as_str().len() <= 200 {
        dir = dir.join("a-long-directory-name-for-the-key");
    }
    fx.mkdir(&dir);
    let env = fx.env().with_cwd(dir.clone());
    let (scan, _) = scan_with(&env);
    let name = scan.view.memory[0]
        .root
        .parent()
        .unwrap()
        .file_name()
        .unwrap()
        .to_owned();
    let sanitized = dir
        .as_str()
        .replace(|c: char| !c.is_ascii_alphanumeric(), "-");
    assert!(name.starts_with(&sanitized[..200]));
    assert_eq!(name.as_bytes()[200], b'-');
    // The suffix is base 36 of the absolute Java hash of the raw path.
    let mut h: i32 = 0;
    for u in dir.as_str().encode_utf16() {
        h = h.wrapping_mul(31).wrapping_add(i32::from(u));
    }
    let suffix = i64::from(h).abs();
    let mut digits = String::new();
    let mut n = suffix;
    while n > 0 {
        digits.insert(0, char::from_digit((n % 36) as u32, 36).unwrap());
        n /= 36;
    }
    assert_eq!(&name[201..], digits);
}

#[test]
fn auto_memory_directory_setting_replaces_the_default_and_first_source_wins() {
    let fx = Fixture::new();
    let custom = fx.root().join("mem-local");
    fx.write(&custom.join("MEMORY.md"), "- [A](a.md) \u{2014} a\n");
    fx.write(
        &custom.join("a.md"),
        "---\nname: a\ndescription: about a\n---\nbody\n",
    );
    fx.write(
        &fx.project_path(".claude/settings.local.json"),
        &format!(r#"{{"autoMemoryDirectory":"{custom}"}}"#),
    );
    fx.write(
        &fx.home_path(".claude/settings.json"),
        r#"{"autoMemoryDirectory":"~/user-mem"}"#,
    );
    let scan = scan_fx(&fx);
    let store = &scan.view.memory[0];
    assert_eq!(store.root, custom);
    assert_eq!(store.entries.len(), 2);
    assert_eq!(store.entries[1].indexed, Some(true));
    assert_eq!(store.entries[1].title, "a");
    assert_eq!(store.entries[1].description.as_deref(), Some("about a"));
}

#[test]
fn a_relative_auto_memory_directory_is_ignored() {
    let fx = Fixture::new();
    fx.write(
        &fx.home_path(".claude/settings.json"),
        r#"{"autoMemoryDirectory":"../elsewhere"}"#,
    );
    let scan = scan_fx(&fx);
    assert!(scan.view.memory[0].root.as_str().contains("/projects/"));
}

#[test]
fn memory_switches_follow_spec_2_7() {
    let fx = Fixture::new();
    fx.write(
        &fx.home_path(".claude/settings.json"),
        r#"{"autoMemoryEnabled":false}"#,
    );
    let off = scan_fx(&fx);
    assert!(
        matches!(&off.view.memory[0].enabled, Enabled::Off { by } if by == "autoMemoryEnabled")
    );
    // `=0` forces it back on, beating the setting.
    let (on, _) = scan_with(&fx.env().with_var("CLAUDE_CODE_DISABLE_AUTO_MEMORY", "0"));
    assert_eq!(on.view.memory[0].enabled, Enabled::On);
    let (forced_off, _) = scan_with(&fx.env().with_var("CLAUDE_CODE_DISABLE_AUTO_MEMORY", "1"));
    assert!(
        matches!(&forced_off.view.memory[0].enabled, Enabled::Off { by } if by == "CLAUDE_CODE_DISABLE_AUTO_MEMORY")
    );
}

#[test]
fn memory_index_diagnostics_carry_fixes_that_add_and_remove_one_line() {
    let fx = Fixture::new();
    let project = fx.project();
    let dir = fx
        .home_path(".claude/projects")
        .join(&memory::project_key(project.as_str()))
        .join("memory");
    let index = dir.join("MEMORY.md");
    fx.write(
        &index,
        "- [Kept](kept.md) \u{2014} k\n- [Gone](gone.md) \u{2014} g\n",
    );
    fx.write(&dir.join("kept.md"), "---\nname: kept\n---\nk\n");
    fx.write(
        &dir.join("new.md"),
        "---\nname: new-one\ndescription: a fresh note\n---\nn\n",
    );
    let scan = scan_fx(&fx);

    let not_indexed = scan
        .diagnostics
        .iter()
        .find(|d| matches!(&d.problem, Problem::MemoryNotInIndex { .. }))
        .expect("topic missing from the index");
    match not_indexed.fix.as_ref().unwrap() {
        Edit::Save { path, text, base } => {
            assert_eq!(path, &index);
            assert!(matches!(base, Base::Rev { .. }));
            assert_eq!(
                text,
                "- [Kept](kept.md) \u{2014} k\n- [Gone](gone.md) \u{2014} g\n- [new-one](new.md) \u{2014} a fresh note\n"
            );
        }
        other => panic!("{other:?}"),
    }
    let missing = scan
        .diagnostics
        .iter()
        .find(|d| matches!(&d.problem, Problem::IndexLinkMissing { target } if target == "gone.md"))
        .expect("index line whose file is missing");
    match missing.fix.as_ref().unwrap() {
        Edit::Save { text, .. } => assert_eq!(text, "- [Kept](kept.md) \u{2014} k\n"),
        other => panic!("{other:?}"),
    }
    let store = &scan.view.memory[0];
    let indexed: Vec<_> = store
        .entries
        .iter()
        .map(|e| (e.title.as_str(), e.indexed))
        .collect();
    assert_eq!(
        indexed,
        [
            ("MEMORY.md", None),
            ("kept", Some(true)),
            ("new-one", Some(false))
        ]
    );
}

#[test]
fn an_index_over_the_limit_is_truncated_and_diagnosed() {
    let fx = Fixture::new();
    let dir = fx
        .home_path(".claude/projects")
        .join(&memory::project_key(fx.project().as_str()))
        .join("memory");
    let text: String = (1..=250).map(|i| format!("- line {i}\n")).collect();
    fx.write(&dir.join("MEMORY.md"), &text);
    let scan = scan_fx(&fx);
    let load = &load_of(&scan, &dir.join("MEMORY.md"))[0];
    match &load.status {
        LoadStatus::Truncated { limit, kept, total } => {
            assert_eq!(
                (limit.kind, *kept, *total),
                (LimitKind::IndexLines, 200, 250)
            );
        }
        other => panic!("{other:?}"),
    }
    assert!(scan.diagnostics.iter().any(|d| matches!(
        &d.problem,
        Problem::IndexOverLimit { limit } if limit.kind == LimitKind::IndexLines
    )));
}

#[test]
fn memory_md_never_expands_imports() {
    let fx = Fixture::new();
    let dir = fx
        .home_path(".claude/projects")
        .join(&memory::project_key(fx.project().as_str()))
        .join("memory");
    fx.write(&dir.join("MEMORY.md"), "@note.md\n");
    fx.write(&dir.join("note.md"), "imported?\n");
    let scan = scan_fx(&fx);
    assert!(
        load_of(&scan, &dir.join("note.md"))
            .iter()
            .all(|l| !matches!(l.via, Via::Import { .. }))
    );
}

// ---------------------------------------------------------------------------------------------
// Everything else the adapter answers
// ---------------------------------------------------------------------------------------------

#[test]
fn known_projects_are_the_keys_of_the_global_state_file() {
    let fx = Fixture::new();
    fx.write(
        &fx.home_path(".claude.json"),
        r#"{"numStartups":3,"projects":{"/b/two":{},"/a/one":{"hasTrustDialogAccepted":true},"relative":{}}}"#,
    );
    let env = fx.env();
    let fs = ProbeFs::new();
    let cx = ScanCx { env: &env, fs: &fs };
    let got: Vec<String> = Claude
        .known_projects(&cx)
        .iter()
        .map(|p| p.to_string())
        .collect();
    assert_eq!(got, ["/a/one", "/b/two"]);
}

#[test]
fn trust_is_read_from_the_global_state_file() {
    let fx = Fixture::new();
    fx.write(
        &fx.home_path(".claude.json"),
        &format!(
            r#"{{"projects":{{"{}":{{"hasTrustDialogAccepted":true}}}}}}"#,
            fx.project()
        ),
    );
    let scan = scan_fx(&fx);
    assert_eq!(scan.view.trust.level, TrustLevel::Trusted);
    assert_eq!(
        scan.view.trust.recorded_in,
        Some(fx.home_path(".claude.json"))
    );
    let empty = Fixture::new();
    assert_eq!(scan_fx(&empty).view.trust.level, TrustLevel::Unknown);
}

#[test]
fn what_a_static_scan_cannot_know_is_reported_as_unknown() {
    let fx = Fixture::new();
    let scan = scan_fx(&fx);
    let unknown: Vec<_> = scan
        .view
        .assumptions
        .iter()
        .filter_map(|a| match a {
            Assumption::Unknown { what } => Some(what.0.as_str()),
            _ => None,
        })
        .collect();
    assert!(unknown.contains(&"claude.assumption.server_managed_policy"));
    assert!(unknown.contains(&"claude.assumption.mcp_instructions"));
    assert!(
        scan.view
            .assumptions
            .contains(&Assumption::ContextWindow { tokens: 200_000 })
    );
}

#[test]
fn slots_name_where_a_new_item_would_load() {
    let fx = Fixture::new();
    let scan = scan_fx(&fx);
    let place_of = |id: &str| {
        scan.view
            .slots
            .iter()
            .find(|s| s.id.0 == id)
            .unwrap_or_else(|| panic!("no slot {id}"))
            .place
            .clone()
    };
    assert_eq!(
        place_of("claude:rule:project"),
        Place::NamedFile {
            dir: fx.project_path(".claude/rules"),
            ext: "md".into()
        }
    );
    assert_eq!(
        place_of("claude:skill:user"),
        Place::NamedDir {
            dir: fx.home_path(".claude/skills"),
            file: "SKILL.md".into()
        }
    );
    assert!(matches!(
        place_of("claude:memory:auto"),
        Place::NamedFile { .. }
    ));
}

#[test]
fn the_guard_refuses_read_only_places_and_new_breakage() {
    let abs = |p: &str| AbsPath::parse(p).unwrap();
    let refused = |p: &str| {
        matches!(
            Claude.guard(&abs(p), None, Some("x")),
            Err(ApiError::ReadOnly { .. })
        )
    };
    assert!(refused("/Library/Application Support/ClaudeCode/CLAUDE.md"));
    assert!(refused("/etc/claude-code/managed-settings.json"));
    assert!(refused("/h/.claude.json"));
    assert!(refused("/h/.claude/remote-settings.json"));
    assert!(refused("/h/.claude/plugins/cache/m/p/1/hooks/hooks.json"));
    assert!(!refused("/h/.claude/CLAUDE.md"));

    let settings = abs("/p/.claude/settings.json");
    assert!(matches!(
        Claude.guard(&settings, Some("{}"), Some("{ // c\n}")),
        Err(ApiError::InvalidEdit { .. })
    ));
    assert!(
        Claude
            .guard(&settings, Some("{}"), Some("{\"a\": 1}\n"))
            .is_ok()
    );

    let rule = abs("/p/.claude/rules/r.md");
    assert!(matches!(
        Claude.guard(
            &rule,
            Some("---\npaths: a\n---\n"),
            Some("---\npaths: [x\n---\n")
        ),
        Err(ApiError::InvalidEdit { .. })
    ));
    // Already broken before: the save is not what broke it.
    assert!(
        Claude
            .guard(
                &rule,
                Some("---\npaths: [x\n---\n"),
                Some("---\npaths: [y\n---\n")
            )
            .is_ok()
    );
}

#[test]
fn render_matches_the_session_blocks_and_symlinks_show_their_target() {
    let fx = Fixture::new();
    let target = fx.project_path("shared/rule.md");
    fx.write(&target, "SHARED\n");
    fx.symlink(&fx.project_path(".claude/rules/linked.md"), &target);
    fx.write(&fx.project_path("CLAUDE.md"), "<!-- note -->\nHELLO\n");
    fx.write(&fx.project_path("CLAUDE.local.md"), "MINE\n");
    let scan = scan_fx(&fx);
    let env = fx.env();
    let fs = ProbeFs::new();
    let cx = ScanCx { env: &env, fs: &fs };
    let blocks = Claude.render(&cx, &scan.view);
    assert_eq!(blocks.len(), 1);
    let text = &blocks[0].text;
    let want = format!(
        "<system-reminder>\n{}\n\nContents of {}/CLAUDE.md (project instructions, checked into the codebase):\n\nHELLO\n\nContents of {} (project instructions, checked into the codebase):\n\nSHARED\n\nContents of {}/CLAUDE.local.md (user's private project instructions, not checked in):\n\nMINE\n</system-reminder>",
        render::INTRO,
        fx.project(),
        target,
        fx.project()
    );
    assert_eq!(text, &want);
    assert_eq!(blocks[0].tokens, crate::tokens::count(text));
    assert_eq!(scan.view.session.len(), 1);
    assert_eq!(scan.view.budget.session_start, blocks[0].tokens);
    assert_eq!(scan.view.session[0].tokens, blocks[0].tokens);
}

#[test]
fn every_rule_ref_in_the_source_has_an_entry_in_both_languages() {
    use std::collections::BTreeSet;
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/agents/claude");
    let mut used = BTreeSet::new();
    let mut stack = vec![dir];
    while let Some(d) = stack.pop() {
        for entry in std::fs::read_dir(&d).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "rs") {
                let text = std::fs::read_to_string(&path).unwrap();
                for piece in text.split('"') {
                    let ok = piece.starts_with("claude.")
                        && piece.len() > 8
                        && piece
                            .bytes()
                            .all(|b| b.is_ascii_lowercase() || b == b'_' || b == b'.');
                    if ok {
                        used.insert(piece.to_owned());
                    }
                }
            }
        }
    }
    // A memory store id, not a rule reference.
    used.remove("claude.auto");
    let have: BTreeSet<_> = Claude.rules().iter().map(|r| r.id.0.clone()).collect();
    let missing: Vec<_> = used.difference(&have).collect();
    assert!(missing.is_empty(), "no rules.toml entry for {missing:?}");
    for rule in Claude.rules() {
        assert!(
            !rule.en.trim().is_empty() && !rule.ko.trim().is_empty(),
            "{}",
            rule.id.0
        );
    }
}
