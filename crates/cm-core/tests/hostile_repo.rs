//! A scan runs as soon as the operator opens a project, so every file a repository commits is
//! input from a stranger. A small committed file must not make a scan hang or take gigabytes
//! (design section 8, "resource exhaustion by repository content").

use std::io::Write;
use std::sync::mpsc;
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
    // The tokenizer loads once per process. Load it first so the wait measures the scan.
    tokens::count("warm up");
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(scan_full(&env));
    });
    match rx.recv_timeout(Duration::from_secs(1)) {
        Ok(scanned) => scanned,
        Err(_) => {
            // A scan stuck on a device keeps allocating, so the process ends here instead of
            // leaving it to grow while the other tests run. Writing to stderr directly keeps the
            // message, which the test harness would otherwise hold back.
            let _ = writeln!(
                std::io::stderr(),
                "{case}: the scan did not return within a second"
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
