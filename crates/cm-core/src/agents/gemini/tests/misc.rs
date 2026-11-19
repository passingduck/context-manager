use super::*;
use crate::agents::registry;

#[test]
fn a_machine_without_gemini_is_absent_with_no_loads_and_slots_that_say_where_files_go() {
    let fx = Fixture::new();
    let scan = scan(&fx.env());
    assert_eq!(scan.view.presence, Presence::Absent);
    assert!(scan.view.loads.is_empty());
    assert!(scan.view.session.is_empty());
    assert!(!scan.view.homes[0].exists);
    assert_eq!(scan.view.homes[0].path, fx.home_path(".gemini"));
    let places: Vec<_> = scan.view.slots.iter().map(|s| s.id.0.clone()).collect();
    assert_eq!(
        places,
        [
            "gemini:instructions:user",
            "gemini:instructions:project",
            "gemini:instructions:subdir",
            "gemini:command:user",
            "gemini:skill:user",
            "gemini:subagent:user",
            "gemini:command:project",
            "gemini:skill:project",
            "gemini:subagent:project",
        ]
    );
    assert_eq!(
        scan.view.slots[0].place,
        Place::File {
            path: fx.home_path(".gemini/GEMINI.md")
        }
    );
    // Nothing was created.
    assert!(!fx.home_path(".gemini").as_path().exists());
}

#[test]
fn gemini_cli_home_replaces_the_home_directory() {
    // Spec 7: config lives in `<it>/.gemini`, and `~/.agents/skills` moves with it.
    let fx = Fixture::new();
    let moved = fx.root().join("moved");
    fx.write(&moved.join(".gemini/GEMINI.md"), "moved global");
    fx.write(&fx.home_path(".gemini/GEMINI.md"), "real home");
    fx.write(
        &moved.join(".agents/skills/s/SKILL.md"),
        "---\nname: s\ndescription: d\n---\nb",
    );
    let env = fx.env().with_var("GEMINI_CLI_HOME", moved.as_str());
    let scan = scan(&env);
    assert_eq!(scan.view.homes[0].path, moved.join(".gemini"));
    assert_eq!(
        scan.view.homes[0].from_env.as_deref(),
        Some("GEMINI_CLI_HOME")
    );
    assert_eq!(
        discovered(&scan, "gemini.context.global"),
        [moved.join(".gemini/GEMINI.md").as_str().to_owned()]
    );
    assert_eq!(scan.view.extensions[0].name, "s");
}

#[test]
fn a_settings_file_that_does_not_parse_is_fatal_for_the_agent() {
    let fx = Fixture::new();
    fx.write(&fx.project_path("GEMINI.md"), "p");
    user_settings(&fx, "{\"a\": 1,}");
    let scan = scan(&trusted(&fx));
    assert!(scan.view.loads.is_empty());
    let fatal = scan.diagnostics.iter().find_map(|d| match &d.problem {
        Problem::ParseError {
            detail,
            fatal_for_agent: true,
            ..
        } => Some((d, detail)),
        _ => None,
    });
    let (diagnostic, detail) = fatal.expect("a fatal parse error");
    assert!(detail.contains("trailing comma"), "{detail}");
    assert_eq!(diagnostic.agent, Some(AgentId::parse("gemini").unwrap()));
    // Comments are fine.
    user_settings(&fx, "// note\n{\"a\": 1 /* x */}");
    assert!(
        !problems(&self::scan(&trusted(&fx)))
            .iter()
            .any(|p| matches!(p, Problem::ParseError { .. }))
    );
}

#[test]
fn settings_that_are_not_an_object_are_fatal() {
    let fx = Fixture::new();
    user_settings(&fx, "[1, 2]");
    assert!(problems(&scan(&fx.env())).iter().any(|p| matches!(
        p,
        Problem::ParseError {
            fatal_for_agent: true,
            ..
        }
    )));
}

#[test]
fn the_workspace_settings_file_is_not_read_when_the_working_directory_is_home() {
    let fx = Fixture::new();
    fx.write(&fx.home_path(".gemini/settings.json"), "{}");
    let env = trusted(&fx).with_cwd(fx.home());
    let scan = scan(&env);
    assert!(
        scan.view
            .hook_files
            .iter()
            .all(|f| f.file != fx.home_path(".gemini/settings.json") || f.scope == Scope::User)
    );
    assert_eq!(scan.view.hook_files.len(), 1);
    assert!(
        scan.view
            .slots
            .iter()
            .all(|s| !s.id.0.ends_with(":project") || s.id.0 == "gemini:instructions:project")
    );
}

#[test]
fn env_variables_in_the_system_settings_path_move_the_system_files() {
    let fx = Fixture::new();
    let sys = fx.root().join("etc");
    fx.write(
        &sys.join("settings.json"),
        r#"{"context":{"fileName":"SYS.md"}}"#,
    );
    fx.write(
        &sys.join("system-defaults.json"),
        r#"{"context":{"fileName":"DEF.md"}}"#,
    );
    let env = fx.env().with_var(
        "GEMINI_CLI_SYSTEM_SETTINGS_PATH",
        sys.join("settings.json").as_str(),
    );
    let scan = scan(&env);
    // The later layer (system) wins a scalar, and the defaults file moved with the path.
    assert!(scan.view.assumptions.iter().any(
        |a| matches!(a, Assumption::Mode { name, value, set_in } if name == "context.fileName" && value == "SYS.md,GEMINI.md" && *set_in == Some(sys.join("settings.json")))
    ));
    let claim = |p: AbsPath| {
        scan.files
            .iter()
            .find(|c| c.path == p)
            .unwrap()
            .policy
            .clone()
    };
    assert_eq!(
        claim(sys.join("settings.json")),
        EditPolicy::ReadOnly {
            why: ReadOnlyWhy::Managed
        }
    );
    assert_eq!(
        claim(sys.join("system-defaults.json")),
        EditPolicy::ReadOnly {
            why: ReadOnlyWhy::Managed
        }
    );
    assert!(scan.view.assumptions.iter().any(
        |a| matches!(a, Assumption::Unknown { what } if what.0 == "gemini.system_file_ownership")
    ));
}

#[test]
fn guard_refuses_cli_owned_state_files() {
    let g = |p: &str| Gemini.guard(&AbsPath::parse(p).unwrap(), None, Some("{}"));
    for path in [
        "/h/.gemini/trusted_hooks.json",
        "/h/.gemini/trustedFolders.json",
        "/h/.gemini/projects.json",
        "/h/.gemini/acknowledgments/agents.json",
        "/h/.gemini/tmp/p/.project_root",
        "/h/.gemini/tmp/p/memory/.inbox/private/x.patch",
        "/h/.gemini/tmp/p/memory/.extraction-state.json",
        "/h/.gemini/tmp/p/memory/.extraction.lock",
        "/etc/gemini-cli/settings.json",
        "/Library/Application Support/GeminiCli/system-defaults.json",
    ] {
        assert_eq!(
            g(path),
            Err(ApiError::ReadOnly {
                why: RuleRef::new("gemini.state_file")
            }),
            "{path}"
        );
    }
    for path in [
        "/h/.gemini/GEMINI.md",
        "/h/.gemini/tmp/p/memory/MEMORY.md",
        "/h/.gemini/extensions/extension-enablement.json",
    ] {
        assert_eq!(g(path), Ok(()), "{path}");
    }
}

#[test]
fn guard_refuses_a_settings_save_that_would_not_parse() {
    let path = AbsPath::parse("/h/.gemini/settings.json").unwrap();
    let refuse = |before: Option<&str>, after: &str| Gemini.guard(&path, before, Some(after));
    assert!(refuse(Some("{}"), "{\"a\":[1,],}").is_err());
    assert!(refuse(None, "{").is_err());
    assert_eq!(refuse(Some("{}"), "// c\n{\"a\": 1}"), Ok(()));
    // A file that was already broken may be saved while it is being fixed.
    assert_eq!(refuse(Some("{,}"), "{,,}"), Ok(()));
    // Other files are not parsed.
    assert_eq!(
        Gemini.guard(
            &AbsPath::parse("/h/.gemini/GEMINI.md").unwrap(),
            None,
            Some("{")
        ),
        Ok(())
    );
}

#[test]
fn every_rule_ref_in_the_adapter_has_both_languages() {
    // Spec: every `RuleRef` the code can emit is a string literal that starts with `gemini.`.
    let sources = [
        include_str!("../compose.rs"),
        include_str!("../context.rs"),
        include_str!("../extension.rs"),
        include_str!("../exts.rs"),
        include_str!("../guard.rs"),
        include_str!("../hooks.rs"),
        include_str!("../memory.rs"),
        include_str!("../mod.rs"),
        include_str!("../prompt.rs"),
        include_str!("../settings.rs"),
        include_str!("../slots.rs"),
        include_str!("../trust.rs"),
        include_str!("../util.rs"),
        include_str!("../world.rs"),
    ];
    // Store ids are not rules.
    let not_rules = ["gemini.private", "gemini.private.sandbox"];
    let rules = Gemini.rules();
    let mut seen = 0;
    for source in sources {
        let production = source.split("#[cfg(test)]").next().unwrap();
        for part in production.split("\"gemini.").skip(1) {
            let id = format!("gemini.{}", part.split('"').next().unwrap());
            if not_rules.contains(&id.as_str())
                || id.ends_with('.')
                || !id
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c == '.' || c == '_')
            {
                continue;
            }
            seen += 1;
            let rule = rules
                .iter()
                .find(|r| r.id.0 == id)
                .unwrap_or_else(|| panic!("{id} is not in rules.toml"));
            assert!(
                !rule.en.trim().is_empty()
                    && !rule.ko.trim().is_empty()
                    && !rule.evidence.trim().is_empty(),
                "{id}"
            );
        }
    }
    assert!(seen > 30, "found only {seen} rule references");
}

#[test]
fn rules_toml_has_no_unused_rule_and_its_korean_has_no_arrow_or_middle_dot() {
    let used: String = [
        include_str!("../compose.rs"),
        include_str!("../context.rs"),
        include_str!("../extension.rs"),
        include_str!("../exts.rs"),
        include_str!("../guard.rs"),
        include_str!("../hooks.rs"),
        include_str!("../memory.rs"),
        include_str!("../mod.rs"),
        include_str!("../prompt.rs"),
        include_str!("../settings.rs"),
        include_str!("../slots.rs"),
        include_str!("../trust.rs"),
        include_str!("../world.rs"),
    ]
    .join("\n");
    for rule in Gemini.rules() {
        assert!(
            used.contains(&format!("\"{}\"", rule.id.0)),
            "{} is never used",
            rule.id.0
        );
        for symbol in ['→', '←', '·', '↔', '⇒'] {
            assert!(!rule.ko.contains(symbol), "{}", rule.id.0);
        }
    }
}

#[test]
fn the_registry_still_lists_gemini_third() {
    assert_eq!(registry()[2].id().as_str(), "gemini");
}

#[test]
fn a_project_untrusted_diagnostic_names_what_was_skipped() {
    let fx = Fixture::new();
    fx.write(&fx.project_path("GEMINI.md"), "p");
    fx.write(&fx.project_path(".gemini/settings.json"), "{}");
    fx.write(
        &fx.project_path(".gemini/agents/a.md"),
        "---\nname: a\ndescription: d\n---\nb",
    );
    let scan = scan(&fx.env());
    let skipped = problems(&scan)
        .into_iter()
        .find_map(|p| match p {
            Problem::ProjectUntrusted { skipped } => {
                Some(skipped.iter().map(|r| r.0.as_str()).collect::<Vec<_>>())
            }
            _ => None,
        })
        .unwrap();
    assert_eq!(
        skipped,
        [
            "gemini.context.project",
            "gemini.settings.workspace",
            "gemini.agent"
        ]
    );
}

#[test]
fn the_ide_workspace_folders_join_the_workspace() {
    let fx = Fixture::new();
    let other = fx.root().join("ide-folder");
    fx.write(&other.join("GEMINI.md"), "ide");
    let env = trusted(&fx).with_var(
        "GEMINI_CLI_IDE_WORKSPACE_PATH",
        &format!("{}:{}", fx.project(), other),
    );
    assert_eq!(
        discovered(&scan(&env), "gemini.context.project"),
        [other.join("GEMINI.md").as_str().to_owned()]
    );
}

#[test]
fn system_md_is_read_from_the_env_file_only_in_a_trusted_folder() {
    let fx = Fixture::new();
    fx.write(&fx.project_path(".env"), "GEMINI_SYSTEM_MD=custom.md\n");
    fx.write(&fx.project_path("custom.md"), "custom prompt");
    let untrusted = scan(&fx.env());
    assert!(discovered(&untrusted, "gemini.system_md").is_empty());
    let trusted_scan = scan(&trusted(&fx));
    assert_eq!(
        discovered(&trusted_scan, "gemini.system_md"),
        [fx.project_path("custom.md").as_str().to_owned()]
    );
    // A missing file is reported as missing.
    let env = trusted(&fx).with_var("GEMINI_SYSTEM_MD", "nowhere.md");
    let missing = scan(&env);
    assert_eq!(
        *skip(only(&missing, &fx.project_path("nowhere.md"))),
        Skip::Missing
    );
    // `0` turns the override off.
    let off = scan(&trusted(&fx).with_var("GEMINI_SYSTEM_MD", "0"));
    assert!(find(&off, &fx.project_path("custom.md")).is_empty());
}

#[test]
fn write_system_md_makes_the_target_generated() {
    let fx = Fixture::new();
    let env = fx.env().with_var("GEMINI_WRITE_SYSTEM_MD", "1");
    let scan = scan(&env);
    let claim = scan
        .files
        .iter()
        .find(|c| c.path == fx.project_path(".gemini/system.md"))
        .unwrap();
    assert_eq!(
        claim.policy,
        EditPolicy::ReadOnly {
            why: ReadOnlyWhy::Generated
        }
    );
}
