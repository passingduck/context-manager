use super::*;
use crate::agents::ScanCx;
use crate::env::ScanEnv;
use crate::probe_fs::ProbeFs;
use crate::testing::Fixture;

fn run_with(env: &ScanEnv) -> ExtOut {
    let fs = ProbeFs::new();
    let cx = ScanCx::new(env, &fs);
    let setup = Setup::load(&cx);
    scan(&cx, &setup, 10)
}

fn run(fx: &Fixture) -> ExtOut {
    fx.mkdir(&fx.project_path(".git"));
    run_with(&fx.env())
}

fn find<'a>(out: &'a ExtOut, name: &str) -> Vec<&'a Extension> {
    out.extensions.iter().filter(|e| e.name == name).collect()
}

fn skill(fx: &Fixture, base: &AbsPath, name: &str, front: &str) {
    fx.write(
        &base.join(&format!("skills/{name}/SKILL.md")),
        &format!("---\n{front}\n---\nBody of {name}\n"),
    );
}

#[test]
fn managed_beats_user_beats_project_and_skills_beat_commands() {
    // Spec 4, "Name collisions".
    let fx = Fixture::new();
    let managed = fx.root().join("managed");
    let env = fx.env().with_var(
        crate::agents::claude::setup::TEST_MANAGED_DIR_VAR,
        managed.as_str(),
    );
    skill(
        &fx,
        &managed.join(".claude"),
        "dup",
        "description: managed one",
    );
    skill(
        &fx,
        &fx.home_path(".claude"),
        "dup",
        "description: user one",
    );
    skill(
        &fx,
        &fx.project_path(".claude"),
        "dup",
        "description: project one",
    );
    fx.write(
        &fx.project_path(".claude/commands/dup.md"),
        "A command named dup\n",
    );
    fx.mkdir(&fx.project_path(".git"));
    let out = run_with(&env);

    let dups = find(&out, "dup");
    assert_eq!(dups.len(), 4);
    let by_desc = |d: &str| {
        dups.iter()
            .find(|e| e.description.as_deref() == Some(d))
            .copied()
            .unwrap()
    };
    let managed_ext = by_desc("managed one");
    assert_eq!(managed_ext.status, ExtStatus::Active);
    for loser in [
        by_desc("user one"),
        by_desc("project one"),
        by_desc("A command named dup"),
    ] {
        assert_eq!(
            loser.status,
            ExtStatus::Shadowed {
                by: managed_ext.id.clone()
            }
        );
        assert_eq!(loser.listing, None);
    }
}

#[test]
fn a_user_skill_beats_a_project_skill() {
    let fx = Fixture::new();
    skill(
        &fx,
        &fx.home_path(".claude"),
        "dup",
        "description: user one",
    );
    skill(
        &fx,
        &fx.project_path(".claude"),
        "dup",
        "description: project one",
    );
    let out = run(&fx);
    let dups = find(&out, "dup");
    let user = dups.iter().find(|e| e.scope == Scope::User).unwrap();
    let project = dups.iter().find(|e| e.scope != Scope::User).unwrap();
    assert_eq!(user.status, ExtStatus::Active);
    assert_eq!(
        project.status,
        ExtStatus::Shadowed {
            by: user.id.clone()
        }
    );
}

#[test]
fn a_skill_has_a_listing_at_session_start_and_a_body_on_invoke() {
    // Spec 4, "What reaches the model".
    let fx = Fixture::new();
    skill(
        &fx,
        &fx.home_path(".claude"),
        "plain",
        "description: Does a thing\nwhen_to_use: when asked",
    );
    skill(
        &fx,
        &fx.home_path(".claude"),
        "hidden",
        "description: x\ndisable-model-invocation: yes",
    );
    skill(
        &fx,
        &fx.home_path(".claude"),
        "gated",
        "description: y\npaths: \"src/**\"",
    );
    let out = run(&fx);

    let plain = &find(&out, "plain")[0];
    let listing = &out.loads[(plain.listing.clone().unwrap().0 - 10) as usize];
    assert_eq!(listing.kind, DocKind::Listing);
    assert_eq!(listing.when, Activation::SessionStart);
    assert_eq!(
        listing.via,
        Via::Listing {
            ext: plain.id.clone()
        }
    );
    assert_eq!(listing.status, LoadStatus::Loaded);
    let body = &out.loads[(plain.body.0 - 10) as usize];
    assert_eq!(body.kind, DocKind::SkillBody);
    assert_eq!(body.when, Activation::OnInvoke);
    assert_eq!(
        body.delivered.as_ref().unwrap().bytes,
        "Body of plain\n".len() as u64
    );
    assert!(body.delivered.as_ref().unwrap().transformed);
    // Load ids count from the first id the caller passed.
    assert!(
        out.loads
            .iter()
            .enumerate()
            .all(|(i, l)| l.id.0 == 10 + i as u32)
    );

    let hidden = &find(&out, "hidden")[0];
    assert!(matches!(hidden.status, ExtStatus::Hidden { .. }));
    assert_eq!(hidden.listing, None);
    let gated = &find(&out, "gated")[0];
    assert_eq!(
        gated.status,
        ExtStatus::Conditional {
            patterns: vec!["src/**".to_owned()]
        }
    );
}

#[test]
fn a_command_name_joins_folders_with_a_colon_and_falls_back_to_the_first_line() {
    // Spec 4, Custom commands.
    let fx = Fixture::new();
    fx.write(
        &fx.project_path(".claude/commands/frontend/component.md"),
        "\n# Make a component\nmore\n",
    );
    let out = run(&fx);
    let cmd = &find(&out, "frontend:component")[0];
    assert_eq!(cmd.ext_kind, ExtKind::Command);
    assert_eq!(cmd.description.as_deref(), Some("Make a component"));
    assert_eq!(
        out.loads[(cmd.body.0 - 10) as usize].kind,
        DocKind::CommandBody
    );
}

#[test]
fn an_overlong_description_is_cut_at_1536_characters() {
    let fx = Fixture::new();
    let long = "x".repeat(2000);
    skill(
        &fx,
        &fx.home_path(".claude"),
        "wordy",
        &format!("description: {long}"),
    );
    let out = run(&fx);
    let ext = &find(&out, "wordy")[0];
    let listing = &out.loads[(ext.listing.clone().unwrap().0 - 10) as usize];
    assert!(matches!(
        listing.status,
        LoadStatus::Truncated {
            kept: 1536,
            total: 2000,
            ..
        }
    ));
}

#[test]
fn the_catalog_over_its_budget_is_reported() {
    // 200,000 tokens x 4 x 0.01 = 8,000 characters.
    let fx = Fixture::new();
    for i in 0..10 {
        skill(
            &fx,
            &fx.home_path(".claude"),
            &format!("s{i}"),
            &format!("description: {}", "d".repeat(1000)),
        );
    }
    let out = run(&fx);
    let over = out
        .diagnostics
        .iter()
        .find_map(|d| match &d.problem {
            Problem::CatalogOverBudget { used, limit } => Some((*used, limit.clone())),
            _ => None,
        })
        .expect("a budget diagnostic");
    assert!(over.0 > 8_000);
    assert_eq!(over.1.value, 8_000);
    assert_eq!(over.1.kind, LimitKind::ListingBudgetChars);
}

#[test]
fn subagent_files_claude_skips_are_rejected() {
    // Spec 4, "Subagent files Claude Code skips".
    let fx = Fixture::new();
    let agents = fx.project_path(".claude/agents");
    fx.write(
        &agents.join("good.md"),
        "---\nname: good\ndescription: ok\n---\nprompt\n",
    );
    fx.write(
        &agents.join("noname.md"),
        "---\ndescription: ok\n---\nprompt\n",
    );
    fx.write(
        &agents.join("colon.md"),
        "---\nname: a:b\ndescription: ok\n---\nprompt\n",
    );
    fx.write(
        &agents.join("nodesc.md"),
        "---\nname: nodesc\n---\nprompt\n",
    );
    fx.write(
        &agents.join("late.md"),
        "\n---\nname: late\ndescription: ok\n---\nprompt\n",
    );
    let out = run(&fx);
    let status = |file: &str| {
        out.extensions
            .iter()
            .find(|e| e.file.file_name() == Some(file))
            .map(|e| e.status.clone())
            .unwrap()
    };
    assert_eq!(status("good.md"), ExtStatus::Active);
    for file in ["noname.md", "colon.md", "nodesc.md", "late.md"] {
        assert!(matches!(status(file), ExtStatus::Rejected { .. }), "{file}");
    }
}

#[test]
fn a_project_agent_beats_a_user_agent_of_the_same_name() {
    let fx = Fixture::new();
    fx.write(
        &fx.home_path(".claude/agents/rev.md"),
        "---\nname: rev\ndescription: user\n---\np\n",
    );
    fx.write(
        &fx.project_path(".claude/agents/rev.md"),
        "---\nname: rev\ndescription: project\n---\np\n",
    );
    let out = run(&fx);
    let revs = find(&out, "rev");
    let project = revs
        .iter()
        .find(|e| e.description.as_deref() == Some("project"))
        .unwrap();
    let user = revs
        .iter()
        .find(|e| e.description.as_deref() == Some("user"))
        .unwrap();
    assert_eq!(project.status, ExtStatus::Active);
    assert_eq!(
        user.status,
        ExtStatus::Shadowed {
            by: project.id.clone()
        }
    );
}

#[test]
fn a_nested_skills_directory_loads_on_demand() {
    let fx = Fixture::new();
    skill(
        &fx,
        &fx.project_path("pkg/.claude"),
        "inner",
        "description: nested",
    );
    let out = run(&fx);
    let inner = &find(&out, "inner")[0];
    assert_eq!(
        inner.status,
        ExtStatus::Conditional {
            patterns: vec!["pkg/**".to_owned()]
        }
    );
}

fn plugin_fixture(fx: &Fixture) -> AbsPath {
    let cache = fx.home_path(".claude/plugins/cache/mkt");
    for name in ["on", "off"] {
        let root = cache.join(&format!("{name}/1.0.0"));
        fx.write(
            &root.join(".claude-plugin/plugin.json"),
            &format!("{{\"name\":\"{name}\"}}"),
        );
        skill(fx, &root, "helper", "description: plugin skill");
        fx.write(&root.join("commands/db/migrate.md"), "Migrate\n");
        fx.write(
            &root.join("agents/sub/bot.md"),
            "---\ndescription: a bot\n---\np\n",
        );
    }
    fx.write(
        &fx.home_path(".claude/plugins/installed_plugins.json"),
        &format!(
            "{{\"version\":2,\"plugins\":{{\"on@mkt\":[{{\"scope\":\"user\",\"installPath\":\"{0}/on/1.0.0\"}}],\"off@mkt\":[{{\"scope\":\"user\",\"installPath\":\"{0}/off/1.0.0\"}}]}}}}",
            cache.as_str()
        ),
    );
    fx.write(
        &fx.home_path(".claude/settings.json"),
        "{\"enabledPlugins\":{\"on@mkt\":true,\"off@mkt\":false}}",
    );
    cache
}

#[test]
fn plugin_components_are_namespaced_attributed_and_read_only() {
    // Spec 4, Plugins and the component table.
    let fx = Fixture::new();
    plugin_fixture(&fx);
    let out = run(&fx);

    let skill_ext = &find(&out, "on:helper")[0];
    assert_eq!(
        skill_ext.scope,
        Scope::Plugin {
            id: "on@mkt".to_owned()
        }
    );
    assert_eq!(skill_ext.status, ExtStatus::Active);
    assert!(find(&out, "on:db:migrate").len() == 1);
    assert!(find(&out, "on:sub:bot").len() == 1);

    let off = &find(&out, "off:helper")[0];
    assert_eq!(
        off.status,
        ExtStatus::Disabled {
            by: "off@mkt".to_owned()
        }
    );
    assert_eq!(off.listing, None);

    let claim = out.files.iter().find(|c| c.path == skill_ext.file).unwrap();
    assert_eq!(
        claim.policy,
        EditPolicy::ReadOnly {
            why: ReadOnlyWhy::PluginCache {
                plugin: "on@mkt".to_owned()
            }
        }
    );
}

#[test]
fn a_cached_plugin_without_an_install_record_is_not_a_plugin() {
    // Spec 4: the cache alone does not tell which plugins are installed.
    let fx = Fixture::new();
    let root = fx.home_path(".claude/plugins/cache/mkt/ghost/1.0.0");
    skill(&fx, &root, "helper", "description: x");
    fx.write(
        &root.join(".claude-plugin/plugin.json"),
        "{\"name\":\"ghost\"}",
    );
    let out = run(&fx);
    assert!(out.plugins.is_empty());
    assert!(find(&out, "ghost:helper").is_empty());
}

#[test]
fn managed_skills_are_read_only_and_synced_skills_never_win() {
    let fx = Fixture::new();
    let managed = fx.root().join("managed");
    let env = fx.env().with_var(
        crate::agents::claude::setup::TEST_MANAGED_DIR_VAR,
        managed.as_str(),
    );
    skill(&fx, &managed.join(".claude"), "policy", "description: org");
    skill(
        &fx,
        &fx.home_path(".claude/skills/synced/aaa_bbb"),
        "pdf",
        "description: synced",
    );
    // `skill()` writes below `<base>/skills/<name>`, so rebuild the synced layout by hand.
    fx.write(
        &fx.home_path(".claude/skills/synced/aaa_bbb/pdf/SKILL.md"),
        "---\ndescription: synced\n---\nb\n",
    );
    fx.mkdir(&fx.project_path(".git"));
    let out = run_with(&env);
    let policy = &find(&out, "policy")[0];
    assert!(out.files.iter().any(|c| c.path == policy.file
        && c.policy
            == EditPolicy::ReadOnly {
                why: ReadOnlyWhy::Managed
            }));
    let synced = &find(&out, "anthropic-skills:pdf")[0];
    assert!(out.files.iter().any(|c| c.path == synced.file
        && c.policy
            == EditPolicy::ReadOnly {
                why: ReadOnlyWhy::Synced
            }));
}
