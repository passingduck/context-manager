//! Oracle: the installed Claude Code writes the session records, and the trace reader reads them.
//!
//! `tools/oracle/claude/session_oracle.py --keep` runs every scenario in a sandbox under a temp
//! directory this test owns: its own `HOME` and `CLAUDE_CONFIG_DIR`, a loopback endpoint, a dummy
//! key, and a `PATH` that starts with the stub `security` the script writes. The script checks the
//! records against what the endpoint received. This test then reads each sandbox's records with
//! the reader and checks the facts of the brief, and the reader's texts against the requests
//! `session_record.py` rebuilds. Nothing here reads a real agent home.
//!
//! The test skips with a message when `claude` or `python3` is not installed.

mod trace_checks;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use cm_core::agents::claude::Claude;
use cm_core::agents::{Agent, ScanCx};
use cm_core::env::ScanEnv;
use cm_core::probe_fs::ProbeFs;
use cm_core::trace::{Fit, OpenTrace};
use cm_core::*;
use serde_json::Value;
use trace_checks::{Expect, Sent, check, text};

const SCENARIOS: [&str; 9] = [
    "oneshot",
    "bash",
    "bigbash",
    "skill",
    "mention",
    "hook",
    "subagent",
    "compact",
    "projectkey",
];

fn which(bin: &str) -> Option<PathBuf> {
    let path = std::env::var("PATH").ok()?;
    path.split(':')
        .map(|dir| Path::new(dir).join(bin))
        .find(|p| p.is_file())
        .or_else(|| {
            let home = std::env::var("HOME").ok()?;
            let local = Path::new(&home).join(".local/bin").join(bin);
            local.is_file().then_some(local)
        })
}

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn abs(path: &Path) -> AbsPath {
    AbsPath::parse(path.canonicalize().unwrap().to_str().unwrap()).unwrap()
}

/// One sandbox the script kept.
struct Sandbox {
    root: PathBuf,
}

impl Sandbox {
    fn env(&self, cwd: &Path) -> ScanEnv {
        ScanEnv::for_fixture(abs(&self.root.join("home")), abs(cwd))
            .with_var("CLAUDE_CONFIG_DIR", abs(&self.root.join("cfg")).as_str())
    }

    fn cwd(&self) -> PathBuf {
        self.root.join("work/proj")
    }

    /// Every main record under `cfg/projects`, for the identity scan and the rebuilt texts.
    fn records(&self) -> Vec<PathBuf> {
        let mut out = Vec::new();
        let projects = self.root.join("cfg/projects");
        for folder in std::fs::read_dir(projects).unwrap() {
            let folder = folder.unwrap().path();
            for file in std::fs::read_dir(&folder).unwrap() {
                let file = file.unwrap().path();
                if file.extension().is_some_and(|e| e == "jsonl") {
                    out.push(file);
                }
            }
        }
        out
    }
}

/// The requests of a record as `session_record.py` rebuilds them, one text per content block.
const REBUILD: &str = r#"
import json, sys
sys.path.insert(0, sys.argv[1])
import session_record as sr
t = sr.Transcript(sys.argv[2])
def block(b):
    ty = b.get("type")
    if ty == "text": return {"text": b.get("text", "")}
    if ty == "thinking": return {"text": b.get("thinking", "")}
    if ty == "tool_use": return {"input": b.get("input")}
    if ty == "tool_result":
        c = b.get("content")
        if isinstance(c, str): return {"text": c}
        if isinstance(c, list):
            return {"text": "\n".join(x.get("text", "") for x in c if isinstance(x, dict) and x.get("type") == "text")}
        return {"text": ""}
    return {"other": True}
out = []
for req in t.requests:
    body, missing = t.rebuild(req)
    if body is None:
        continue
    out.append({"at": req.get("timestamp"),
                "system": [b.get("text", "") for b in body.get("system", [])],
                "messages": [[block(b) for b in sr.as_blocks(m["content"])] for m in body["messages"]]})
print(json.dumps(out))
"#;

fn rebuilt(record: &Path) -> Vec<Sent> {
    let tools = repo().join("tools/oracle/claude");
    let out = Command::new("python3")
        .arg("-I")
        .arg("-c")
        .arg(REBUILD)
        .arg(&tools)
        .arg(record)
        .output()
        .expect("python3 runs");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let list: Vec<Value> = serde_json::from_slice(&out.stdout).unwrap();
    list.into_iter()
        .map(|r| Sent {
            at: r["at"].as_str().unwrap_or_default().to_owned(),
            system: r["system"]
                .as_array()
                .unwrap()
                .iter()
                .map(|s| s.as_str().unwrap().to_owned())
                .collect(),
            messages: r["messages"]
                .as_array()
                .unwrap()
                .iter()
                .map(|m| {
                    m.as_array()
                        .unwrap()
                        .iter()
                        .map(|b| match (b.get("text"), b.get("input")) {
                            (Some(t), _) => Expect::Text(t.as_str().unwrap().to_owned()),
                            (None, Some(input)) => Expect::Input(input.clone()),
                            _ => Expect::Other,
                        })
                        .collect()
                })
                .collect(),
        })
        .collect()
}

/// The values of every identity field (session spec, section 15) a record holds.
fn identity_values(record: &Path) -> Vec<String> {
    const KEYS: [&str; 9] = [
        "accountUuid",
        "organizationUuid",
        "ownerAccountUuid",
        "ownerOrganizationUuid",
        "bridgeSessionId",
        "userEmail",
        "teamName",
        "teamConfigPath",
        "preCompactOrganizations",
    ];
    fn walk(value: &Value, out: &mut Vec<String>) {
        match value {
            Value::Object(map) => {
                for (k, v) in map {
                    if KEYS.contains(&k.as_str()) {
                        let mut leaves = Vec::new();
                        strings(v, &mut leaves);
                        out.extend(leaves.into_iter().filter(|s| s.len() >= 6));
                    }
                    walk(v, out);
                }
            }
            Value::Array(items) => items.iter().for_each(|v| walk(v, out)),
            _ => {}
        }
    }
    fn strings(value: &Value, out: &mut Vec<String>) {
        match value {
            Value::String(s) => out.push(s.clone()),
            Value::Array(items) => items.iter().for_each(|v| strings(v, out)),
            Value::Object(map) => map.values().for_each(|v| strings(v, out)),
            _ => {}
        }
    }
    let mut out = Vec::new();
    for line in std::fs::read_to_string(record).unwrap().lines() {
        if let Ok(value) = serde_json::from_str::<Value>(line) {
            walk(&value, &mut out);
        }
    }
    out
}

fn open(sandbox: &Sandbox, id: &TraceId, part: Option<&str>) -> OpenTrace {
    let env = sandbox.env(&sandbox.cwd());
    let fs = ProbeFs::new();
    let data = Claude
        .trace(&ScanCx::new(&env, &fs), id, part, None)
        .expect("the session opens");
    OpenTrace::new(data)
}

/// The script's endpoint reports usage for every reply. Its numbers follow the request it
/// answers (`usage_of` in the script), and the script compares them with the reply records.
fn assert_endpoint_usage(open: &OpenTrace, what: &str) {
    for (i, step) in open.trace().steps.iter().enumerate() {
        assert!(step.usage.is_some(), "{what}: step {i} has usage");
    }
}

/// Every check that holds for every record: each step with a request is exact, its usage is the
/// endpoint's, and the shared invariants.
fn check_session(sandbox: &Sandbox, info: &TraceInfo, what: &str) -> OpenTrace {
    let open = open(sandbox, &info.id, None);
    let secrets = identity_values(Path::new(info.file.as_str()));
    let texts = check(&open, &rebuilt(Path::new(info.file.as_str())), &secrets);
    let trace = open.trace();
    let requests = trace.steps.iter().filter(|s| s.sent.is_some()).count();
    assert_eq!(
        texts, requests,
        "{what}: every request's texts were compared"
    );
    assert!(
        trace.steps.iter().any(|s| s.sent.is_some()),
        "{what}: a request"
    );
    for (i, step) in trace.steps.iter().enumerate() {
        if let Some(sent) = &step.sent {
            assert!(sent.exact, "{what}: step {i} is exact");
        }
    }
    assert_endpoint_usage(&open, what);
    open
}

fn pieces(open: &OpenTrace, step: u32) -> Vec<(TracePiece, String)> {
    open.step_pieces(step)
        .unwrap()
        .into_iter()
        .enumerate()
        .map(|(n, p)| {
            let t = text(open, step, n as u32);
            (p, t)
        })
        .collect()
}

fn all_pieces(open: &OpenTrace) -> Vec<(TracePiece, String)> {
    (0..open.trace().steps.len() as u32)
        .flat_map(|i| pieces(open, i))
        .collect()
}

fn same_file(a: &AbsPath, b: &Path) -> bool {
    Path::new(a.as_str()).canonicalize().ok() == b.canonicalize().ok()
}

#[test]
fn the_reader_reads_every_oracle_scenario_exactly() {
    let Some(_claude) = which("claude") else {
        eprintln!("SKIP session oracle: `claude` is not installed");
        return;
    };
    if which("python3").is_none() {
        eprintln!("SKIP session oracle: `python3` is not installed");
        return;
    }
    // Under the system temp directory, which Claude Code does not treat as a scratchpad, so it
    // loads the sandbox's CLAUDE.md files.
    let tmp = tempfile::tempdir().unwrap();
    let tmp_root = tmp.path().canonicalize().unwrap();
    let script = repo().join("tools/oracle/claude/session_oracle.py");
    let out = Command::new("python3")
        .arg(&script)
        .args(SCENARIOS)
        .arg("--keep")
        .env("TMPDIR", &tmp_root)
        .output()
        .expect("python3 runs");
    let stdout = String::from_utf8_lossy(&out.stdout);
    if stdout.starts_with("SKIP") {
        eprintln!("{stdout}");
        return;
    }
    for name in SCENARIOS {
        assert!(
            stdout.contains(&format!("[{name}] PASS")),
            "the oracle passes {name}:\n{stdout}\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let mut boxes: BTreeMap<&str, Sandbox> = BTreeMap::new();
    for entry in std::fs::read_dir(&tmp_root).unwrap() {
        let root = entry.unwrap().path();
        let name = root.file_name().unwrap().to_str().unwrap().to_owned();
        if let Some(scenario) = SCENARIOS
            .iter()
            .find(|s| name.starts_with(&format!("cm-session-oracle-{s}-")))
        {
            boxes.insert(scenario, Sandbox { root });
        }
    }
    assert_eq!(boxes.len(), SCENARIOS.len(), "one sandbox per scenario");

    for (name, sandbox) in &boxes {
        let env = sandbox.env(&sandbox.cwd());
        let fs = ProbeFs::new();
        let list = Claude.traces(&ScanCx::new(&env, &fs));
        assert_eq!(
            list.len(),
            sandbox.records().len(),
            "{name}: the list holds every main record"
        );
        let opened: Vec<OpenTrace> = list
            .iter()
            .map(|info| check_session(sandbox, info, name))
            .collect();
        let only = &opened[0];
        let found = |kind: PieceKind| -> Vec<(TracePiece, String)> {
            all_pieces(only)
                .into_iter()
                .filter(|(p, _)| p.kind == kind)
                .collect()
        };
        match *name {
            "oneshot" => {
                let first = pieces(only, 0);
                let instructions: Vec<&AbsPath> = first
                    .iter()
                    .filter(|(p, _)| p.kind == PieceKind::Instructions)
                    .filter_map(|(p, _)| p.file.as_ref())
                    .collect();
                for file in [
                    sandbox.root.join("cfg/CLAUDE.md"),
                    sandbox.cwd().join("CLAUDE.md"),
                ] {
                    assert!(
                        instructions.iter().any(|f| same_file(f, &file)),
                        "oneshot: an Instructions piece names {}",
                        file.display()
                    );
                }
                assert!(first.iter().any(|(p, _)| p.kind == PieceKind::Catalog));
                assert!(
                    first
                        .iter()
                        .any(|(p, _)| p.kind == PieceKind::ToolDefinition)
                );
            }
            "bash" => {
                // The first reply leads with a thinking block signed with 200 bytes, which the
                // endpoint counts as 50 thinking tokens. The second request extends the first, so
                // its sizes are split by that reply's usage.
                let split = only
                    .fits()
                    .iter()
                    .position(|f| f.fit == Fit::Split)
                    .expect("bash: a request split by the reply before it");
                let added = only.fits()[split].added.unwrap();
                let thinking: Vec<u32> = only.step_pieces(split as u32).unwrap()
                    [added.after as usize..]
                    .iter()
                    .filter(|p| p.kind == PieceKind::Thinking)
                    .map(|p| p.tokens)
                    .collect();
                assert_eq!(
                    thinking,
                    [50],
                    "bash: the thinking piece costs its thinking tokens"
                );
                let calls = found(PieceKind::ToolCall);
                let results = found(PieceKind::ToolResult);
                assert!(
                    calls
                        .iter()
                        .any(|(p, _)| p.label.as_deref() == Some("Bash"))
                );
                assert!(
                    results
                        .iter()
                        .any(|(p, t)| p.label.as_deref() == Some("Bash")
                            && t.contains("oracle-bash-output"))
                );
            }
            "bigbash" => {
                let results = found(PieceKind::ToolResult);
                let (piece, text) = results
                    .iter()
                    .find(|(p, _)| p.label.as_deref() == Some("Bash"))
                    .expect("bigbash: a Bash result");
                assert!(
                    text.starts_with("<persisted-output>"),
                    "bigbash: the stand-in"
                );
                let file = piece.file.as_ref().expect("bigbash: the saved file");
                assert!(file.as_str().contains("/tool-results/"));
                assert!(text.contains(file.as_str()) || Path::new(file.as_str()).is_file());
            }
            "skill" => {
                let bodies = found(PieceKind::ExtensionBody);
                assert!(
                    bodies
                        .iter()
                        .any(|(p, t)| p.label.as_deref() == Some("oracle-skill")
                            && t.contains("ORACLE-SKILL-BODY"))
                );
            }
            "mention" => {
                let mentions = found(PieceKind::FileMention);
                let notes = sandbox.cwd().join("notes.txt");
                assert!(
                    mentions
                        .iter()
                        .any(|(p, _)| p.file.as_ref().is_some_and(|f| same_file(f, &notes)))
                );
            }
            "hook" => {
                let hooks = found(PieceKind::HookOutput);
                assert!(
                    hooks
                        .iter()
                        .any(|(_, t)| t.contains("ORACLE-SESSIONSTART-HOOK-LINE"))
                );
            }
            "compact" => {
                let trace = only.trace();
                assert_eq!(trace.boundaries.len(), 1, "compact: one boundary");
                let after = trace.boundaries[0].before;
                let step = trace
                    .steps
                    .iter()
                    .enumerate()
                    .skip(after as usize)
                    .find(|(_, s)| s.sent.is_some())
                    .map(|(i, _)| i as u32)
                    .expect("compact: a request after the boundary");
                assert!(
                    pieces(only, step)
                        .iter()
                        .any(|(p, _)| p.kind == PieceKind::Summary)
                );
            }
            "subagent" => {
                let parts = &only.trace().parts;
                assert_eq!(parts.len(), 1, "subagent: one part");
                let part = open(sandbox, &list[0].id, Some(&parts[0].part));
                let secrets = identity_values(Path::new(list[0].file.as_str()));
                let file = PathBuf::from(part.trace().info.file.as_str());
                let requests = part
                    .trace()
                    .steps
                    .iter()
                    .filter(|s| s.sent.is_some())
                    .count();
                assert_eq!(check(&part, &rebuilt(&file), &secrets), requests);
                assert!(!part.trace().steps.is_empty());
                assert!(
                    part.trace()
                        .steps
                        .iter()
                        .all(|s| s.sent.as_ref().is_some_and(|s| s.exact))
                );
                assert_endpoint_usage(&part, "subagent part");
            }
            "projectkey" => {
                let cwds: Vec<PathBuf> = list
                    .iter()
                    .map(|t| PathBuf::from(t.cwd.as_ref().unwrap().as_str()))
                    .collect();
                for dir in [
                    sandbox.cwd().join("sub"),
                    sandbox.cwd().join(".claude/worktrees/oracle-wt"),
                ] {
                    let dir = dir.canonicalize().unwrap();
                    assert!(
                        cwds.contains(&dir),
                        "projectkey: a session from {}",
                        dir.display()
                    );
                }
            }
            other => panic!("no checks for {other}"),
        }
    }
}
