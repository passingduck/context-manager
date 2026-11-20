use super::*;

fn register(fx: &Fixture, slug: &str) {
    fx.write(
        &fx.home_path(".gemini/projects.json"),
        &format!(
            "{{\"projects\":{{\"{}\":\"{slug}\",\"/other/project\":\"other\"}}}}",
            fx.project()
        ),
    );
}

#[test]
fn slug_lookup_reads_projects_json_and_never_creates_it() {
    // Spec 4: no entry means no private memory yet, and the scanner creates nothing.
    let fx = Fixture::new();
    fx.mkdir(&fx.home_path(".gemini"));
    let before = scan(&fx.env());
    assert!(before.view.memory.is_empty());
    assert!(!fx.home_path(".gemini/projects.json").as_path().exists());
    assert!(!fx.home_path(".gemini/tmp").as_path().exists());

    register(&fx, "my-proj");
    let after = scan(&fx.env());
    assert_eq!(after.view.memory.len(), 1);
    assert_eq!(after.view.memory[0].id, "gemini.private");
    assert_eq!(
        after.view.memory[0].root,
        fx.home_path(".gemini/tmp/my-proj/memory")
    );
}

#[test]
fn the_slug_is_keyed_by_the_working_directory_not_the_git_root() {
    let fx = Fixture::new();
    git(&fx, &fx.project());
    register(&fx, "p");
    let env = fx.env().with_cwd(fx.project_path("sub"));
    fx.mkdir(&fx.project_path("sub"));
    assert!(scan(&env).view.memory.is_empty());
}

#[test]
fn memory_md_is_the_loaded_index_and_sibling_notes_are_model_may_read() {
    let fx = Fixture::new();
    register(&fx, "p");
    let dir = fx.home_path(".gemini/tmp/p/memory");
    fx.write(&dir.join("MEMORY.md"), "- [style](style.md)\n");
    fx.write(
        &dir.join("style.md"),
        "---\ndescription: how I write\n---\n# Style\nbody",
    );
    fx.write(&dir.join("loose.md"), "# Loose\nnot indexed");
    let scan = scan(&fx.env());
    let index = only(&scan, &dir.join("MEMORY.md"));
    assert_eq!(index.kind, DocKind::MemoryIndex);
    assert_eq!(index.when, Activation::SessionStart);
    let style = only(&scan, &dir.join("style.md"));
    assert_eq!(style.when, Activation::ModelMayRead);
    assert_eq!(style.kind, DocKind::MemoryTopic);

    let store = &scan.view.memory[0];
    assert_eq!(store.index, Some(dir.join("MEMORY.md")));
    let note = |name: &str| {
        store
            .entries
            .iter()
            .find(|e| e.file == dir.join(name))
            .unwrap()
    };
    assert_eq!(note("style.md").role, MemoryRole::Note);
    assert_eq!(note("style.md").indexed, Some(true));
    assert_eq!(note("style.md").title, "Style");
    assert_eq!(note("style.md").description.as_deref(), Some("how I write"));
    assert_eq!(note("loose.md").indexed, Some(false));
    assert_eq!(note("MEMORY.md").role, MemoryRole::Index);
    assert_eq!(store.write, MemoryWrite::Direct);
}

#[test]
fn the_legacy_private_file_loads_only_when_memory_md_is_missing() {
    let fx = Fixture::new();
    register(&fx, "p");
    let dir = fx.home_path(".gemini/tmp/p/memory");
    fx.write(&dir.join("GEMINI.md"), "legacy");
    let legacy = scan(&fx.env());
    assert_eq!(
        only(&legacy, &dir.join("GEMINI.md")).status,
        LoadStatus::Loaded
    );
    fx.write(&dir.join("MEMORY.md"), "index");
    let modern = scan(&fx.env());
    assert_eq!(
        discovered(&modern, "gemini.memory.private"),
        [dir.join("MEMORY.md").as_str().to_owned()]
    );
}

#[test]
fn the_inbox_and_service_state_are_read_only() {
    let fx = Fixture::new();
    register(&fx, "p");
    let dir = fx.home_path(".gemini/tmp/p/memory");
    fx.write(&dir.join("MEMORY.md"), "i");
    fx.write(&dir.join(".inbox/private/fix.patch"), "diff");
    fx.write(&dir.join(".inbox/global/g.patch"), "diff");
    fx.write(&dir.join(".extraction-state.json"), "{}");
    fx.write(&dir.join("skills/draft/SKILL.md"), "draft");
    fx.write(
        &fx.home_path(".gemini/tmp/p/.project_root"),
        fx.project().as_str(),
    );
    let scan = scan(&fx.env());
    let policy = |path: AbsPath| {
        scan.files
            .iter()
            .find(|c| c.path == path)
            .unwrap()
            .policy
            .clone()
    };
    assert_eq!(
        policy(dir.join(".inbox/private/fix.patch")),
        EditPolicy::ReadOnly {
            why: ReadOnlyWhy::Generated
        }
    );
    assert_eq!(
        policy(dir.join(".inbox/global/g.patch")),
        EditPolicy::ReadOnly {
            why: ReadOnlyWhy::Generated
        }
    );
    assert_eq!(
        policy(dir.join(".extraction-state.json")),
        EditPolicy::ReadOnly {
            why: ReadOnlyWhy::AgentState
        }
    );
    assert_eq!(
        policy(fx.home_path(".gemini/projects.json")),
        EditPolicy::ReadOnly {
            why: ReadOnlyWhy::AgentState
        }
    );
    assert_eq!(
        policy(fx.home_path(".gemini/tmp/p/.project_root")),
        EditPolicy::ReadOnly {
            why: ReadOnlyWhy::AgentState
        }
    );
    let generated = scan.view.memory[0]
        .entries
        .iter()
        .filter(|e| e.role == MemoryRole::Generated)
        .count();
    assert_eq!(generated, 4);
}

#[test]
fn the_sandbox_runtime_directory_is_a_second_place_to_look() {
    let fx = Fixture::new();
    let runtime = fx.home_path(".cache/.gemini");
    fx.write(
        &runtime.join("projects.json"),
        &format!("{{\"projects\":{{\"{}\":\"sb\"}}}}", fx.project()),
    );
    fx.write(&runtime.join("tmp/sb/memory/MEMORY.md"), "sandbox index");
    let scan = scan(&fx.env());
    assert_eq!(scan.view.memory.len(), 1);
    let store = &scan.view.memory[0];
    assert_eq!(store.id, "gemini.private.sandbox");
    assert!(matches!(store.enabled, Enabled::Off { .. }));
    assert_eq!(store.index, Some(runtime.join("tmp/sb/memory/MEMORY.md")));
    // A normal session does not load it.
    assert!(find(&scan, &runtime.join("tmp/sb/memory/MEMORY.md")).is_empty());
}

#[test]
fn known_projects_are_the_keys_of_projects_json() {
    let fx = Fixture::new();
    register(&fx, "p");
    let env = fx.env();
    let fs = ProbeFs::new();
    let projects = Gemini.known_projects(&ScanCx {
        env: &env,
        fs: &fs,
        parked_dir: None,
    });
    assert_eq!(
        projects,
        [fx.project(), AbsPath::parse("/other/project").unwrap()]
    );
}

#[test]
fn a_private_memory_index_switches_the_system_instruction_to_the_object_form() {
    let fx = Fixture::new();
    register(&fx, "p");
    fx.write(
        &fx.home_path(".gemini/tmp/p/memory/MEMORY.md"),
        "remember this",
    );
    let blocks = render(&fx.env());
    assert_eq!(blocks.len(), 1);
    assert!(
        blocks[0]
            .text()
            .starts_with("---\n\n<loaded_context>\n<user_project_memory>\n")
    );
}
