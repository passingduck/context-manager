use super::*;

fn ext_of<'a>(scan: &'a AgentScan, kind: ExtKind, name: &str) -> Vec<&'a Extension> {
    scan.view
        .extensions
        .iter()
        .filter(|e| e.ext_kind == kind && e.name == name)
        .collect()
}

fn skill(fx: &Fixture, dir: &AbsPath, name: &str, description: &str) {
    fx.write(
        &dir.join(format!("{name}/SKILL.md").as_str()),
        &format!("---\nname: {name}\ndescription: {description}\n---\nBody of {name}.\n"),
    );
}

#[test]
fn command_naming_rule_turns_separators_into_colons() {
    // Spec 6: `gcs/sync.toml` is `/gcs:sync`.
    let fx = Fixture::new();
    fx.write(
        &fx.home_path(".gemini/commands/gcs/sync.toml"),
        "prompt = \"sync\"\ndescription = \"d\"",
    );
    fx.write(
        &fx.home_path(".gemini/commands/odd name.toml"),
        "prompt = \"x\"",
    );
    let scan = scan(&fx.env());
    let names: Vec<_> = scan
        .view
        .extensions
        .iter()
        .filter(|e| e.ext_kind == ExtKind::Command)
        .map(|e| e.name.as_str())
        .collect();
    assert_eq!(names, ["gcs:sync", "odd_name"]);
    let sync = ext_of(&scan, ExtKind::Command, "gcs:sync")[0];
    assert_eq!(sync.description.as_deref(), Some("d"));
    let body = scan.view.loads.iter().find(|l| l.id == sync.body).unwrap();
    assert_eq!(body.kind, DocKind::CommandBody);
    assert_eq!(body.when, Activation::OnInvoke);
}

#[test]
fn a_project_command_replaces_a_user_command_with_the_same_name() {
    let fx = Fixture::new();
    fx.write(
        &fx.home_path(".gemini/commands/deploy.toml"),
        "prompt = \"user\"",
    );
    fx.write(
        &fx.project_path(".gemini/commands/deploy.toml"),
        "prompt = \"project\"",
    );
    let scan = scan(&trusted(&fx));
    let both = ext_of(&scan, ExtKind::Command, "deploy");
    assert_eq!(both.len(), 2);
    assert_eq!(both[0].scope, Scope::User);
    assert!(matches!(both[0].status, ExtStatus::Shadowed { .. }));
    assert_eq!(both[1].status, ExtStatus::Active);
}

#[test]
fn a_conflicting_extension_command_is_renamed_with_the_extension_name() {
    let fx = Fixture::new();
    fx.write(
        &fx.home_path(".gemini/commands/deploy.toml"),
        "prompt = \"user\"",
    );
    let ext = fx.home_path(".gemini/extensions/tools");
    fx.write(
        &ext.join("gemini-extension.json"),
        r#"{"name":"tools","version":"1"}"#,
    );
    fx.write(&ext.join("commands/deploy.toml"), "prompt = \"ext\"");
    let scan = scan(&trusted(&fx));
    assert_eq!(ext_of(&scan, ExtKind::Command, "tools.deploy").len(), 1);
    assert_eq!(ext_of(&scan, ExtKind::Command, "deploy").len(), 1);
}

#[test]
fn a_command_without_a_prompt_is_rejected() {
    let fx = Fixture::new();
    fx.write(
        &fx.home_path(".gemini/commands/bad.toml"),
        "description = \"x\"",
    );
    let scan = scan(&fx.env());
    assert!(matches!(
        ext_of(&scan, ExtKind::Command, "bad")[0].status,
        ExtStatus::Rejected { .. }
    ));
}

#[test]
fn skill_precedence_later_roots_win_a_name_clash() {
    // Spec 6: extension < ~/.gemini/skills < ~/.agents/skills < project .gemini < project .agents.
    let fx = Fixture::new();
    let ext = fx.home_path(".gemini/extensions/e");
    fx.write(
        &ext.join("gemini-extension.json"),
        r#"{"name":"e","version":"1"}"#,
    );
    skill(&fx, &ext.join("skills"), "pdf", "from extension");
    skill(
        &fx,
        &fx.home_path(".gemini/skills"),
        "pdf",
        "from user gemini",
    );
    skill(
        &fx,
        &fx.home_path(".agents/skills"),
        "pdf",
        "from user agents",
    );
    skill(
        &fx,
        &fx.project_path(".gemini/skills"),
        "pdf",
        "from project gemini",
    );
    skill(
        &fx,
        &fx.project_path(".agents/skills"),
        "pdf",
        "from project agents",
    );
    skill(&fx, &fx.home_path(".gemini/skills"), "solo", "only one");
    let scan = scan(&trusted(&fx));
    let pdfs = ext_of(&scan, ExtKind::Skill, "pdf");
    assert_eq!(pdfs.len(), 5);
    let described: Vec<_> = pdfs
        .iter()
        .map(|e| {
            (
                e.description.clone().unwrap(),
                matches!(e.status, ExtStatus::Active),
            )
        })
        .collect();
    assert_eq!(
        described,
        [
            ("from extension".to_owned(), false),
            ("from user gemini".to_owned(), false),
            ("from user agents".to_owned(), false),
            ("from project gemini".to_owned(), false),
            ("from project agents".to_owned(), true),
        ]
    );
    assert!(matches!(pdfs[0].status, ExtStatus::Shadowed { .. }));
    // Only the winner reaches the catalog.
    let blocks = render(&trusted(&fx));
    let catalog = blocks
        .iter()
        .find(|b| b.label == BlockLabel::SkillCatalog)
        .unwrap();
    assert!(catalog.text().contains("from project agents"));
    assert!(!catalog.text().contains("from user agents"));
    assert!(catalog.text().contains("only one"));
}

#[test]
fn a_skill_has_a_listing_at_session_start_and_a_body_on_invoke() {
    let fx = Fixture::new();
    skill(&fx, &fx.home_path(".gemini/skills"), "pdf", "Read PDFs");
    let scan = scan(&fx.env());
    let item = ext_of(&scan, ExtKind::Skill, "pdf")[0];
    let listing = scan
        .view
        .loads
        .iter()
        .find(|l| Some(&l.id) == item.listing.as_ref())
        .unwrap();
    assert_eq!(listing.when, Activation::SessionStart);
    assert_eq!(listing.kind, DocKind::Listing);
    let body = scan.view.loads.iter().find(|l| l.id == item.body).unwrap();
    assert_eq!(
        (body.kind, body.when.clone()),
        (DocKind::SkillBody, Activation::OnInvoke)
    );
    assert!(
        scan.view
            .session
            .iter()
            .any(|b| b.label == BlockLabel::SkillCatalog)
    );
}

#[test]
fn project_skills_need_trust_and_skills_disabled_matches_case_insensitively() {
    let fx = Fixture::new();
    skill(&fx, &fx.project_path(".gemini/skills"), "proj", "p");
    skill(&fx, &fx.home_path(".gemini/skills"), "Home", "h");
    user_settings(&fx, r#"{"skills":{"disabled":["home"]}}"#);
    let untrusted = scan(&fx.env());
    assert!(matches!(
        ext_of(&untrusted, ExtKind::Skill, "proj")[0].status,
        ExtStatus::Hidden { .. }
    ));
    assert_eq!(
        ext_of(&untrusted, ExtKind::Skill, "Home")[0].status,
        ExtStatus::Disabled {
            by: "skills.disabled".to_owned()
        }
    );
    let on = scan(&trusted(&fx));
    assert_eq!(
        ext_of(&on, ExtKind::Skill, "proj")[0].status,
        ExtStatus::Active
    );
}

#[test]
fn skills_enabled_false_disables_every_skill() {
    let fx = Fixture::new();
    skill(&fx, &fx.home_path(".gemini/skills"), "a", "a");
    user_settings(&fx, r#"{"skills":{"enabled":false}}"#);
    let scan = scan(&fx.env());
    assert!(matches!(
        ext_of(&scan, ExtKind::Skill, "a")[0].status,
        ExtStatus::Disabled { .. }
    ));
    assert!(
        !scan
            .view
            .session
            .iter()
            .any(|b| b.label == BlockLabel::SkillCatalog)
    );
}

#[test]
fn a_skill_name_is_sanitized_and_a_simple_frontmatter_fallback_reads_colons() {
    let fx = Fixture::new();
    fx.write(
        &fx.home_path(".gemini/skills/x/SKILL.md"),
        "---\nname: my:skill\ndescription: Does a: b and c\n---\nbody",
    );
    let scan = scan(&fx.env());
    let item = ext_of(&scan, ExtKind::Skill, "my-skill");
    assert_eq!(item.len(), 1);
    assert_eq!(item[0].description.as_deref(), Some("Does a: b and c"));
}

#[test]
fn a_skill_without_frontmatter_is_rejected() {
    let fx = Fixture::new();
    fx.write(&fx.home_path(".gemini/skills/x/SKILL.md"), "just text");
    let scan = scan(&fx.env());
    assert!(matches!(
        ext_of(&scan, ExtKind::Skill, "x")[0].status,
        ExtStatus::Rejected { .. }
    ));
}

fn agent_file(fx: &Fixture, dir: &AbsPath, file: &str, name: &str) -> AbsPath {
    let path = dir.join(file);
    fx.write(
        &path,
        &format!("---\nname: {name}\ndescription: d\n---\nYou are {name}.\n"),
    );
    path
}

#[test]
fn a_project_subagent_waits_for_acknowledgment_and_any_edit_resets_it() {
    // Spec 6: acknowledgments hold the sha256 of the file content.
    let fx = Fixture::new();
    let file = agent_file(&fx, &fx.project_path(".gemini/agents"), "rev.md", "rev");
    let first = scan(&trusted(&fx));
    assert!(matches!(
        ext_of(&first, ExtKind::Subagent, "rev")[0].status,
        ExtStatus::Hidden { .. }
    ));

    let hash = sha256_hex(&std::fs::read(file.as_path()).unwrap());
    fx.write(
        &fx.home_path(".gemini/acknowledgments/agents.json"),
        &format!("{{\"{}\":{{\"rev\":\"{hash}\"}}}}", fx.project()),
    );
    let acked = scan(&trusted(&fx));
    assert_eq!(
        ext_of(&acked, ExtKind::Subagent, "rev")[0].status,
        ExtStatus::Active
    );
    // The acknowledgment file is read-only state.
    let claim = acked
        .files
        .iter()
        .find(|c| c.path == fx.home_path(".gemini/acknowledgments/agents.json"))
        .unwrap();
    assert_eq!(
        claim.policy,
        EditPolicy::ReadOnly {
            why: ReadOnlyWhy::AgentState
        }
    );

    fx.write(&file, "---\nname: rev\ndescription: d\n---\nchanged\n");
    let edited = scan(&trusted(&fx));
    assert!(matches!(
        ext_of(&edited, ExtKind::Subagent, "rev")[0].status,
        ExtStatus::Hidden { .. }
    ));
}

#[test]
fn user_subagents_need_no_acknowledgment_and_underscore_files_are_skipped() {
    let fx = Fixture::new();
    agent_file(&fx, &fx.home_path(".gemini/agents"), "a.md", "a");
    agent_file(&fx, &fx.home_path(".gemini/agents"), "_draft.md", "draft");
    fx.write(&fx.home_path(".gemini/agents/b.md"), "no frontmatter");
    let scan = scan(&fx.env());
    assert_eq!(
        ext_of(&scan, ExtKind::Subagent, "a")[0].status,
        ExtStatus::Active
    );
    assert!(ext_of(&scan, ExtKind::Subagent, "draft").is_empty());
    assert!(matches!(
        ext_of(&scan, ExtKind::Subagent, "b")[0].status,
        ExtStatus::Rejected { .. }
    ));
}

#[test]
fn enable_agents_false_and_overrides_disable_subagents() {
    let fx = Fixture::new();
    agent_file(&fx, &fx.home_path(".gemini/agents"), "a.md", "a");
    agent_file(&fx, &fx.home_path(".gemini/agents"), "c.md", "c");
    user_settings(&fx, r#"{"agents":{"overrides":{"c":{"enabled":false}}}}"#);
    let scan_a = scan(&fx.env());
    assert_eq!(
        ext_of(&scan_a, ExtKind::Subagent, "a")[0].status,
        ExtStatus::Active
    );
    assert_eq!(
        ext_of(&scan_a, ExtKind::Subagent, "c")[0].status,
        ExtStatus::Disabled {
            by: "agents.overrides".to_owned()
        }
    );
    user_settings(&fx, r#"{"experimental":{"enableAgents":false}}"#);
    let off = scan(&fx.env());
    assert!(matches!(
        ext_of(&off, ExtKind::Subagent, "a")[0].status,
        ExtStatus::Disabled { .. }
    ));
}

// ---- Extensions (spec 6) ---------------------------------------------------------------------

fn install(fx: &Fixture, dir: &str, manifest: &str) -> AbsPath {
    let root = fx.home_path(&format!(".gemini/extensions/{dir}"));
    fx.write(&root.join("gemini-extension.json"), manifest);
    root
}

#[test]
fn an_extension_context_file_defaults_to_gemini_md_and_loads_in_tier_two() {
    let fx = Fixture::new();
    let root = install(&fx, "e", r#"{"name":"e","version":"1"}"#);
    fx.write(&root.join("GEMINI.md"), "ext rules");
    let scan = scan(&fx.env());
    let load = only(&scan, &root.join("GEMINI.md"));
    assert_eq!(load.scope, Scope::Plugin { id: "e".to_owned() });
    assert_eq!(load.status, LoadStatus::Loaded);
    // Loaded regardless of trust, and read-only because `extensions update` replaces the copy.
    let entry = scan
        .files
        .iter()
        .find(|c| c.path == root.join("GEMINI.md"))
        .unwrap();
    assert!(matches!(
        entry.policy,
        EditPolicy::ReadOnly {
            why: ReadOnlyWhy::PluginCache { .. }
        }
    ));
}

#[test]
fn extension_context_file_names_reject_absolute_and_parent_paths() {
    let fx = Fixture::new();
    let root = install(
        &fx,
        "e",
        r#"{"name":"e","version":"1","contextFileName":["docs/A.md","../B.md","/abs.md"]}"#,
    );
    fx.write(&root.join("docs/A.md"), "a");
    fx.write(&root.join("../B.md"), "b");
    let scan = scan(&fx.env());
    assert_eq!(
        only(&scan, &root.join("docs/A.md")).status,
        LoadStatus::Loaded
    );
    assert!(find(&scan, &fx.home_path(".gemini/extensions/B.md")).is_empty());
}

#[test]
fn extension_enablement_last_matching_rule_wins() {
    let fx = Fixture::new();
    let root = install(&fx, "e", r#"{"name":"e","version":"1"}"#);
    fx.write(&root.join("GEMINI.md"), "x");
    let enablement = fx.home_path(".gemini/extensions/extension-enablement.json");
    let cwd = fx.project();
    // Disabled everywhere under the home directory, then enabled for this project.
    fx.write(
        &enablement,
        &format!(r#"{{"e":{{"overrides":["!{}/*"]}}}}"#, fx.root()),
    );
    assert!(find(&scan(&fx.env()), &root.join("GEMINI.md")).is_empty());
    fx.write(
        &enablement,
        &format!(r#"{{"e":{{"overrides":["!{}/*","{}/"]}}}}"#, fx.root(), cwd),
    );
    assert_eq!(
        only(&scan(&fx.env()), &root.join("GEMINI.md")).status,
        LoadStatus::Loaded
    );
    // The enablement file is editable but the CLI rewrites it.
    let claim = scan(&fx.env())
        .files
        .into_iter()
        .find(|c| c.path == enablement)
        .unwrap();
    assert_eq!(claim.policy, EditPolicy::Editable);
}

#[test]
fn a_linked_extension_reads_its_files_from_the_source_directory() {
    let fx = Fixture::new();
    let source = fx.root().join("dev-ext");
    fx.write(
        &source.join("gemini-extension.json"),
        r#"{"name":"linked","version":"1"}"#,
    );
    fx.write(&source.join("GEMINI.md"), "from source");
    let dir = fx.home_path(".gemini/extensions/linked");
    fx.write(
        &dir.join(".gemini-extension-install.json"),
        &format!(r#"{{"type":"link","source":"{source}"}}"#),
    );
    let scan = scan(&fx.env());
    assert_eq!(
        only(&scan, &source.join("GEMINI.md")).status,
        LoadStatus::Loaded
    );
    // A linked extension belongs to the user, so its files stay editable.
    assert!(
        scan.files
            .iter()
            .all(|c| c.path != source.join("GEMINI.md"))
    );
}

#[test]
fn a_duplicate_extension_name_is_fatal_and_a_bad_manifest_is_rejected() {
    let fx = Fixture::new();
    install(&fx, "a", r#"{"name":"same","version":"1"}"#);
    install(&fx, "b", r#"{"name":"same","version":"1"}"#);
    install(&fx, "c", r#"{"name":"nover"}"#);
    let scan = scan(&fx.env());
    assert!(scan.view.loads.is_empty());
    assert!(problems(&scan).iter().any(|p| matches!(
        p,
        Problem::ParseError {
            fatal_for_agent: true,
            ..
        }
    )));
    assert!(
        problems(&scan)
            .iter()
            .any(|p| matches!(p, Problem::ExtensionRejected { .. }))
    );
}

#[test]
fn admin_and_security_settings_block_extensions() {
    let fx = Fixture::new();
    let root = install(&fx, "e", r#"{"name":"e","version":"1"}"#);
    fx.write(&root.join("GEMINI.md"), "x");
    fx.write(
        &root.join(".gemini-extension-install.json"),
        r#"{"type":"git","source":"https://example.com/e.git"}"#,
    );
    user_settings(&fx, r#"{"security":{"blockGitExtensions":true}}"#);
    let scan_a = scan(&fx.env());
    assert!(find(&scan_a, &root.join("GEMINI.md")).is_empty());
    assert!(
        problems(&scan_a)
            .iter()
            .any(|p| matches!(p, Problem::ExtensionRejected { .. }))
    );
    user_settings(&fx, r#"{"admin":{"extensions":{"enabled":false}}}"#);
    assert!(find(&scan(&fx.env()), &root.join("GEMINI.md")).is_empty());
}
