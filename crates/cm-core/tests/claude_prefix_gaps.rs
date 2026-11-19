//! The content of unknown size that the Claude adapter's session blocks follow, as `scan`
//! returns it (design section 3, `ContextBlock.preceded_by`; spec 4, "Where the listings sit in
//! the request"). Runs on a fixture home, never `~/.claude`.

use cm_core::agents::registry;
use cm_core::model::*;
use cm_core::scan::scan_with_parked;
use cm_core::testing::Fixture;

fn claude_session(fx: &Fixture) -> Vec<ContextBlock> {
    let env = fx.env();
    scan_with_parked(registry(), &env, None)
        .snapshot
        .agents
        .into_iter()
        .find(|v| v.agent == AgentId::parse("claude").unwrap())
        .expect("claude view")
        .session
}

fn skill(fx: &Fixture) {
    fx.write(
        &fx.home_path(".claude/skills/one/SKILL.md"),
        "---\nname: one\ndescription: d\n---\n\nbody\n",
    );
}

#[test]
fn an_instruction_file_takes_the_built_in_gap_and_the_first_catalog_the_first_prompt() {
    let fx = Fixture::new();
    fx.write(&fx.home_path(".claude/CLAUDE.md"), "be kind\n");
    skill(&fx);
    let session = claude_session(&fx);
    assert_eq!(session[0].label, BlockLabel::Instructions);
    assert_eq!(session[0].preceded_by, [Unsized::BuiltIn]);
    let catalog = session
        .iter()
        .position(|b| b.label == BlockLabel::SkillCatalog)
        .unwrap();
    assert_eq!(session[catalog].preceded_by, [Unsized::FirstPrompt]);
    assert!(
        session
            .iter()
            .enumerate()
            .all(|(i, b)| i == 0 || i == catalog || b.preceded_by.is_empty())
    );
}

#[test]
fn a_skill_without_an_instruction_file_starts_with_both_gaps() {
    let fx = Fixture::new();
    skill(&fx);
    let session = claude_session(&fx);
    assert_eq!(session[0].label, BlockLabel::SkillCatalog);
    assert_eq!(
        session[0].preceded_by,
        [Unsized::BuiltIn, Unsized::FirstPrompt]
    );
}
