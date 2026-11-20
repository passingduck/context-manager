use super::*;

#[test]
fn project_tier_is_sorted_by_utf16_code_units_not_root_to_leaf() {
    // Spec 1, concatenation order: `/r/B/GEMINI.md` sorts before `/r/GEMINI.md`.
    let fx = Fixture::new();
    let root = fx.project();
    git(&fx, &root);
    fx.write(&root.join("GEMINI.md"), "root");
    fx.write(&root.join("B/GEMINI.md"), "leaf");
    let env = trusted(&fx).with_cwd(root.join("B"));
    let scan = scan(&env);
    assert_eq!(
        discovered(&scan, "gemini.context.project"),
        [
            root.join("B/GEMINI.md").as_str().to_owned(),
            root.join("GEMINI.md").as_str().to_owned()
        ]
    );
}

#[test]
fn agents_md_sorts_before_gemini_md_in_one_directory() {
    let fx = Fixture::new();
    user_settings(&fx, r#"{"context":{"fileName":"AGENTS.md"}}"#);
    fx.write(&fx.project_path("GEMINI.md"), "g");
    fx.write(&fx.project_path("AGENTS.md"), "a");
    let scan = scan(&trusted(&fx));
    assert_eq!(
        discovered(&scan, "gemini.context.project"),
        [
            fx.project_path("AGENTS.md").as_str().to_owned(),
            fx.project_path("GEMINI.md").as_str().to_owned()
        ]
    );
}

#[test]
fn context_file_name_prepends_and_keeps_gemini_md() {
    // Spec 2: "AGENTS.md" gives ["AGENTS.md","GEMINI.md"]; a list keeps its order.
    let fx = Fixture::new();
    for (setting, expected) in [
        (r#""AGENTS.md""#, "AGENTS.md,GEMINI.md"),
        (
            r#"["AGENTS.md","CONTEXT.md"]"#,
            "AGENTS.md,CONTEXT.md,GEMINI.md",
        ),
        (r#"["  ", "GEMINI.md", "X.md"]"#, "GEMINI.md,X.md"),
    ] {
        user_settings(&fx, &format!(r#"{{"context":{{"fileName":{setting}}}}}"#));
        let scan = scan(&trusted(&fx));
        let mode = scan
            .view
            .assumptions
            .iter()
            .find_map(|a| match a {
                Assumption::Mode { name, value, .. } if name == "context.fileName" => {
                    Some(value.clone())
                }
                _ => None,
            })
            .expect("a Mode assumption");
        assert_eq!(mode, expected);
    }
}

#[test]
fn global_file_slot_uses_the_first_configured_name() {
    let fx = Fixture::new();
    user_settings(&fx, r#"{"context":{"fileName":["AGENTS.md"]}}"#);
    let scan = scan(&fx.env());
    let slot = scan
        .view
        .slots
        .iter()
        .find(|s| s.id.0 == "gemini:instructions:user")
        .unwrap();
    assert_eq!(
        slot.place,
        Place::File {
            path: fx.home_path(".gemini/AGENTS.md")
        }
    );
}

#[test]
fn context_file_name_may_hold_a_subdirectory() {
    let fx = Fixture::new();
    user_settings(&fx, r#"{"context":{"fileName":"docs/AGENTS.md"}}"#);
    fx.write(&fx.project_path("docs/AGENTS.md"), "x");
    let scan = scan(&trusted(&fx));
    assert_eq!(
        discovered(&scan, "gemini.context.project"),
        [fx.project_path("docs/AGENTS.md").as_str().to_owned()]
    );
}

#[test]
fn boundary_ceiling_stops_at_git_and_without_git_only_the_working_directory_loads() {
    // Spec 1, project row.
    let fx = Fixture::new();
    let root = fx.project();
    fx.write(&root.join("GEMINI.md"), "root");
    fx.write(&root.join("pkg/sub/GEMINI.md"), "sub");
    fx.write(&fx.root().join("GEMINI.md"), "above the repo");
    let env = trusted(&fx).with_cwd(root.join("pkg/sub"));

    // No `.git` anywhere: only the working directory.
    let without = scan(&env);
    assert_eq!(
        discovered(&without, "gemini.context.project"),
        [root.join("pkg/sub/GEMINI.md").as_str().to_owned()]
    );

    // With `.git` at the root: the root and the working directory, not what is above.
    git(&fx, &root);
    let with = scan(&env);
    assert_eq!(
        discovered(&with, "gemini.context.project"),
        [
            root.join("GEMINI.md").as_str().to_owned(),
            root.join("pkg/sub/GEMINI.md").as_str().to_owned()
        ]
    );
}

#[test]
fn a_boundary_marker_may_be_a_file_and_empty_markers_check_only_the_working_directory() {
    let fx = Fixture::new();
    let root = fx.project();
    fx.write(&root.join("GEMINI.md"), "root");
    fx.write(&root.join("a/GEMINI.md"), "a");
    fx.write(&root.join(".git"), "gitdir: elsewhere"); // a worktree has a file
    let env = trusted(&fx).with_cwd(root.join("a"));
    assert_eq!(discovered(&scan(&env), "gemini.context.project").len(), 2);
    user_settings(&fx, r#"{"context":{"memoryBoundaryMarkers":[]}}"#);
    assert_eq!(discovered(&scan(&env), "gemini.context.project").len(), 1);
    user_settings(
        &fx,
        r#"{"context":{"memoryBoundaryMarkers":["pnpm-lock.yaml"]}}"#,
    );
    fx.write(&root.join("pnpm-lock.yaml"), "");
    assert_eq!(discovered(&scan(&env), "gemini.context.project").len(), 2);
}

#[test]
fn the_walk_never_enters_the_global_gemini_directory() {
    // Spec 1: it stops if it reaches `~/.gemini`.
    let fx = Fixture::new();
    fx.write(&fx.home_path(".gemini/GEMINI.md"), "global");
    let env = trusted(&fx).with_cwd(fx.home_path(".gemini"));
    let scan = scan(&env);
    assert!(discovered(&scan, "gemini.context.project").is_empty());
    assert_eq!(discovered(&scan, "gemini.context.global").len(), 1);
}

#[test]
fn a_gitignored_gemini_md_still_loads() {
    // Spec 1: ignore files do not apply to context files.
    let fx = Fixture::new();
    git(&fx, &fx.project());
    fx.write(&fx.project_path(".gitignore"), "GEMINI.md\nsub/\n");
    fx.write(&fx.project_path(".geminiignore"), "GEMINI.md\n");
    fx.write(&fx.project_path("GEMINI.md"), "ignored but loaded");
    let scan = scan(&trusted(&fx));
    assert_eq!(discovered(&scan, "gemini.context.project").len(), 1);
}

#[test]
fn on_access_finds_a_gemini_md_in_a_directory_the_walk_skipped() {
    let fx = Fixture::new();
    git(&fx, &fx.project());
    fx.write(&fx.project_path(".gitignore"), "build/\n");
    fx.write(&fx.project_path("build/out/GEMINI.md"), "generated rules");
    fx.write(&fx.project_path("build/GEMINI.md"), "build rules");
    let env = trusted(&fx);
    let found = on_access(&env, &fx.project_path("build/out"));
    let files: Vec<_> = found.iter().map(|c| c.file.as_str().to_owned()).collect();
    assert_eq!(
        files,
        [
            fx.project_path("build/GEMINI.md").as_str().to_owned(),
            fx.project_path("build/out/GEMINI.md").as_str().to_owned()
        ]
    );
    assert_eq!(
        found[1].access,
        AccessRule::UnderDir {
            dir: fx.project_path("build/out")
        }
    );
}

#[test]
fn just_in_time_files_below_the_working_directory_are_on_access_loads() {
    let fx = Fixture::new();
    fx.write(&fx.project_path("GEMINI.md"), "root");
    fx.write(&fx.project_path("src/GEMINI.md"), "src rules");
    let scan = scan(&trusted(&fx));
    let jit = only(&scan, &fx.project_path("src/GEMINI.md"));
    assert_eq!(
        jit.when,
        Activation::OnAccess {
            rule: AccessRule::UnderDir {
                dir: fx.project_path("src")
            }
        }
    );
    assert_eq!(jit.status, LoadStatus::Loaded);
    let mut counted = scan.view.clone();
    crate::passes::budget(&mut counted);
    assert!(counted.budget.on_access_max > 0);
    // A directory above the working directory is not an access trigger target.
    assert!(on_access(&trusted(&fx), &fx.root()).is_empty());
}

#[test]
fn on_access_skips_files_the_session_start_already_loaded() {
    let fx = Fixture::new();
    git(&fx, &fx.project());
    fx.write(&fx.project_path("GEMINI.md"), "root");
    assert!(on_access(&trusted(&fx), &fx.project_path("a/b")).is_empty());
}

#[test]
fn an_empty_file_is_loaded_as_empty_and_makes_no_block() {
    let fx = Fixture::new();
    fx.write(&fx.project_path("GEMINI.md"), "  \n\t\n");
    let scan = scan(&trusted(&fx));
    assert_eq!(
        *skip(only(&scan, &fx.project_path("GEMINI.md"))),
        Skip::Empty
    );
    assert!(scan.view.session.is_empty());
}

#[test]
fn a_symlink_to_the_same_file_loads_once() {
    // Spec 1: dedupe is by file identity.
    let fx = Fixture::new();
    user_settings(&fx, r#"{"context":{"fileName":"AGENTS.md"}}"#);
    fx.write(&fx.project_path("GEMINI.md"), "text");
    fx.symlink(&fx.project_path("AGENTS.md"), &fx.project_path("GEMINI.md"));
    let scan = scan(&trusted(&fx));
    let loads: Vec<_> = scan
        .view
        .loads
        .iter()
        .filter(|l| matches!(l.scope, Scope::Project { .. }))
        .collect();
    assert_eq!(loads.len(), 2);
    assert_eq!(
        loads
            .iter()
            .filter(|l| l.status == LoadStatus::Loaded)
            .count(),
        1
    );
    assert!(loads.iter().any(|l| matches!(
        l.status,
        LoadStatus::Skipped {
            skip: Skip::AlreadyLoaded { .. }
        }
    )));
}

#[test]
fn include_directories_each_get_their_own_upward_walk() {
    let fx = Fixture::new();
    let other = fx.root().join("other");
    git(&fx, &other);
    fx.write(&other.join("GEMINI.md"), "other root");
    fx.write(&other.join("pkg/GEMINI.md"), "other pkg");
    fx.write(&fx.project_path("GEMINI.md"), "mine");
    user_settings(
        &fx,
        &format!(
            r#"{{"context":{{"includeDirectories":["{}"]}}}}"#,
            other.join("pkg")
        ),
    );
    let scan = scan(&trusted(&fx));
    let files = discovered(&scan, "gemini.context.project");
    assert_eq!(files.len(), 3);
    assert!(files.contains(&other.join("GEMINI.md").as_str().to_owned()));
    assert!(files.contains(&other.join("pkg/GEMINI.md").as_str().to_owned()));
}

#[test]
fn a_missing_include_directory_is_skipped() {
    let fx = Fixture::new();
    user_settings(
        &fx,
        r#"{"context":{"includeDirectories":["/nonexistent/dir"]}}"#,
    );
    let scan = scan(&trusted(&fx));
    assert!(scan.view.assumptions.iter().all(
        |a| !matches!(a, Assumption::Mode { name, .. } if name == "context.includeDirectories")
    ));
}

// ---- Imports (spec 3) ------------------------------------------------------------------------

#[test]
fn an_import_is_a_load_under_its_host() {
    let fx = Fixture::new();
    fx.write(&fx.project_path("GEMINI.md"), "see\n@./rules.md\n");
    fx.write(&fx.project_path("rules.md"), "be kind");
    let scan = scan(&trusted(&fx));
    let host = only(&scan, &fx.project_path("GEMINI.md"));
    let import = only(&scan, &fx.project_path("rules.md"));
    assert_eq!(import.when, Activation::WithHost);
    assert_eq!(
        import.via,
        Via::Import {
            host: host.id.clone(),
            line: 2,
            depth: 1
        }
    );
    assert_eq!(import.status, LoadStatus::Loaded);
}

#[test]
fn a_failed_import_is_a_load_with_a_skip_reason_and_a_diagnostic() {
    let fx = Fixture::new();
    fx.write(&fx.project_path("GEMINI.md"), "@./gone.md\n");
    let scan = scan(&trusted(&fx));
    let load = only(&scan, &fx.project_path("gone.md"));
    assert_eq!(*skip(load), Skip::Missing);
    assert!(
        problems(&scan)
            .iter()
            .any(|p| matches!(p, Problem::ImportMissing { raw } if raw == "@./gone.md"))
    );
}

#[test]
fn a_new_at_word_after_whitespace_is_an_accidental_import() {
    // Spec 3: `@someone said` becomes an import attempt.
    let fx = Fixture::new();
    fx.write(&fx.project_path("GEMINI.md"), "ask @someone said hi\n");
    let scan = scan(&trusted(&fx));
    assert_eq!(
        *skip(only(&scan, &fx.project_path("someone"))),
        Skip::Missing
    );
    assert!(
        problems(&scan)
            .iter()
            .any(|p| matches!(p, Problem::AccidentalImport { raw } if raw == "@someone"))
    );
}

#[test]
fn tokens_that_are_not_imports_stay_text() {
    // Spec 3: an email, `@~/x.md`, `@1.md`, and a token inside backticks.
    let fx = Fixture::new();
    fx.write(
        &fx.project_path("GEMINI.md"),
        "mail a@b.md\n@~/x.md\n@1.md\nuse `@./a.md` here\n```\n@./b.md\n```\n",
    );
    let scan = scan(&trusted(&fx));
    assert_eq!(find(&scan, &fx.project_path("GEMINI.md")).len(), 1);
    assert_eq!(scan.view.loads.len(), 1, "{:?}", scan.view.loads);
}

#[test]
fn an_import_outside_the_allowed_root_is_refused_without_git_and_allowed_with_git() {
    // Spec 3, allowed root: the file's nearest git root, else its own directory.
    let fx = Fixture::new();
    fx.write(&fx.project_path("sub/GEMINI.md"), "@../shared.md\n");
    fx.write(&fx.project_path("shared.md"), "shared");
    let env = trusted(&fx).with_cwd(fx.project_path("sub"));

    let refused = scan(&env);
    let load = only(&refused, &fx.project_path("shared.md"));
    assert_eq!(
        *skip(load),
        Skip::Invalid {
            why: Invalid::OutsideRoot
        }
    );
    assert!(
        problems(&refused)
            .iter()
            .any(|p| matches!(p, Problem::ImportOutsideRoot { .. }))
    );

    git(&fx, &fx.project());
    let allowed = scan(&env);
    assert_eq!(
        only(&allowed, &fx.project_path("shared.md")).status,
        LoadStatus::Loaded
    );
}

#[test]
fn the_global_file_may_import_only_from_the_global_directory() {
    let fx = Fixture::new();
    fx.write(
        &fx.home_path(".gemini/GEMINI.md"),
        "@./inside.md\n@../outside.md\n",
    );
    fx.write(&fx.home_path(".gemini/inside.md"), "in");
    fx.write(&fx.home_path("outside.md"), "out");
    let scan = scan(&fx.env());
    assert_eq!(
        only(&scan, &fx.home_path(".gemini/inside.md")).status,
        LoadStatus::Loaded
    );
    assert!(matches!(
        skip(only(&scan, &fx.home_path("outside.md"))),
        Skip::Invalid { .. }
    ));
}

#[test]
fn imports_stop_at_depth_five_and_leave_the_deeper_token_alone() {
    // Spec 3: the root is depth 0; a file at depth 5 is inlined with its own tokens as text.
    let fx = Fixture::new();
    fx.write(&fx.project_path("GEMINI.md"), "@./d1.md");
    for depth in 1..=5 {
        fx.write(
            &fx.project_path(&format!("d{depth}.md")),
            &format!("text {depth}\n@./d{}.md", depth + 1),
        );
    }
    fx.write(&fx.project_path("d6.md"), "never");
    let scan = scan(&trusted(&fx));
    for depth in 1..=5 {
        assert_eq!(
            only(&scan, &fx.project_path(&format!("d{depth}.md"))).status,
            LoadStatus::Loaded
        );
    }
    assert!(find(&scan, &fx.project_path("d6.md")).is_empty());
}

#[test]
fn a_cycle_inlines_the_root_once_more_and_stops_the_next_hop() {
    // Spec 3: A to B to A inlines A at depth 2, and only then does B stop.
    let fx = Fixture::new();
    fx.write(&fx.project_path("GEMINI.md"), "root\n@./b.md");
    fx.write(&fx.project_path("b.md"), "b\n@./GEMINI.md");
    let scan = scan(&trusted(&fx));
    let again = find(&scan, &fx.project_path("GEMINI.md"));
    assert_eq!(again.len(), 2, "the root file appears again as an import");
    assert!(
        problems(&scan)
            .iter()
            .any(|p| matches!(p, Problem::ImportCycle { .. }))
    );
}

#[test]
fn the_same_file_imported_from_two_sibling_branches_loads_twice() {
    let fx = Fixture::new();
    fx.write(&fx.project_path("GEMINI.md"), "@./a.md\n@./b.md\n");
    fx.write(&fx.project_path("a.md"), "@./shared.md");
    fx.write(&fx.project_path("b.md"), "@./shared.md");
    fx.write(&fx.project_path("shared.md"), "s");
    let scan = scan(&trusted(&fx));
    let loads = find(&scan, &fx.project_path("shared.md"));
    assert_eq!(loads.len(), 2);
    assert!(loads.iter().all(|l| l.status == LoadStatus::Loaded));
}

#[test]
fn imports_accept_any_file_type() {
    let fx = Fixture::new();
    fx.write(
        &fx.project_path("GEMINI.md"),
        "@./data.bin.txt\n@./Makefile\n",
    );
    fx.write(&fx.project_path("data.bin.txt"), "x");
    fx.write(&fx.project_path("Makefile"), "all:");
    let scan = scan(&trusted(&fx));
    assert_eq!(
        only(&scan, &fx.project_path("data.bin.txt")).status,
        LoadStatus::Loaded
    );
    assert_eq!(
        only(&scan, &fx.project_path("Makefile")).status,
        LoadStatus::Loaded
    );
}
