//! Oracle: the installed Claude Code is the ground truth for what loads at session start.
//!
//! Each scenario builds the fixture tree of `docs/agents/claude-code.md` (see "Fixture shape"),
//! runs `claude -p` against a local sink with a fixture `CLAUDE_CONFIG_DIR`, a fixture `HOME`
//! and a dummy API key, reads the captured request body, and compares the instruction reminder
//! in it with what the Claude adapter renders for the same tree.
//!
//! A tripwire listener stands in for every proxy variable, so an outbound request that honours
//! a proxy would be recorded there. The sink answers every request with a 400. Nothing in this
//! file reads `~/.claude`.
//!
//! The test skips with a message when `claude` or `git` is not installed.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use cm_core::agents::{ScanCx, find};
use cm_core::env::ScanEnv;
use cm_core::model::{AbsPath, AgentId};
use cm_core::probe_fs::ProbeFs;
use serde_json::Value;

const INTRO_START: &str = "Codebase and user instructions are shown below.";

// ---------------------------------------------------------------------------------------------
// The sink and the tripwire
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct Captured {
    method: String,
    path: String,
    headers: BTreeMap<String, String>,
    body: Vec<u8>,
}

/// A local HTTP listener. With `answer`, it records each request and replies 400. Without, it
/// only records that someone connected and closes.
struct Listener {
    port: u16,
    seen: Arc<Mutex<Vec<Captured>>>,
    connections: Arc<Mutex<u32>>,
    stop: Arc<AtomicBool>,
}

impl Listener {
    fn start(answer: bool) -> Listener {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        listener.set_nonblocking(true).expect("non-blocking");
        let port = listener.local_addr().expect("addr").port();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let connections = Arc::new(Mutex::new(0u32));
        let stop = Arc::new(AtomicBool::new(false));
        let (seen2, conns2, stop2) = (seen.clone(), connections.clone(), stop.clone());
        std::thread::spawn(move || {
            while !stop2.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        *conns2.lock().unwrap() += 1;
                        if answer {
                            let seen = seen2.clone();
                            std::thread::spawn(move || serve(stream, seen));
                        }
                    }
                    Err(_) => std::thread::sleep(Duration::from_millis(10)),
                }
            }
        });
        Listener {
            port,
            seen,
            connections,
            stop,
        }
    }

    fn url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    fn requests(&self) -> Vec<Captured> {
        self.seen.lock().unwrap().clone()
    }

    fn connection_count(&self) -> u32 {
        *self.connections.lock().unwrap()
    }
}

impl Drop for Listener {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

fn serve(mut stream: TcpStream, seen: Arc<Mutex<Vec<Captured>>>) {
    stream.set_nonblocking(false).ok();
    stream.set_read_timeout(Some(Duration::from_secs(30))).ok();
    let mut buf: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 65536];
    loop {
        // Read until the headers are complete.
        let head_end = loop {
            if let Some(at) = find_sub(&buf, b"\r\n\r\n") {
                break at;
            }
            match stream.read(&mut chunk) {
                Ok(0) | Err(_) => return,
                Ok(n) => buf.extend_from_slice(&chunk[..n]),
            }
        };
        let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
        let mut lines = head.split("\r\n");
        let mut first = lines.next().unwrap_or("").split(' ');
        let method = first.next().unwrap_or("").to_owned();
        let path = first.next().unwrap_or("").to_owned();
        let headers: BTreeMap<String, String> = lines
            .filter_map(|l| l.split_once(':'))
            .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_owned()))
            .collect();
        let mut rest = buf[head_end + 4..].to_vec();
        let body = if headers
            .get("transfer-encoding")
            .is_some_and(|v| v.contains("chunked"))
        {
            read_chunked(&mut stream, &mut rest)
        } else {
            let want: usize = headers
                .get("content-length")
                .and_then(|v| v.parse().ok())
                .unwrap_or(0);
            while rest.len() < want {
                match stream.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => rest.extend_from_slice(&chunk[..n]),
                }
            }
            let body = rest[..want.min(rest.len())].to_vec();
            rest.drain(..want.min(rest.len()));
            body
        };
        seen.lock().unwrap().push(Captured {
            method,
            path,
            headers,
            body,
        });
        let payload = br#"{"type":"error","error":{"type":"invalid_request_error","message":"capture sink"}}"#;
        let reply = format!(
            "HTTP/1.1 400 Bad Request\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n",
            payload.len()
        );
        if stream.write_all(reply.as_bytes()).is_err() || stream.write_all(payload).is_err() {
            return;
        }
        buf = rest;
    }
}

fn read_chunked(stream: &mut TcpStream, rest: &mut Vec<u8>) -> Vec<u8> {
    let mut body = Vec::new();
    let mut chunk = [0u8; 65536];
    loop {
        let line_end = loop {
            if let Some(at) = find_sub(rest, b"\r\n") {
                break at;
            }
            match stream.read(&mut chunk) {
                Ok(0) | Err(_) => return body,
                Ok(n) => rest.extend_from_slice(&chunk[..n]),
            }
        };
        let size = usize::from_str_radix(
            String::from_utf8_lossy(&rest[..line_end])
                .trim()
                .split(';')
                .next()
                .unwrap_or("0"),
            16,
        )
        .unwrap_or(0);
        rest.drain(..line_end + 2);
        while rest.len() < size + 2 {
            match stream.read(&mut chunk) {
                Ok(0) | Err(_) => return body,
                Ok(n) => rest.extend_from_slice(&chunk[..n]),
            }
        }
        body.extend_from_slice(&rest[..size]);
        rest.drain(..size + 2);
        if size == 0 {
            return body;
        }
    }
}

fn find_sub(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

// ---------------------------------------------------------------------------------------------
// Fixture trees (docs/agents/claude-code.md, "Fixture shape")
// ---------------------------------------------------------------------------------------------

struct Tree {
    root: PathBuf,
    _guard: tempfile::TempDir,
}

impl Tree {
    fn cfg(&self) -> PathBuf {
        self.root.join("cfg")
    }
    fn home(&self) -> PathBuf {
        self.root.join("home")
    }
    fn top(&self) -> PathBuf {
        self.root.join("top")
    }
    fn repo(&self) -> PathBuf {
        self.top().join("repo")
    }
    fn w(&self, rel: &str, text: &str) {
        let path = self.root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
}

/// A directory outside any session scratchpad (spec 1.8) and outside `~/.claude`.
fn fresh_root(name: &str) -> Tree {
    let mut base = std::env::temp_dir();
    if base.components().any(|c| c.as_os_str() == "scratchpad") {
        base = PathBuf::from("/var/tmp");
    }
    let guard = tempfile::Builder::new()
        .prefix(&format!("cm-oracle-{name}-"))
        .tempdir_in(base)
        .expect("temp dir");
    let root = guard.path().canonicalize().expect("canonical");
    Tree {
        root,
        _guard: guard,
    }
}

fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .current_dir(dir)
        .args([
            "-c",
            "user.name=oracle",
            "-c",
            "user.email=oracle@example.invalid",
        ])
        .args([
            "-c",
            "commit.gpgsign=false",
            "-c",
            "init.defaultBranch=main",
        ])
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("run git");
    assert!(status.success(), "git {args:?} failed");
}

/// The base tree of `exp.sh`.
fn build_base(t: &Tree) {
    let repo = "top/repo";
    t.w("cfg/CLAUDE.md", "MARK-USER-CLAUDE\n");
    t.w("cfg/rules/u1.md", "MARK-USER-RULE-NOPATHS\n");
    t.w(
        "cfg/rules/u2.md",
        "---\npaths: \"**/*.py\"\n---\nMARK-USER-RULE-PY\n",
    );
    t.w("top/CLAUDE.md", "MARK-TOP-ANCESTOR\n");
    t.w("top/.claude/rules/top.md", "MARK-TOP-RULE\n");
    std::fs::create_dir_all(t.repo()).unwrap();
    git(&t.repo(), &["init", "-q", "."]);
    t.w(
        &format!("{repo}/CLAUDE.md"),
        "<!-- MARK-HTML-COMMENT-SHOULD-STRIP -->\nMARK-REPO-CLAUDE\nImport chain: @docs/i1.md\nCode span: `@docs/span.md`\n```\n@docs/fenced.md\n<!-- MARK-COMMENT-IN-FENCE-KEPT -->\n```\nInline comment: <!-- @docs/comment.md --> after\nEmail-like foo@docs/email.md\nSpace escaped: @docs/with\\ space.md\nMissing: @docs/missing.md\nBinary ext: @docs/pic.png\n",
    );
    for (n, next) in [
        (1, Some(2)),
        (2, Some(3)),
        (3, Some(4)),
        (4, Some(5)),
        (5, Some(6)),
        (6, None),
    ] {
        let tail = next.map_or(String::new(), |m| format!("@i{m}.md\n"));
        t.w(
            &format!("{repo}/docs/i{n}.md"),
            &format!("MARK-I{n}\n{tail}"),
        );
    }
    t.w(
        &format!("{repo}/docs/span.md"),
        "MARK-SPAN-SHOULD-NOT-IMPORT\n",
    );
    t.w(
        &format!("{repo}/docs/fenced.md"),
        "MARK-FENCED-SHOULD-NOT-IMPORT\n",
    );
    t.w(&format!("{repo}/docs/comment.md"), "MARK-COMMENT-IMPORT\n");
    t.w(
        &format!("{repo}/docs/email.md"),
        "MARK-EMAIL-SHOULD-NOT-IMPORT\n",
    );
    t.w(&format!("{repo}/docs/with space.md"), "MARK-SPACE-IMPORT\n");
    t.w(
        &format!("{repo}/docs/pic.png"),
        "MARK-PNG-SHOULD-NOT-IMPORT\n",
    );
    t.w(
        &format!("{repo}/.claude/CLAUDE.md"),
        "MARK-REPO-DOTCLAUDE-CLAUDE\n",
    );
    t.w(&format!("{repo}/CLAUDE.local.md"), "MARK-REPO-LOCAL\n");
    t.w(&format!("{repo}/AGENTS.md"), "MARK-REPO-AGENTS\n");
    t.w(
        &format!("{repo}/.claude/rules/a.md"),
        "MARK-RULE-A-NOPATHS\n",
    );
    t.w(
        &format!("{repo}/.claude/rules/sub/b.md"),
        "MARK-RULE-SUB-B-NOPATHS\n",
    );
    t.w(
        &format!("{repo}/.claude/rules/c.md"),
        "---\npaths:\n  - \"src/**/*.ts\"\n---\nMARK-RULE-C-TS\n",
    );
    t.w(
        &format!("{repo}/.claude/rules/star.md"),
        "---\npaths: \"**\"\n---\nMARK-RULE-STAR\n",
    );
    t.w(
        &format!("{repo}/.claude/rules/notmd.txt"),
        "MARK-RULE-TXT-SHOULD-NOT-LOAD\n",
    );
    t.w(
        &format!("{repo}/.claude/rules/badyaml.md"),
        "---\npaths: [unclosed\n---\nMARK-RULE-BADYAML\n",
    );
    t.w(&format!("{repo}/app/CLAUDE.md"), "MARK-APP-CLAUDE\n");
    t.w(
        &format!("{repo}/app/.claude/rules/d.md"),
        "MARK-APP-RULE-D\n",
    );
    t.w(
        &format!("{repo}/app/nested/CLAUDE.md"),
        "MARK-NESTED-SHOULD-BE-LAZY\n",
    );
    std::fs::create_dir_all(t.repo().join("src")).unwrap();
    std::fs::create_dir_all(t.home()).unwrap();

    // Auto memory, keyed by the main repository root (spec 2.3).
    let key: String = t
        .repo()
        .to_str()
        .unwrap()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let mut memory = String::from("@docs/i1.md MARK-MEMORY-IMPORT-TEST\n");
    for i in 2..=250 {
        memory.push_str(&format!("- MARK-MEM-LINE-{i:03}\n"));
    }
    let mem_rel = format!("cfg/projects/{key}/memory");
    t.w(&format!("{mem_rel}/MEMORY.md"), &memory);
    t.w(
        &format!("{mem_rel}/topic.md"),
        "MARK-TOPIC-SHOULD-NOT-PRELOAD\n",
    );
}

#[derive(Clone, Copy, Debug)]
enum Scenario {
    Base,
    FromRepo,
    Agents,
    Excludes,
    Worktree,
    NestedWorktree,
    Bytes,
}

impl Scenario {
    fn name(self) -> &'static str {
        match self {
            Scenario::Base => "base",
            Scenario::FromRepo => "fromrepo",
            Scenario::Agents => "agents",
            Scenario::Excludes => "excludes",
            Scenario::Worktree => "worktree",
            Scenario::NestedWorktree => "wtnested",
            Scenario::Bytes => "bytes",
        }
    }
}

/// Builds the tree and returns the working directory of the session.
fn build(t: &Tree, scenario: Scenario) -> PathBuf {
    build_base(t);
    match scenario {
        Scenario::Base => t.repo().join("app"),
        Scenario::FromRepo => t.repo(),
        Scenario::Agents => {
            // Spec 1.6, RUN:agents: no CLAUDE.md of any kind in cwd or above.
            for rel in [
                "top/CLAUDE.md",
                "top/repo/CLAUDE.md",
                "top/repo/.claude/CLAUDE.md",
                "top/repo/CLAUDE.local.md",
                "top/repo/app/CLAUDE.md",
            ] {
                std::fs::remove_file(t.root.join(rel)).unwrap();
            }
            t.w("top/repo/.claude/AGENTS.md", "MARK-REPO-DOTCLAUDE-AGENTS\n");
            t.w("top/repo/app/AGENTS.md", "MARK-APP-AGENTS\n");
            t.repo().join("app")
        }
        Scenario::Excludes => {
            t.w(
                "cfg/settings.json",
                "{\"claudeMdExcludes\":[\"**/top/CLAUDE.md\",\"**/.claude/rules/sub/**\"]}\n",
            );
            t.repo().join("app")
        }
        Scenario::Worktree => {
            git(&t.repo(), &["add", "-A"]);
            git(&t.repo(), &["commit", "-q", "-m", "init"]);
            git(&t.repo(), &["worktree", "add", "-q", "../wt"]);
            t.w("top/wt/CLAUDE.local.md", "MARK-WT-LOCAL\n");
            t.top().join("wt/app")
        }
        Scenario::NestedWorktree => {
            git(&t.repo(), &["add", "-A"]);
            git(&t.repo(), &["commit", "-q", "-m", "init"]);
            git(
                &t.repo(),
                &["worktree", "add", "-q", ".claude/worktrees/wt"],
            );
            t.repo().join(".claude/worktrees/wt/app")
        }
        Scenario::Bytes => {
            // Spec 2.4, RUN:bytes: 150 lines of 298 bytes each.
            let key: String = t
                .repo()
                .to_str()
                .unwrap()
                .chars()
                .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
                .collect();
            let mut memory = String::new();
            for i in 1..=150 {
                memory.push_str(&format!("- MARK-BYTES-{i:03} {}\n", "x".repeat(298 - 18)));
            }
            t.w(&format!("cfg/projects/{key}/memory/MEMORY.md"), &memory);
            t.repo().join("app")
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Running Claude Code
// ---------------------------------------------------------------------------------------------

fn which(bin: &str) -> Option<PathBuf> {
    let path = std::env::var("PATH").ok()?;
    path.split(':')
        .map(|dir| Path::new(dir).join(bin))
        .find(|p| p.is_file())
        .or_else(|| {
            let home = std::env::var("HOME").ok()?;
            let p = Path::new(&home).join(".local/bin").join(bin);
            p.is_file().then_some(p)
        })
}

struct Run {
    reminder: String,
    requests: Vec<Captured>,
    tripwire_connections: u32,
}

/// Runs `claude -p` in `cwd` against a fresh sink. `home` and `cfg` come from the tree.
fn run_claude(claude: &Path, t: &Tree, cwd: &Path) -> Run {
    let sink = Listener::start(true);
    let tripwire = Listener::start(false);
    let status = Command::new("perl")
        .args(["-e", "alarm 120; exec @ARGV"])
        .arg(claude)
        .args(["-p", "say hi", "--debug-file"])
        .arg(t.root.join("debug.log"))
        .current_dir(cwd)
        .env_clear()
        .env("HOME", t.home())
        .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
        .env("TERM", "dumb")
        .env("CLAUDE_CONFIG_DIR", t.cfg())
        .env("ANTHROPIC_API_KEY", "sk-ant-dummy-capture")
        .env("ANTHROPIC_BASE_URL", sink.url())
        .env("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC", "1")
        .env("DISABLE_AUTOUPDATER", "1")
        // Anything that honours a proxy lands on the tripwire, never on the network.
        .env("HTTP_PROXY", tripwire.url())
        .env("HTTPS_PROXY", tripwire.url())
        .env("http_proxy", tripwire.url())
        .env("https_proxy", tripwire.url())
        .env("ALL_PROXY", tripwire.url())
        .env("NO_PROXY", "127.0.0.1,localhost")
        .env("no_proxy", "127.0.0.1,localhost")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("spawn claude");
    let _ = status;
    // The request may arrive a moment after the process exits.
    let deadline = Instant::now() + Duration::from_secs(3);
    while sink.requests().is_empty() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    let requests = sink.requests();
    let reminder = requests
        .iter()
        .filter_map(|r| serde_json::from_slice::<Value>(&r.body).ok())
        .find_map(|body| first_user_reminder(&body))
        .unwrap_or_default();
    Run {
        reminder,
        requests,
        tripwire_connections: tripwire.connection_count(),
    }
}

fn first_user_reminder(body: &Value) -> Option<String> {
    let first = body.get("messages")?.as_array()?.first()?;
    let content = first.get("content")?;
    let texts: Vec<&str> = match content {
        Value::String(s) => vec![s.as_str()],
        Value::Array(blocks) => blocks
            .iter()
            .filter_map(|b| b.get("text").and_then(Value::as_str))
            .collect(),
        _ => Vec::new(),
    };
    texts
        .into_iter()
        .find(|t| t.contains(INTRO_START))
        .map(str::to_owned)
}

// ---------------------------------------------------------------------------------------------
// Our side, and the comparison
// ---------------------------------------------------------------------------------------------

fn ours(t: &Tree, cwd: &Path) -> String {
    let abs = |p: &Path| AbsPath::parse(p.to_str().unwrap()).unwrap();
    let env = ScanEnv::for_fixture(abs(&t.home()), abs(cwd))
        .with_var("CLAUDE_CONFIG_DIR", t.cfg().to_str().unwrap());
    let fs = ProbeFs::new();
    let cx = ScanCx {
        env: &env,
        fs: &fs,
        parked_dir: None,
    };
    let agent = find(&AgentId::parse("claude").unwrap()).unwrap();
    let scan = agent.scan(&cx);
    let blocks = agent.render(&cx, &scan.view);
    blocks
        .iter()
        .map(|b| b.text.as_str())
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// Splits a reminder at its `Contents of` headers. Within one rules directory the real order
/// is the directory listing order, which is not sorted (spec 1.2), so each run of rules of one
/// directory is sorted before comparing.
fn pieces(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut rest = text;
    let marker = "\n\nContents of ";
    while let Some(at) = rest.find(marker) {
        out.push(rest[..at].to_owned());
        rest = &rest[at + 2..];
    }
    out.push(rest.to_owned());
    let key = |piece: &str| -> Option<String> {
        let path = piece.strip_prefix("Contents of ")?.split(" (").next()?;
        let at = path.find("/rules/")?;
        Some(path[..at + 7].to_owned())
    };
    let mut normalized: Vec<String> = Vec::new();
    let mut i = 0;
    while i < out.len() {
        match key(&out[i]) {
            Some(k) => {
                let mut run = vec![out[i].clone()];
                let mut j = i + 1;
                while j < out.len() && key(&out[j]).as_deref() == Some(&k) {
                    run.push(out[j].clone());
                    j += 1;
                }
                run.sort();
                normalized.extend(run);
                i = j;
            }
            None => {
                normalized.push(out[i].clone());
                i += 1;
            }
        }
    }
    normalized
}

fn check(scenario: Scenario) {
    let Some(claude) = which("claude") else {
        eprintln!("SKIP {}: `claude` is not installed", scenario.name());
        return;
    };
    if which("git").is_none() {
        eprintln!("SKIP {}: `git` is not installed", scenario.name());
        return;
    }
    let t = fresh_root(scenario.name());
    let cwd = build(&t, scenario);
    let run = run_claude(&claude, &t, &cwd);

    // No request left the machine: the sink saw every call, and nothing reached the tripwire.
    assert_eq!(
        run.tripwire_connections, 0,
        "a connection reached the proxy tripwire"
    );
    assert!(!run.requests.is_empty(), "the sink saw no request");
    for r in &run.requests {
        assert_eq!(r.method, "POST", "unexpected {} {}", r.method, r.path);
        assert!(
            r.path.starts_with("/v1/messages"),
            "unexpected path {}",
            r.path
        );
        let key = r.headers.get("x-api-key").cloned().unwrap_or_default();
        assert_eq!(
            key, "sk-ant-dummy-capture",
            "the dummy key was not the one sent"
        );
    }
    assert!(
        !run.reminder.is_empty(),
        "no instruction reminder in the captured requests"
    );

    let want = pieces(&run.reminder);
    let got = pieces(&ours(&t, &cwd));
    let root = t.root.to_str().unwrap();
    let show = |v: &[String]| {
        v.iter()
            .map(|p| p.replace(root, "$E"))
            .map(|p| {
                let head: String = p.lines().take(3).collect::<Vec<_>>().join(" | ");
                head.chars().take(220).collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    if want != got {
        let first = want
            .iter()
            .zip(got.iter())
            .position(|(a, b)| a != b)
            .unwrap_or(want.len().min(got.len()));
        panic!(
            "scenario {}: reminders differ at piece {first}\n--- claude ({} pieces)\n{}\n--- ours ({} pieces)\n{}\n--- claude piece:\n{}\n--- our piece:\n{}",
            scenario.name(),
            want.len(),
            show(&want),
            got.len(),
            show(&got),
            want.get(first)
                .map_or("", |p| p.as_str())
                .replace(root, "$E"),
            got.get(first)
                .map_or("", |p| p.as_str())
                .replace(root, "$E"),
        );
    }
}

#[test]
fn oracle_base() {
    check(Scenario::Base);
}

#[test]
fn oracle_fromrepo() {
    check(Scenario::FromRepo);
}

#[test]
fn oracle_agents() {
    check(Scenario::Agents);
}

#[test]
fn oracle_excludes() {
    check(Scenario::Excludes);
}

#[test]
fn oracle_worktree() {
    check(Scenario::Worktree);
}

#[test]
fn oracle_nested_worktree() {
    check(Scenario::NestedWorktree);
}

#[test]
fn oracle_bytes() {
    check(Scenario::Bytes);
}
