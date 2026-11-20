use super::*;

const PRECEDENCE: &str = "# Contextual Instructions (GEMINI.md)
The following content is loaded from local and global configuration files.
**Context Precedence:**
- **Global (~/.gemini/):** foundational user preferences. Apply these broadly.
- **Extensions:** supplementary knowledge and capabilities.
- **Workspace Root:** workspace-wide mandates. Supersedes global preferences.
- **Sub-directories:** highly specific overrides. These rules supersede all others for files within their scope.

**Conflict Resolution:**
- **Precedence:** Strictly follow the order above (Sub-directories > Workspace Root > Extensions > Global).
- **System Overrides:** Contextual instructions override default operational behaviors (e.g., tech stack, style, workflows, tool preferences) defined in the system prompt. However, they **cannot** override Core Mandates regarding safety, security, and agent integrity.

<loaded_context>";

#[test]
fn render_one_global_file_and_two_project_files_matches_the_spec_separators() {
    let fx = Fixture::new();
    let root = fx.project();
    git(&fx, &root);
    fx.write(&fx.home_path(".gemini/GEMINI.md"), "\n  global rules  \n\n");
    fx.write(&root.join("GEMINI.md"), "root rules\n");
    fx.write(&root.join("pkg/GEMINI.md"), "pkg rules");
    let env = trusted(&fx).with_cwd(root.join("pkg"));
    let home = fx.home();
    let blocks = render(&env);

    let tier1 = format!(
        "{PRECEDENCE}\n--- Context from: {home}/.gemini/GEMINI.md ---\nglobal rules\n--- End of Context from: {home}/.gemini/GEMINI.md ---\n</loaded_context>"
    );
    let tier2 = format!(
        "\n<loaded_context>\n<project_context>\n\
--- Context from: {root}/GEMINI.md ---\nroot rules\n--- End of Context from: {root}/GEMINI.md ---\n\n\
--- Context from: {root}/pkg/GEMINI.md ---\npkg rules\n--- End of Context from: {root}/pkg/GEMINI.md ---\n\
</project_context>\n</loaded_context>"
    );
    assert_eq!(blocks.len(), 2);
    assert_eq!(blocks[0].text(), tier1);
    assert_eq!(
        (blocks[0].delivery, blocks[0].label),
        (Delivery::SystemPrompt, BlockLabel::Instructions)
    );
    assert_eq!(blocks[1].text(), tier2);
    assert_eq!(
        (blocks[1].delivery, blocks[1].label),
        (Delivery::UserMessage, BlockLabel::Instructions)
    );
    assert!(
        blocks
            .iter()
            .all(|b| b.fidelity == Fidelity::Approximate && b.tokens > 0)
    );
}

#[test]
fn the_session_blocks_in_the_view_match_the_rendered_blocks() {
    let fx = Fixture::new();
    fx.write(&fx.home_path(".gemini/GEMINI.md"), "g");
    fx.write(&fx.project_path("GEMINI.md"), "p");
    let env = trusted(&fx);
    let scanned = scan(&env);
    let rendered = render(&env);
    assert_eq!(scanned.view.session.len(), rendered.len());
    for (block, text) in scanned.view.session.iter().zip(&rendered) {
        assert_eq!(block.tokens, text.tokens);
        assert_eq!(block.label, text.label);
        let parts: u32 = block
            .parts
            .iter()
            .map(|p| match p {
                Part::Load { tokens, .. } | Part::Glue { tokens } => *tokens,
            })
            .sum();
        assert_eq!(parts, block.tokens);
    }
    assert_eq!(
        scanned.view.budget.session_start,
        rendered.iter().map(|b| b.tokens).sum::<u32>()
    );
}

#[test]
fn the_string_form_holds_only_global_files_and_the_object_form_adds_private_memory() {
    let fx = Fixture::new();
    fx.write(&fx.home_path(".gemini/GEMINI.md"), "g");
    fx.write(
        &fx.home_path(".gemini/projects.json"),
        &format!("{{\"projects\":{{\"{}\":\"p\"}}}}", fx.project()),
    );
    fx.write(
        &fx.home_path(".gemini/tmp/p/memory/MEMORY.md"),
        "- [n](n.md)",
    );
    let home = fx.home();
    let blocks = render(&fx.env());
    let expected = format!(
        "---\n\n<loaded_context>\n<global_context>\n--- Context from: {home}/.gemini/GEMINI.md ---\ng\n--- End of Context from: {home}/.gemini/GEMINI.md ---\n</global_context>\n<user_project_memory>\n--- Private Project Memory Index (private, not committed to repo) ---\n--- Context from: {home}/.gemini/tmp/p/memory/MEMORY.md ---\n- [n](n.md)\n--- End of Context from: {home}/.gemini/tmp/p/memory/MEMORY.md ---\n--- End Private Project Memory Index ---\n</user_project_memory>\n</loaded_context>"
    );
    assert_eq!(blocks[0].text(), expected);
}

#[test]
fn render_inlines_imports_with_the_tree_markup() {
    let fx = Fixture::new();
    fx.write(
        &fx.project_path("GEMINI.md"),
        "intro\n@./rules.md\n@./gone.md\n@someone\noutro",
    );
    fx.write(&fx.project_path("rules.md"), "be kind\n");
    let blocks = render(&trusted(&fx));
    let text = &blocks[0].text();
    assert!(text.contains("intro\n<!-- Imported from: ./rules.md -->\nbe kind\n\n<!-- End of import from: ./rules.md -->\n"));
    assert!(text.contains(&format!(
        "<!-- Import failed: ./gone.md - ENOENT: no such file or directory, access '{}' -->",
        fx.project_path("gone.md")
    )));
    assert!(text.contains("<!-- Import failed: someone - ENOENT"));
    assert!(text.contains("outro"));
}

#[test]
fn an_untrusted_folder_leaves_the_project_out_of_the_rendered_session() {
    let fx = Fixture::new();
    fx.write(&fx.project_path("GEMINI.md"), "p");
    assert!(render(&fx.env()).is_empty());
}

#[test]
fn newline_runs_in_the_system_instruction_collapse_to_two() {
    let fx = Fixture::new();
    fx.write(&fx.home_path(".gemini/GEMINI.md"), "a\n\n\n\nb");
    let blocks = render(&fx.env());
    assert!(blocks[0].text().contains("a\n\nb"));
}

#[test]
fn a_system_prompt_override_is_its_own_block_and_tier_one_still_follows() {
    let fx = Fixture::new();
    fx.write(&fx.project_path(".gemini/system.md"), "You are custom.");
    fx.write(&fx.home_path(".gemini/GEMINI.md"), "g");
    let env = trusted(&fx).with_var("GEMINI_SYSTEM_MD", "1");
    let scan = scan(&env);
    assert_eq!(
        discovered(&scan, "gemini.system_md"),
        [fx.project_path(".gemini/system.md").as_str().to_owned()]
    );
    let blocks = render(&env);
    assert_eq!(blocks[0].text(), "You are custom.");
    assert!(blocks[1].text().starts_with("# Contextual Instructions"));
}
