use super::*;

fn project_files_status(scan: &AgentScan, fx: &Fixture) -> LoadStatus {
    only(scan, &fx.project_path("GEMINI.md")).status.clone()
}

fn setup(fx: &Fixture) {
    fx.write(&fx.project_path("GEMINI.md"), "project rules");
    fx.write(&fx.home_path(".gemini/GEMINI.md"), "global rules");
    fx.write(
        &fx.project_path(".gemini/settings.json"),
        r#"{"hooks":{"BeforeTool":[{"hooks":[{"type":"command","command":"echo w"}]}]}}"#,
    );
    fx.write(
        &fx.project_path(".gemini/commands/p.toml"),
        "prompt = \"x\"",
    );
}

#[test]
fn untrusted_folder_matrix_no_rule_means_untrusted_and_skips_project_sources() {
    // Spec 1, 2, 5, 6: folder trust is on by default, and no rule means the CLI asks and
    // behaves as untrusted until the user answers.
    let fx = Fixture::new();
    setup(&fx);
    let scan = scan(&fx.env());
    assert_eq!(scan.view.trust.level, TrustLevel::Untrusted);
    assert_eq!(scan.view.trust.recorded_in, None);
    assert_eq!(
        *skip(only(&scan, &fx.project_path("GEMINI.md"))),
        Skip::Untrusted
    );
    // Global memory loads regardless of trust.
    assert_eq!(
        only(&scan, &fx.home_path(".gemini/GEMINI.md")).status,
        LoadStatus::Loaded
    );
    // The workspace hook is listed and will not run.
    let hook = &scan.view.hooks[0];
    assert_eq!(
        hook.state,
        HookState::WillNotRun {
            why: HookBlock::FolderUntrusted
        }
    );
    // The project command is hidden.
    let command = scan
        .view
        .extensions
        .iter()
        .find(|e| e.ext_kind == ExtKind::Command)
        .unwrap();
    assert!(matches!(command.status, ExtStatus::Hidden { .. }));
    assert!(problems(&scan)
        .iter()
        .any(|p| matches!(p, Problem::ProjectUntrusted { skipped } if skipped.iter().any(|r| r.0 == "gemini.context.project"))));
}

#[test]
fn untrusted_folder_matrix_user_hooks_do_not_run_either() {
    // Spec 5, Trust: the whole merged hooks object is gated, user and system hooks included.
    let fx = Fixture::new();
    user_settings(
        &fx,
        r#"{"hooks":{"SessionStart":[{"hooks":[{"type":"command","command":"echo u"}]}]}}"#,
    );
    let scan = scan(&fx.env());
    assert_eq!(
        scan.view.hooks[0].state,
        HookState::WillNotRun {
            why: HookBlock::FolderUntrusted
        }
    );
}

#[test]
fn untrusted_folder_matrix_a_trust_folder_rule_trusts_the_folder() {
    let fx = Fixture::new();
    setup(&fx);
    fx.write(
        &fx.home_path(".gemini/trustedFolders.json"),
        &format!(
            "// comments are allowed\n{{\"{}\": \"TRUST_FOLDER\"}}",
            fx.project()
        ),
    );
    let scan = scan(&fx.env());
    assert_eq!(scan.view.trust.level, TrustLevel::Trusted);
    assert_eq!(
        scan.view.trust.recorded_in,
        Some(fx.home_path(".gemini/trustedFolders.json"))
    );
    assert_eq!(project_files_status(&scan, &fx), LoadStatus::Loaded);
    assert_eq!(scan.view.hooks[0].state, HookState::Active);
}

#[test]
fn untrusted_folder_matrix_trust_parent_covers_siblings() {
    let fx = Fixture::new();
    setup(&fx);
    fx.write(
        &fx.home_path(".gemini/trustedFolders.json"),
        &format!("{{\"{}\": \"TRUST_PARENT\"}}", fx.project_path("elsewhere")),
    );
    assert_eq!(scan(&fx.env()).view.trust.level, TrustLevel::Trusted);
}

#[test]
fn untrusted_folder_matrix_the_longest_rule_wins() {
    let fx = Fixture::new();
    setup(&fx);
    fx.write(
        &fx.home_path(".gemini/trustedFolders.json"),
        &format!(
            "{{\"{}\": \"TRUST_FOLDER\", \"{}\": \"DO_NOT_TRUST\"}}",
            fx.root(),
            fx.project()
        ),
    );
    let scan = scan(&fx.env());
    assert_eq!(scan.view.trust.level, TrustLevel::Untrusted);
    assert_eq!(
        scan.view.trust.recorded_in,
        Some(fx.home_path(".gemini/trustedFolders.json"))
    );
}

#[test]
fn untrusted_folder_matrix_environment_variables_beat_the_file() {
    let fx = Fixture::new();
    setup(&fx);
    fx.write(
        &fx.home_path(".gemini/trustedFolders.json"),
        &format!("{{\"{}\": \"TRUST_FOLDER\"}}", fx.project()),
    );
    let off = scan(&fx.env().with_var("GEMINI_CLI_TRUST_WORKSPACE", "false"));
    assert_eq!(off.view.trust.level, TrustLevel::Untrusted);
    let restricted = scan(&fx.env().with_var("GEMINI_RESTRICTED_MODE", "true"));
    assert_eq!(restricted.view.trust.level, TrustLevel::Untrusted);
    let on = scan(&fx.env().with_var("GEMINI_CLI_TRUST_WORKSPACE", "true"));
    assert_eq!(on.view.trust.level, TrustLevel::Trusted);
    assert_eq!(on.view.trust.recorded_in, None);
}

#[test]
fn untrusted_folder_matrix_turning_the_feature_off_trusts_everything() {
    let fx = Fixture::new();
    setup(&fx);
    user_settings(&fx, r#"{"security":{"folderTrust":{"enabled":false}}}"#);
    assert_eq!(scan(&fx.env()).view.trust.level, TrustLevel::Trusted);
}

#[test]
fn untrusted_folder_matrix_the_workspace_file_cannot_turn_trust_off_for_itself() {
    // Spec 2: the trust check reads only system defaults, user and system settings.
    let fx = Fixture::new();
    fx.write(
        &fx.project_path(".gemini/settings.json"),
        r#"{"security":{"folderTrust":{"enabled":false}}}"#,
    );
    assert_eq!(scan(&fx.env()).view.trust.level, TrustLevel::Untrusted);
}

#[test]
fn a_trusted_folders_file_that_does_not_parse_is_fatal() {
    let fx = Fixture::new();
    fx.write(&fx.home_path(".gemini/trustedFolders.json"), "{not json");
    let scan = scan(&fx.env());
    assert!(scan.view.loads.is_empty());
    assert!(problems(&scan).iter().any(|p| matches!(
        p,
        Problem::ParseError {
            fatal_for_agent: true,
            ..
        }
    )));
}

#[test]
fn private_memory_loads_in_an_untrusted_folder() {
    // Spec 4: loaded regardless of folder trust.
    let fx = Fixture::new();
    fx.write(
        &fx.home_path(".gemini/projects.json"),
        &format!("{{\"projects\":{{\"{}\":\"proj\"}}}}", fx.project()),
    );
    fx.write(
        &fx.home_path(".gemini/tmp/proj/memory/MEMORY.md"),
        "- [a](a.md)",
    );
    let scan = scan(&fx.env());
    assert_eq!(scan.view.trust.level, TrustLevel::Untrusted);
    assert_eq!(
        only(&scan, &fx.home_path(".gemini/tmp/proj/memory/MEMORY.md")).status,
        LoadStatus::Loaded
    );
}

#[test]
fn extension_hooks_run_in_an_untrusted_folder() {
    let fx = Fixture::new();
    let ext = fx.home_path(".gemini/extensions/e");
    fx.write(
        &ext.join("gemini-extension.json"),
        r#"{"name":"e","version":"1"}"#,
    );
    fx.write(
        &ext.join("hooks/hooks.json"),
        r#"{"hooks":{"SessionStart":[{"hooks":[{"type":"command","command":"${extensionPath}/x.sh"}]}]}}"#,
    );
    let scan = scan(&fx.env());
    assert_eq!(scan.view.hooks.len(), 1);
    assert_eq!(scan.view.hooks[0].state, HookState::Active);
    assert_eq!(
        scan.view.hooks[0].source.scope,
        Scope::Plugin { id: "e".to_owned() }
    );
}
