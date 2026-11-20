//! What the model receives from each load, place by place: the `load_texts` operation and the
//! pieces of `render`, on fixture trees.
//!
//! Every fixture runs through `agreement`: for every block of every agent, the scan's parts and
//! the rendered pieces are the same list (same kinds, same loads, same counts), every load piece
//! is what `load_texts` returns for that load, and every delivered load's text counts to the
//! tokens the scan reports for it. Each test then checks the text of one kind of load against
//! its spec.

use cm_core::agents::{ScanCx, find};
use cm_core::env::ScanEnv;
use cm_core::scan::{Scanned, scan_full};
use cm_core::testing::Fixture;
use cm_core::texts::load_texts;
use cm_core::*;

struct Run {
    env: ScanEnv,
    scanned: Scanned,
}

impl Run {
    fn new(env: ScanEnv) -> Run {
        let scanned = scan_full(&env);
        let run = Run { env, scanned };
        run.agreement();
        run
    }

    fn cx(&self) -> ScanCx<'_> {
        ScanCx::new(&self.env, self.scanned.fs())
    }

    fn view(&self, agent: &str) -> &AgentView {
        self.scanned
            .snapshot
            .agents
            .iter()
            .find(|v| v.agent.as_str() == agent)
            .unwrap()
    }

    fn render(&self, agent: &str) -> Vec<RenderedBlock> {
        let view = self.view(agent);
        find(&view.agent).unwrap().render(&self.cx(), view)
    }

    /// `load_texts` for one file, with each entry's load.
    fn texts(&self, agent: &str, file: &AbsPath) -> Vec<(Load, LoadText)> {
        let view = self.view(agent);
        let texts = load_texts(find(&view.agent).unwrap(), &self.cx(), view, file);
        texts
            .into_iter()
            .map(|t| (view.loads[t.load.0 as usize].clone(), t))
            .collect()
    }

    fn agreement(&self) {
        for view in &self.scanned.snapshot.agents {
            let agent = view.agent.as_str();
            let rendered = self.render(agent);
            assert_eq!(view.session.len(), rendered.len(), "{agent}: block count");
            for (block, shown) in view.session.iter().zip(&rendered) {
                assert_eq!(block.parts.len(), shown.pieces.len(), "{agent}: parts");
                let sum: u32 = block
                    .parts
                    .iter()
                    .map(|p| match p {
                        Part::Glue { tokens } | Part::Load { tokens, .. } => *tokens,
                    })
                    .sum();
                assert_eq!(sum, block.tokens, "{agent}: a block costs its parts");
                assert_eq!(block.tokens, shown.tokens, "{agent}: scan and render count");
                let joined: String = shown.pieces.iter().map(Piece::text).collect();
                assert_eq!(joined, shown.text(), "{agent}: pieces join to the text");
                for (part, piece) in block.parts.iter().zip(&shown.pieces) {
                    match (part, piece) {
                        (Part::Glue { tokens }, Piece::Frame { text }) => {
                            assert_eq!(*tokens, tokens::count(text), "{agent}: frame count");
                        }
                        (Part::Load { load, tokens }, Piece::Load { load: id, text }) => {
                            assert_eq!(load, id, "{agent}: same load in the same place");
                            assert_eq!(*tokens, tokens::count(text), "{agent}: piece count");
                            let file = &view.loads[load.0 as usize].file;
                            let got = self.texts(agent, file);
                            let (_, entry) = got.iter().find(|(l, _)| &l.id == load).unwrap();
                            assert_eq!(
                                entry.text.as_deref(),
                                Some(text.as_str()),
                                "{agent}: load_texts returns the piece of {file}"
                            );
                        }
                        other => panic!("{agent}: part and piece differ: {other:?}"),
                    }
                }
            }
            let files: std::collections::BTreeSet<&AbsPath> =
                view.loads.iter().map(|l| &l.file).collect();
            for file in files {
                for (load, entry) in self.texts(agent, file) {
                    match (&load.delivered, &entry.text) {
                        (Some(d), Some(text)) => assert_eq!(
                            d.tokens,
                            tokens::count(text),
                            "{agent}: the scan counts the text load_texts returns for {file} ({:?})",
                            load.kind
                        ),
                        (None, None) => {}
                        (d, t) => panic!("{agent}: {file} delivered {d:?} but text {t:?}"),
                    }
                }
            }
        }
    }
}

const SKILL: &str =
    "---\nname: demo\ndescription: Shows a demo skill\n---\n\nStep one.\nStep two.\nStep three.\n";

/// The owner's setup: a project `CLAUDE.md` imports a skill file with `@`.
fn imported_skill() -> (Fixture, AbsPath) {
    let fx = Fixture::new();
    fx.write(
        &fx.project_path("CLAUDE.md"),
        "@.claude/skills/demo/SKILL.md\n",
    );
    let skill = fx.project_path(".claude/skills/demo/SKILL.md");
    fx.write(&skill, SKILL);
    (fx, skill)
}

#[test]
fn each_place_a_file_loads_has_its_own_text() {
    // Claude spec 1.5 (an import is its own block after its host), 1.8 (frontmatter removed),
    // and 4 (one listing line, the body on invoke).
    let (fx, skill) = imported_skill();
    let run = Run::new(fx.env());
    let texts = run.texts("claude", &skill);
    let of = |pred: &dyn Fn(&Load) -> bool| {
        texts
            .iter()
            .find(|(l, _)| pred(l))
            .map(|(_, t)| t.text.clone().unwrap())
            .unwrap()
    };
    let import = of(&|l| matches!(l.via, Via::Import { .. }));
    let listing = of(&|l| l.kind == DocKind::Listing);
    let body = of(&|l| l.kind == DocKind::SkillBody);
    assert_eq!(texts.len(), 3);
    assert_eq!(import, "Step one.\nStep two.\nStep three.");
    assert_eq!(listing, "demo: Shows a demo skill");
    assert_eq!(body, "\nStep one.\nStep two.\nStep three.\n");
    assert!(texts.iter().all(|(_, t)| !t.whole_file));

    // The import's piece sits in the instruction block, the listing line in the skill listing.
    let rendered = run.render("claude");
    let piece_of = |label: BlockLabel| {
        let block = rendered.iter().find(|b| b.label == label).unwrap();
        block
            .pieces
            .iter()
            .filter_map(|p| match p {
                Piece::Load { load, text }
                    if run.view("claude").loads[load.0 as usize].file == skill =>
                {
                    Some(text.clone())
                }
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(piece_of(BlockLabel::Instructions), [import]);
    assert_eq!(piece_of(BlockLabel::SkillCatalog), [listing]);
}

#[test]
fn the_doctor_reports_a_listed_body_that_an_import_also_loads() {
    let (fx, skill) = imported_skill();
    let run = Run::new(fx.env());
    let view = run.view("claude");
    let import = view
        .loads
        .iter()
        .find(|l| l.file == skill && matches!(l.via, Via::Import { .. }))
        .unwrap();
    let found: Vec<&Diagnostic> = run
        .scanned
        .snapshot
        .diagnostics
        .iter()
        .filter(|d| matches!(d.problem, Problem::BodyAlsoImported { .. }))
        .collect();
    assert_eq!(found.len(), 1);
    assert_eq!(
        found[0].at,
        Subject::File {
            path: skill.clone(),
            line: None
        }
    );
    assert_eq!(
        found[0].problem,
        Problem::BodyAlsoImported {
            import: import.id.clone(),
            host: fx.project_path("CLAUDE.md"),
            line: 1,
        }
    );

    // Without the import there is nothing to report.
    fx.write(&fx.project_path("CLAUDE.md"), "no import here\n");
    let run = Run::new(fx.env());
    assert!(
        !run.scanned
            .snapshot
            .diagnostics
            .iter()
            .any(|d| matches!(d.problem, Problem::BodyAlsoImported { .. }))
    );
}

#[test]
fn a_nested_instruction_file_sends_its_text_on_access() {
    // Claude spec 1.3 and 1.8: a file below cwd loads on access, without its block comments.
    let fx = Fixture::new();
    let nested = fx.project_path("sub/CLAUDE.md");
    fx.write(&nested, "<!-- for people only -->\nnested rules\n");
    let rule = fx.project_path(".claude/rules/api.md");
    fx.write(&rule, "---\npaths: [\"src/**\"]\n---\nUse the client.\n");
    let run = Run::new(fx.env());
    let [(load, text)] = run.texts("claude", &nested).try_into().unwrap();
    assert!(matches!(load.when, Activation::OnAccess { .. }));
    assert_eq!(text.text.as_deref(), Some("nested rules"));
    let [(load, text)] = run.texts("claude", &rule).try_into().unwrap();
    assert!(matches!(load.when, Activation::OnAccess { .. }));
    assert_eq!(text.text.as_deref(), Some("Use the client."));
}

#[test]
fn a_cut_memory_index_sends_the_part_that_is_kept() {
    // Claude spec 2.2: the index loads up to 200 lines.
    let fx = Fixture::new();
    let first = Run::new(fx.env());
    let index = first.view("claude").memory[0].index.clone().unwrap();
    let lines: Vec<String> = (1..=250).map(|n| format!("- line {n}")).collect();
    fx.write(&index, &lines.join("\n"));
    let run = Run::new(fx.env());
    let [(load, text)] = run.texts("claude", &index).try_into().unwrap();
    assert!(matches!(load.status, LoadStatus::Truncated { .. }));
    let text = text.text.unwrap();
    assert!(text.starts_with("- line 1\n"));
    assert!(text.contains("- line 200"));
    assert!(!text.contains("- line 201\n"));
}

#[test]
fn a_skipped_load_sends_nothing() {
    let fx = Fixture::new();
    fx.write(&fx.project_path("CLAUDE.md"), "@missing.md\nrules\n");
    let run = Run::new(fx.env());
    let [(load, text)] = run
        .texts("claude", &fx.project_path("missing.md"))
        .try_into()
        .unwrap();
    assert!(matches!(load.status, LoadStatus::Skipped { .. }));
    assert_eq!(text.text, None);
}

#[test]
fn a_codex_agents_md_is_its_text_without_the_frame() {
    // Codex spec 1.4: `codex debug prompt-input` frames the files with a heading and an
    // `<INSTRUCTIONS>` element.
    let fx = Fixture::new();
    fx.write(&fx.project_path(".git/HEAD"), "ref: refs/heads/main\n");
    let codex_home = fx.home_path(".codex");
    fx.write(&codex_home.join("config.toml"), "");
    let agents = fx.project_path("AGENTS.md");
    fx.write(&agents, "Run the tests first.\n");
    let env = fx.env().with_var("CODEX_HOME", codex_home.as_str());
    let run = Run::new(env);
    let [(_, text)] = run.texts("codex", &agents).try_into().unwrap();
    assert_eq!(text.text.as_deref(), Some("Run the tests first.\n"));
    assert!(text.whole_file);
    let block = run.render("codex").into_iter().next().unwrap().text();
    assert!(block.starts_with("# AGENTS.md instructions for "));
    assert!(block.contains("<INSTRUCTIONS>\nRun the tests first.\n\n</INSTRUCTIONS>"));
}

#[test]
fn a_gemini_import_sends_its_text_inside_the_host() {
    // Gemini spec 3: an import is inlined into its host's text.
    let fx = Fixture::new();
    fx.write(&fx.project_path(".git/HEAD"), "ref: refs/heads/main\n");
    fx.mkdir(&fx.home_path(".gemini"));
    let host = fx.project_path("GEMINI.md");
    fx.write(&host, "Top.\n@./part.md\nBottom.\n");
    let part = fx.project_path("part.md");
    fx.write(&part, "The part.\n");
    fx.write(
        &fx.home_path(".gemini/trustedFolders.json"),
        &format!("{{\"{}\": \"TRUST_FOLDER\"}}", fx.project()),
    );
    let run = Run::new(fx.env());
    let [(_, imported)] = run.texts("gemini", &part).try_into().unwrap();
    let imported = imported.text.unwrap();
    assert_eq!(imported, "The part.\n");
    let [(_, host_text)] = run.texts("gemini", &host).try_into().unwrap();
    assert!(host_text.text.unwrap().contains(&imported));
}
