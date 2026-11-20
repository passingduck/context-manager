//! A scan runs as soon as the operator opens a project, so every file a repository commits is
//! input from a stranger. A small committed file must not make a scan hang or take gigabytes
//! (design section 8, "resource exhaustion by repository content").

use std::io::Write;
use std::sync::{Mutex, mpsc};
use std::time::Duration;

use cm_core::env::ScanEnv;
use cm_core::scan::{Scanned, scan_full};
use cm_core::testing::Fixture;
use cm_core::*;

/// One path per adapter reader, with the files that make the adapter read it.
const READ_PATHS: &[(&str, &[(&str, &str)])] = &[
    ("GEMINI.md", &[]),
    (".claude/settings.json", &[]),
    (".gemini/settings.json", &[]),
    (".codex/config.toml", &[]),
    ("CLAUDE.md", &[]),
    (".codex/hooks.json", &[]),
    (
        ".agents/skills/s/agents/openai.yaml",
        &[(
            ".agents/skills/s/SKILL.md",
            "---\nname: s\ndescription: d\n---\n",
        )],
    ),
    (
        "gemini-import.md",
        &[("GEMINI.md", "@./gemini-import.md\n")],
    ),
    (
        "docs/claude-import.md",
        &[("CLAUDE.md", "@docs/claude-import.md\n")],
    ),
];

/// A project all three agents read: a git root, the three agent directories, and a Gemini trust
/// record for the folder, as in the reviewer's probe.
fn project(files: &[(&str, &str)]) -> Fixture {
    let fx = Fixture::new();
    for dir in [".git", ".claude", ".gemini", ".codex"] {
        fx.mkdir(&fx.project_path(dir));
    }
    fx.write(
        &fx.home_path(".gemini/trustedFolders.json"),
        &format!("{{\"{}\": \"TRUST_FOLDER\"}}", fx.project()),
    );
    for (rel, text) in files {
        fx.write(&fx.project_path(rel), text);
    }
    fx
}

/// Runs the scan on a worker thread and waits a second for it.
fn scan_within_a_second(env: ScanEnv, case: &str) -> Scanned {
    scan_within(env, case, 1)
}

/// One scan at a time, so that the wait measures one scan and not the other tests' scans.
static ONE_SCAN: Mutex<()> = Mutex::new(());

/// Runs the scan on a worker thread and waits `seconds` for it.
fn scan_within(env: ScanEnv, case: &str, seconds: u64) -> Scanned {
    // The tokenizer loads once per process. Load it first so the wait measures the scan.
    tokens::count("warm up");
    let _one = ONE_SCAN
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(scan_full(&env));
    });
    match rx.recv_timeout(Duration::from_secs(seconds)) {
        Ok(scanned) => scanned,
        Err(_) => {
            // A stuck scan keeps allocating, so the process ends here instead of leaving it to
            // grow while the other tests run. Writing to stderr directly keeps the message, which
            // the test harness would otherwise hold back.
            let _ = writeln!(
                std::io::stderr(),
                "{case}: the scan did not return within {seconds} s"
            );
            std::process::exit(101);
        }
    }
}

fn unreadable_not_regular() -> FileState {
    FileState::Unreadable {
        error: "not a regular file".to_owned(),
    }
}

fn state_of<'a>(scanned: &'a Scanned, path: &AbsPath) -> Option<&'a FileState> {
    scanned.snapshot.files.get(path).map(|entry| &entry.state)
}

#[test]
fn a_link_to_a_device_at_a_read_path_is_refused_without_reading() {
    for (rel, files) in READ_PATHS {
        let fx = project(files);
        let path = fx.project_path(rel);
        fx.symlink(&path, &AbsPath::parse("/dev/zero").unwrap());
        let scanned = scan_within_a_second(fx.env(), &format!("a link to /dev/zero at {rel}"));
        assert_eq!(
            state_of(&scanned, &path),
            Some(&unreadable_not_regular()),
            "a link to /dev/zero at {rel} carries the reason for a file that is not regular"
        );
    }
}

#[test]
fn a_named_pipe_at_a_read_path_is_refused_without_opening() {
    for (rel, files) in READ_PATHS {
        let fx = project(files);
        let path = fx.project_path(rel);
        fx.mkdir(&path.parent().unwrap());
        let made = std::process::Command::new("mkfifo")
            .arg(path.as_str())
            .status()
            .expect("run mkfifo");
        assert!(made.success(), "mkfifo {path}");
        let scanned = scan_within_a_second(fx.env(), &format!("a named pipe at {rel}"));
        assert_eq!(
            state_of(&scanned, &path),
            Some(&unreadable_not_regular()),
            "a named pipe at {rel} carries the reason for a file that is not regular"
        );
    }
}

#[test]
fn a_settings_file_over_8_mib_is_listed_and_not_read() {
    let size = 9 * 1024 * 1024;
    let head = r#"{"hooks":{"PreToolUse":[{"matcher":"","hooks":[{"type":"command","command":"echo big"}]}]},"pad":""#;
    let text = format!("{head}{}\"}}", "x".repeat(size - head.len() - 2));
    let fx = project(&[(".claude/settings.json", &text)]);
    let path = fx.project_path(".claude/settings.json");
    let scanned = scan_within_a_second(fx.env(), "a 9 MiB settings file");

    assert_eq!(
        state_of(&scanned, &path),
        Some(&FileState::TooBig { bytes: size as u64 }),
        "a 9 MiB settings file carries the too-large reason"
    );
    let claude = scanned
        .snapshot
        .agents
        .iter()
        .find(|view| view.agent.as_str() == "claude")
        .unwrap();
    assert!(
        claude.hooks.iter().all(|hook| hook.source.file != path),
        "the hook inside a 9 MiB settings file shows that the adapter read it"
    );
}

/// The reviewer's fan-out: `<root>.md` and `a.md` to `d.md` each hold `k` lines that import the
/// next file, and `e.md` is a leaf.
fn fan_out(fx: &Fixture, root: &str, k: usize) {
    let names = [root, "a", "b", "c", "d", "e"];
    for pair in names.windows(2) {
        let text: String = (0..k)
            .map(|line| format!("line {line} @./{}.md\n", pair[1]))
            .collect();
        fx.write(&fx.project_path(&format!("{}.md", pair[0])), &text);
    }
    fx.write(&fx.project_path("e.md"), "leaf\n");
}

fn view<'a>(scanned: &'a Scanned, agent: &str) -> &'a AgentView {
    scanned
        .snapshot
        .agents
        .iter()
        .find(|view| view.agent.as_str() == agent)
        .unwrap()
}

#[test]
fn gemini_imports_that_multiply_stop_at_the_node_budget() {
    let fx = project(&[]);
    fan_out(&fx, "GEMINI", 10);
    let scanned = scan_within_a_second(fx.env(), "Gemini imports that multiply, k = 10");

    let root = fx.project_path("GEMINI.md");
    let budget: Vec<_> = scanned
        .snapshot
        .diagnostics
        .iter()
        .filter(|d| matches!(d.problem, Problem::ImportBudgetExhausted { .. }))
        .map(|d| (&d.at, &d.problem))
        .collect();
    assert_eq!(
        budget,
        [(
            &Subject::File {
                path: root.clone(),
                line: None
            },
            &Problem::ImportBudgetExhausted { max: 1_000 }
        )],
        "one diagnostic names GEMINI.md and the budget of 1,000 imports"
    );
    let gemini = view(&scanned, "gemini");
    let host = gemini
        .loads
        .iter()
        .find(|load| load.file == root && matches!(load.via, Via::Discovery { .. }))
        .unwrap();
    let block = gemini
        .session
        .iter()
        .find(|block| {
            block
                .parts
                .iter()
                .any(|part| matches!(part, Part::Load { load, .. } if *load == host.id))
        })
        .unwrap();
    assert_eq!(
        block.fidelity,
        Fidelity::Approximate,
        "the block that carries GEMINI.md is marked as not exact"
    );
}

#[test]
fn claude_imports_of_the_same_shape_stay_linear() {
    for k in [10, 40] {
        let fx = project(&[]);
        fan_out(&fx, "CLAUDE", k);
        let scanned = scan_within_a_second(
            fx.env(),
            &format!("Claude imports of the fan-out shape, k = {k}"),
        );
        let imports = view(&scanned, "claude")
            .loads
            .iter()
            .filter(|load| matches!(load.via, Via::Import { .. }))
            .count();
        // Claude loads a file once per session, so every token is one load: k in each of the
        // four files that load, and k too deep in the fifth.
        assert_eq!(imports, 5 * k, "Claude import loads for k = {k}");
    }
}

/// One listed directory of each agent: the directory, the agent that lists it, and the one item
/// in it.
const LISTED_DIRS: &[(&str, &str, &str, &str)] = &[
    (".claude/commands", "claude", "c.md", "hi\n"),
    (
        ".codex/agents",
        "codex",
        "c.toml",
        "name = \"c\"\ndescription = \"d\"\ndeveloper_instructions = \"p\"\n",
    ),
    (".gemini/commands", "gemini", "c.toml", "prompt = \"p\"\n"),
    (
        ".claude/skills",
        "claude",
        "s/SKILL.md",
        "---\nname: s\ndescription: d\n---\n",
    ),
    (
        ".agents/skills",
        "codex",
        "s/SKILL.md",
        "---\nname: s\ndescription: d\n---\n",
    ),
    (
        ".gemini/skills",
        "gemini",
        "s/SKILL.md",
        "---\nname: s\ndescription: d\n---\n",
    ),
];

#[test]
fn links_that_loop_in_a_listed_directory_list_it_once() {
    for (dir, agent, item, text) in LISTED_DIRS {
        let fx = project(&[(&format!("{dir}/{item}"), text)]);
        fx.write(
            &fx.home_path(".codex/config.toml"),
            &format!(
                "[projects.\"{}\"]\ntrust_level = \"trusted\"\n",
                fx.project()
            ),
        );
        for n in 0..6 {
            fx.symlink(
                &fx.project_path(&format!("{dir}/l{n}")),
                &fx.project_path(dir),
            );
        }
        let scanned = scan_within(fx.env(), &format!("six links to . in {dir}"), 3);
        let under = fx.project_path(dir);
        let items: Vec<&AbsPath> = view(&scanned, agent)
            .extensions
            .iter()
            .map(|ext| &ext.file)
            .filter(|file| file.starts_with(&under))
            .collect();
        assert_eq!(
            items,
            [&fx.project_path(&format!("{dir}/{item}"))],
            "{agent} lists the one item of {dir} once"
        );
    }
}

#[test]
fn a_skills_directory_of_50_000_entries_is_listed_to_the_entry_budget() {
    let fx = project(&[]);
    let skills = fx.project_path(".claude/skills");
    for n in 0..50_000 {
        let dir = skills.join(&format!("s{n:05}"));
        std::fs::create_dir_all(dir.as_path()).unwrap();
        std::fs::write(
            dir.join("SKILL.md").as_path(),
            format!("---\nname: s{n:05}\ndescription: d\n---\n"),
        )
        .unwrap();
    }
    let scanned = scan_within(fx.env(), "a skills directory of 50,000 entries", 3);
    let listed = view(&scanned, "claude")
        .extensions
        .iter()
        .filter(|ext| ext.file.starts_with(&skills))
        .count();
    assert_eq!(listed, 5_000, "Claude lists the first 5,000 skills");
    let capped: Vec<_> = scanned
        .snapshot
        .diagnostics
        .iter()
        .filter(|d| matches!(d.problem, Problem::ListingCapped { .. }))
        .map(|d| (&d.at, &d.problem))
        .collect();
    assert_eq!(
        capped,
        [(
            &Subject::File {
                path: skills.clone(),
                line: None
            },
            &Problem::ListingCapped { max: 5_000 }
        )],
        "one diagnostic names the skills directory and the budget of 5,000 entries"
    );
}

const MIB: usize = 1024 * 1024;

/// `bytes` of plain text in lines, with no token an agent follows.
fn plain_text(bytes: usize) -> String {
    let line = "plain words in a line of text\n";
    let mut text = line.repeat(bytes / line.len() + 1);
    text.truncate(bytes);
    text
}

fn problems(scanned: &Scanned, pick: fn(&Problem) -> bool) -> Vec<(&Subject, &Problem)> {
    scanned
        .snapshot
        .diagnostics
        .iter()
        .filter(|d| pick(&d.problem))
        .map(|d| (&d.at, &d.problem))
        .collect()
}

#[test]
fn a_2_mib_claude_md_is_counted_from_its_first_mib_and_says_so() {
    let text = plain_text(2 * MIB);
    let fx = project(&[("CLAUDE.md", &text)]);
    let scanned = scan_within(fx.env(), "a 2 MiB CLAUDE.md", 3);

    let root = fx.project_path("CLAUDE.md");
    assert_eq!(
        problems(&scanned, |p| matches!(
            p,
            Problem::TextPartlyAnalyzed { .. }
        )),
        [(
            &Subject::File {
                path: root.clone(),
                line: None
            },
            &Problem::TextPartlyAnalyzed { max: MIB as u64 }
        )],
        "one diagnostic names CLAUDE.md and the analysis bound of 1 MiB"
    );
    let claude = view(&scanned, "claude");
    let load = claude.loads.iter().find(|load| load.file == root).unwrap();
    // The text is twice its first MiB, so the estimate is twice that count.
    let expected = 2 * tokens::count(&text[..MIB]);
    let delivered = load.delivered.as_ref().unwrap().tokens;
    assert!(
        delivered.abs_diff(expected) <= expected / 100,
        "Claude counts CLAUDE.md as twice its first MiB: {delivered} against {expected}"
    );
    let block = claude
        .session
        .iter()
        .find(|block| {
            block
                .parts
                .iter()
                .any(|part| matches!(part, Part::Load { load: id, .. } if *id == load.id))
        })
        .unwrap();
    assert_eq!(
        block.fidelity,
        Fidelity::Approximate,
        "the block that carries CLAUDE.md is marked as not exact"
    );
}

#[test]
fn ten_links_to_one_8_mib_command_stop_at_the_scan_read_budget() {
    let fx = project(&[("big.md", &plain_text(8 * MIB))]);
    let names: Vec<AbsPath> = (0..10)
        .map(|n| fx.project_path(&format!(".claude/commands/c{n}.md")))
        .collect();
    for name in &names {
        fx.symlink(name, &fx.project_path("big.md"));
    }
    let scanned = scan_within(fx.env(), "ten links to one 8 MiB command", 3);

    let max = 64 * MIB as u64;
    assert_eq!(
        problems(&scanned, |p| matches!(
            p,
            Problem::ReadBudgetExhausted { .. }
        )),
        [(&Subject::Agent, &Problem::ReadBudgetExhausted { max })],
        "one diagnostic names the scan read budget of 64 MiB"
    );
    let refused = FileState::Unreadable {
        error: "the scan read budget is spent".to_owned(),
    };
    let states: Vec<bool> = names
        .iter()
        .map(|name| match state_of(&scanned, name) {
            Some(FileState::Present { .. }) => true,
            Some(state) if *state == refused => false,
            other => panic!("{name} has {other:?}"),
        })
        .collect();
    // Seven or eight 8 MiB reads fit in 64 MiB, after the few small files the scan reads first.
    let read = states.iter().filter(|read| **read).count();
    assert!(
        (7..=8).contains(&read) && states.iter().skip(read).all(|read| !read),
        "the first links are read and every later one carries the budget reason: {states:?}"
    );
    assert!(
        scanned.fs().bytes_read() <= max,
        "the scan read at most 64 MiB"
    );
}

/// `GEMINI.md` or `CLAUDE.md` with `lines` lines, each made by `line` from its number.
fn root_file(fx: &Fixture, name: &str, lines: usize, line: impl Fn(usize) -> String) {
    let text: String = (0..lines).map(|n| line(n) + "\n").collect();
    fx.write(&fx.project_path(name), &text);
}

/// `count` links `l<n>.md` in the project, each to `target`.
fn links_to(fx: &Fixture, target: &str, count: usize) {
    for n in 0..count {
        fx.symlink(
            &fx.project_path(&format!("l{n}.md")),
            &fx.project_path(target),
        );
    }
}

fn at_file(path: &AbsPath) -> Subject {
    Subject::File {
        path: path.clone(),
        line: None,
    }
}

#[test]
fn a_gemini_md_that_imports_one_1_mib_file_on_1_000_lines_stops_at_the_byte_budget() {
    let fx = project(&[("big.md", &plain_text(MIB))]);
    root_file(&fx, "GEMINI.md", 1_000, |_| "@./big.md".to_owned());
    let scanned = scan_within(
        fx.env(),
        "GEMINI.md that imports a 1 MiB file 1,000 times",
        3,
    );

    let root = fx.project_path("GEMINI.md");
    assert_eq!(
        problems(&scanned, |p| matches!(
            p,
            Problem::ImportBytesExhausted { .. }
        )),
        [(
            &at_file(&root),
            &Problem::ImportBytesExhausted {
                max: 4 * MIB as u64
            }
        )],
        "one diagnostic names GEMINI.md and the expansion budget of 4 MiB"
    );
    // GEMINI.md is 10 kB, so three imports of 1 MiB fit in 4 MiB and the fourth does not.
    let big = fx.project_path("big.md");
    let imports = view(&scanned, "gemini")
        .loads
        .iter()
        .filter(|load| load.file == big)
        .count();
    assert_eq!(imports, 3, "Gemini loads big.md three times");
}

#[test]
fn a_gemini_md_that_imports_1_000_links_to_one_8_mib_file_stops_at_the_byte_budget() {
    let fx = project(&[("big.md", &plain_text(8 * MIB))]);
    links_to(&fx, "big.md", 1_000);
    root_file(&fx, "GEMINI.md", 1_000, |n| format!("@./l{n}.md"));
    let scanned = scan_within(
        fx.env(),
        "GEMINI.md that imports 1,000 links to an 8 MiB file",
        3,
    );

    let root = fx.project_path("GEMINI.md");
    assert_eq!(
        problems(&scanned, |p| matches!(
            p,
            Problem::ImportBytesExhausted { .. }
        )),
        [(
            &at_file(&root),
            &Problem::ImportBytesExhausted {
                max: 4 * MIB as u64
            }
        )],
        "one diagnostic names GEMINI.md and the expansion budget of 4 MiB"
    );
}

#[test]
fn a_claude_md_that_imports_500_links_to_one_8_mib_file_stops_at_the_byte_budget() {
    let fx = project(&[("big.md", &plain_text(8 * MIB))]);
    links_to(&fx, "big.md", 500);
    root_file(&fx, "CLAUDE.md", 500, |n| format!("@l{n}.md"));
    let scanned = scan_within(
        fx.env(),
        "CLAUDE.md that imports 500 links to an 8 MiB file",
        3,
    );

    let root = fx.project_path("CLAUDE.md");
    assert_eq!(
        problems(&scanned, |p| matches!(
            p,
            Problem::ImportBytesExhausted { .. }
        )),
        [(
            &at_file(&root),
            &Problem::ImportBytesExhausted {
                max: 4 * MIB as u64
            }
        )],
        "one diagnostic names CLAUDE.md and the expansion budget of 4 MiB"
    );
}

#[test]
fn a_hundred_gemini_md_files_that_import_one_1_mib_file_stop_at_the_scan_expansion_budget() {
    let fx = project(&[("big.md", &plain_text(MIB))]);
    for n in 0..100 {
        fx.write(
            &fx.project_path(&format!("d{n}/GEMINI.md")),
            "@../big.md\n@../big.md\n@../big.md\n",
        );
    }
    let scanned = scan_within(fx.env(), "100 GEMINI.md files that import a 1 MiB file", 3);

    let max = 8 * MIB as u64;
    assert_eq!(
        problems(&scanned, |p| matches!(
            p,
            Problem::ScanExpansionExhausted { .. }
        )),
        [(&Subject::Agent, &Problem::ScanExpansionExhausted { max })],
        "one diagnostic names the scan expansion budget of 8 MiB"
    );
    // Each import adds the whole 1 MiB file, so eight fit in 8 MiB.
    let big = fx.project_path("big.md");
    let imports = view(&scanned, "gemini")
        .loads
        .iter()
        .filter(|load| load.file == big && matches!(load.via, Via::Import { .. }))
        .count();
    assert_eq!(imports, 8, "Gemini expands big.md eight times in all");
}

#[test]
fn a_claude_md_that_links_500_links_to_one_8_mib_file_follows_the_file_once() {
    let fx = project(&[("big.md", &plain_text(8 * MIB))]);
    links_to(&fx, "big.md", 500);
    root_file(&fx, "CLAUDE.md", 500, |n| format!("[part {n}](l{n}.md)"));
    let scanned = scan_within(
        fx.env(),
        "CLAUDE.md that links 500 links to an 8 MiB file",
        3,
    );

    let references: Vec<&AbsPath> = view(&scanned, "claude")
        .loads
        .iter()
        .filter(|load| matches!(load.via, Via::Reference { .. }))
        .map(|load| &load.file)
        .collect();
    assert_eq!(
        references,
        [&fx.project_path("l0.md")],
        "Claude references the one file behind 500 links once"
    );
}
