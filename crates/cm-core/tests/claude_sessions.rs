//! The Claude Code trace reader on records built by hand (no CLI), following
//! `docs/agents/claude-code-sessions.md`. Each record is written into a fixture home; the hashes
//! and digests are computed here from the spec (sections 3.2 and 3.3), not by the reader.
//!
//! The record texts are short stand-ins: no system prompt or tool definition of Claude Code, and
//! nothing taken from a real session.

mod trace_checks;

use cm_core::agents::claude::Claude;
use cm_core::agents::{Agent, ScanCx};
use cm_core::env::ScanEnv;
use cm_core::probe_fs::{LineLimits, ProbeFs};
use cm_core::testing::Fixture;
use cm_core::trace::OpenTrace;
use cm_core::*;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use trace_checks::{Expect, Sent, check, text};

// ---------------------------------------------------------------------------------------------
// The spec's hashes, written for ASCII fixtures
// ---------------------------------------------------------------------------------------------

/// Section 3.2: no whitespace, keys sorted. Every key and string here is ASCII, so byte order is
/// UTF-16 order, and every number is a small integer.
fn canon(value: &Value) -> String {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let fields: Vec<String> = keys
                .iter()
                .map(|k| format!("{}:{}", Value::String((*k).clone()), canon(&map[*k])))
                .collect();
            format!("{{{}}}", fields.join(","))
        }
        Value::Array(items) => {
            let items: Vec<String> = items.iter().map(canon).collect();
            format!("[{}]", items.join(","))
        }
        other => other.to_string(),
    }
}

fn sha(text: &str) -> String {
    Sha256::digest(text.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Section 3.2: string content counts as one text block.
fn blocks(message: &Value) -> Value {
    let mut m = message.clone();
    if let Some(Value::String(text)) = m.get("content") {
        m["content"] = json!([{"type": "text", "text": text}]);
    }
    m
}

/// The message hash: block form, no `cache_control` (the fixtures put none in a message).
fn message_hash(message: &Value) -> String {
    sha(&canon(&blocks(message)))
}

// ---------------------------------------------------------------------------------------------
// A record writer
// ---------------------------------------------------------------------------------------------

const MODEL: &str = "claude-test-model";
const OWNER_ACCOUNT: &str = "acct-5f1e7c-fixture-secret";
const OWNER_ORG: &str = "org-90ab3d-fixture-secret";
const OWNER_EMAIL: &str = "person@fixture.invalid";

fn secrets() -> Vec<String> {
    [OWNER_ACCOUNT, OWNER_ORG, OWNER_EMAIL]
        .map(str::to_owned)
        .to_vec()
}

struct Record {
    cwd: String,
    lines: Vec<String>,
    tick: u32,
    system: Vec<Value>,
    tools: Vec<Value>,
    shape: String,
    /// Each request written, by id: its messages in block form.
    requests: Vec<(String, Vec<Value>)>,
    /// Per request, what the checks expect it to hold.
    sent: Vec<Sent>,
}

impl Record {
    fn new(cwd: &AbsPath) -> Record {
        let system = vec![json!({"type": "text", "text": "You are a fixture agent."})];
        let tools = vec![json!({
            "name": "Echo",
            "description": "Echoes its input.",
            "input_schema": {"type": "object", "properties": {"x": {"type": "integer"}}}
        })];
        let shape = sha(&canon(&json!({"system": system, "tools": tools})));
        let mut record = Record {
            cwd: cwd.to_string(),
            lines: Vec::new(),
            tick: 0,
            system,
            tools,
            shape,
            requests: Vec::new(),
            sent: Vec::new(),
        };
        // Section 15: metadata and attachments that identify an account, an organisation and a
        // person. None of these values may reach a trace.
        record.push(
            json!({"type": "session-owner", "accountUuid": OWNER_ACCOUNT,
            "organizationUuid": OWNER_ORG, "sessionId": "s"}),
        );
        record.push(json!({"type": "attachment", "cwd": record.cwd,
            "attachment": {"type": "credential_org", "organizationUuid": OWNER_ORG}}));
        record.push(json!({"type": "attachment", "cwd": record.cwd,
            "attachment": {"type": "session_context", "context": {"userEmail": OWNER_EMAIL}}}));
        record
    }

    fn time(&mut self) -> String {
        self.tick += 1;
        format!("2026-11-20T10:00:{:02}.000Z", self.tick)
    }

    fn push(&mut self, mut line: Value) {
        if line.get("timestamp").is_none() {
            line["timestamp"] = Value::String(self.time());
        }
        self.lines.push(line.to_string());
    }

    fn write(&self, fx: &Fixture, id: &str) -> AbsPath {
        let key: String = self
            .cwd
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
            .collect();
        let file = fx.home_path(&format!(".claude/projects/{key}/{id}.jsonl"));
        fx.write(&file, &(self.lines.join("\n") + "\n"));
        file
    }

    fn user(&mut self, content: Value) -> Value {
        let message = json!({"role": "user", "content": content});
        self.push(json!({"type": "user", "cwd": self.cwd, "message": message}));
        blocks(&message)
    }

    fn attachment(&mut self, attachment: Value, rendered: &str, role: &str) {
        self.push(
            json!({"type": "attachment", "cwd": self.cwd, "attachment": attachment,
            "rendered": [{"content": rendered}], "renderedRole": role}),
        );
    }

    fn shape(&mut self) {
        self.push(json!({"type": "api-request-shape", "shapeHash": self.shape,
            "shape": {"system": self.system, "tools": self.tools}}));
    }

    fn blob(&mut self, message: Value) -> Value {
        self.push(
            json!({"type": "api-request-blob", "hash": message_hash(&message), "message": message}),
        );
        blocks(&message)
    }

    /// One reply, one record per block (section 6), each with the final usage.
    fn reply(&mut self, request: Option<&str>, id: &str, content: Vec<Value>, n: u64) -> Value {
        let usage = json!({"input_tokens": 1000 + n, "cache_read_input_tokens": 20000 + n,
            "cache_creation_input_tokens": 300 + n, "output_tokens": 40 + n,
            "output_tokens_details": {"thinking_tokens": n}});
        for (i, block) in content.iter().enumerate() {
            let mut line = json!({"type": "assistant", "cwd": self.cwd, "apiBlockIndex": i,
                "message": {"id": id, "role": "assistant", "model": MODEL,
                            "content": [block], "usage": usage}});
            if let Some(request) = request {
                line["requestRef"] = Value::String(request.to_owned());
            }
            self.push(line);
        }
        json!({"role": "assistant", "content": content})
    }

    /// Section 3.1 and 3.3: a request of `messages`, recorded as its base's first `keep` and the
    /// hashes of the rest. Marks the first system block and the last block of the last message.
    fn request(&mut self, id: &str, base: Option<&str>, keep: usize, messages: &[Value]) -> String {
        let tail: Vec<String> = messages[keep..].iter().map(message_hash).collect();
        self.request_with(id, base, keep, messages, tail, None)
    }

    fn request_with(
        &mut self,
        id: &str,
        base: Option<&str>,
        keep: usize,
        messages: &[Value],
        tail: Vec<String>,
        digest: Option<String>,
    ) -> String {
        let params = json!({"model": MODEL, "max_tokens": 1000});
        let mark = json!({"type": "ephemeral"});
        let last = messages.len() - 1;
        let last_block = messages[last]["content"].as_array().unwrap().len() - 1;
        let mut body = params.clone();
        let mut system = self.system.clone();
        system[0]["cache_control"] = mark.clone();
        let mut sent: Vec<Value> = messages.to_vec();
        sent[last]["content"][last_block]["cache_control"] = mark.clone();
        body["system"] = Value::Array(system);
        body["tools"] = Value::Array(self.tools.clone());
        body["messages"] = Value::Array(sent);
        let digest = digest.unwrap_or_else(|| sha(&canon(&body)));
        let at = self.time();
        let mut line = json!({"type": "api-request", "id": id, "timestamp": at,
        "querySource": "sdk", "shapeHash": self.shape, "params": params, "keep": keep,
        "tail": tail, "requestDigest": digest,
        "cacheControl": [
            {"path": ["system", 0], "value": mark},
            {"path": ["messages", last, "content", last_block], "value": mark},
        ]});
        if let Some(base) = base {
            line["base"] = Value::String(base.to_owned());
        }
        self.lines.push(line.to_string());
        self.requests.push((id.to_owned(), messages.to_vec()));
        self.sent.push(Sent {
            at: at.clone(),
            system: self
                .system
                .iter()
                .map(|b| b["text"].as_str().unwrap().to_owned())
                .collect(),
            messages: messages.iter().map(expect_blocks).collect(),
        });
        at
    }
}

/// What a message's pieces hold, from the message as written.
fn expect_blocks(message: &Value) -> Vec<Expect> {
    message["content"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| match b["type"].as_str().unwrap() {
            "text" => Expect::Text(b["text"].as_str().unwrap().to_owned()),
            "tool_use" => Expect::Input(b["input"].clone()),
            "tool_result" => Expect::Text(b["content"].as_str().unwrap().to_owned()),
            _ => Expect::Other,
        })
        .collect()
}

fn env(fx: &Fixture) -> ScanEnv {
    fx.env()
}

fn open(fx: &Fixture, id: &str) -> OpenTrace {
    open_prefix(fx, id, None)
}

fn open_prefix(fx: &Fixture, id: &str, prefix: Option<u64>) -> OpenTrace {
    let env = env(fx);
    let fs = ProbeFs::new();
    let data = Claude
        .trace(
            &ScanCx::new(&env, &fs),
            &TraceId(id.to_owned()),
            None,
            prefix,
        )
        .expect("the session opens");
    OpenTrace::new(data)
}

fn pieces(open: &OpenTrace, step: u32) -> Vec<(TracePiece, String)> {
    let pieces = open.step_pieces(step).unwrap();
    pieces
        .into_iter()
        .enumerate()
        .map(|(n, p)| {
            let t = text(open, step, n as u32);
            (p, t)
        })
        .collect()
}

/// The record most tests start from: an instruction file and a date as attachments, a prompt,
/// a reply with a tool call, its result, and a request that keeps fewer messages than its base.
fn conversation(fx: &Fixture) -> Record {
    let mut r = Record::new(&fx.project());
    let reminder =
        "<system-reminder>\nContents of /p/CLAUDE.md (project):\n\nbe brief\n</system-reminder>";
    r.attachment(
        json!({"type": "instructions", "files": [{"path": "/p/CLAUDE.md", "type": "Project", "content": "be brief"}]}),
        reminder,
        "user",
    );
    r.attachment(
        json!({"type": "date"}),
        "<system-reminder>\nToday is Monday.\n</system-reminder>",
        "system",
    );
    r.user(json!("hello there"));
    r.shape();
    let u1 = r.blob(json!({"role": "user", "content": [
        {"type": "text", "text": format!("{reminder}\n")},
        {"type": "text", "text": "hello there"}]}));
    // Section 3.2: a system message with string content and a key beside role and content.
    let s1 = r.blob(json!({"role": "system", "content": "Today is Monday.", "output_config": {"effort": "low"}}));
    r.request("r1", None, 0, &[u1.clone(), s1.clone()]);
    let a1 = r.reply(
        Some("r1"),
        "m1",
        vec![
            json!({"type": "text", "text": "Calling it."}),
            json!({"type": "tool_use", "id": "toolu_1", "name": "Echo", "input": {"x": 1}}),
        ],
        0,
    );
    let result = json!({"type": "tool_result", "tool_use_id": "toolu_1", "content": "echoed 1"});
    r.push(
        json!({"type": "user", "cwd": r.cwd, "message": {"role": "user", "content": [result]},
        "toolUseResult": {"stdout": "echoed 1"}}),
    );
    let t1 = json!({"role": "user", "content": [result]});
    r.request(
        "r2",
        Some("r1"),
        2,
        &[u1.clone(), s1.clone(), a1.clone(), t1],
    );
    // No reply to r2. A prompt typed meanwhile joins the pending user message, and r3 drops
    // r2's last message for the longer one (spec 4.3).
    r.user(json!("and also this"));
    let t2 =
        json!({"role": "user", "content": [result, {"type": "text", "text": "and also this"}]});
    r.request("r3", Some("r2"), 3, &[u1, s1, a1, t2]);
    r.reply(
        Some("r3"),
        "m3",
        vec![json!({"type": "text", "text": "Done."})],
        1,
    );
    r
}

// ---------------------------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------------------------

#[test]
fn every_request_rebuilds_exactly_with_its_pieces_labelled_from_the_record() {
    let fx = Fixture::new();
    let r = conversation(&fx);
    r.write(&fx, "sess-1");
    let open = open(&fx, "sess-1");
    assert_eq!(check(&open, &r.sent, &secrets()), r.sent.len());
    let trace = open.trace();
    assert_eq!(trace.problems, Vec::<TraceProblem>::new());
    assert_eq!(trace.steps.len(), 3);
    assert!(trace.steps.iter().all(|s| s.sent.as_ref().unwrap().exact));
    assert_eq!(trace.steps[0].model.as_deref(), Some(MODEL));
    assert_eq!(
        trace.steps[0].usage,
        Some(Usage {
            input: 1000,
            cache_read: 20000,
            cache_write: 300,
            output: 40,
            thinking: Some(0)
        })
    );
    assert_eq!(trace.steps[1].usage, None, "r2 has no reply");
    assert_eq!(trace.steps[2].usage.as_ref().map(|u| u.input), Some(1001));
    let reply: Vec<_> = trace.steps[0]
        .reply
        .iter()
        .map(|p| (p.kind, p.label.clone()))
        .collect();
    assert_eq!(
        reply,
        [
            (PieceKind::ModelText, None),
            (PieceKind::ToolCall, Some("Echo".into()))
        ]
    );

    let first = pieces(&open, 0);
    let find = |kind: PieceKind| first.iter().find(|(p, _)| p.kind == kind).unwrap();
    assert_eq!(find(PieceKind::System).1, "You are a fixture agent.");
    assert_eq!(
        find(PieceKind::ToolDefinition).0.label.as_deref(),
        Some("Echo")
    );
    let (instructions, text) = find(PieceKind::Instructions);
    assert_eq!(text, "be brief");
    assert_eq!(instructions.label.as_deref(), Some("Project"));
    assert_eq!(
        instructions.file.as_ref().map(|f| f.as_str()),
        Some("/p/CLAUDE.md")
    );
    assert_eq!(find(PieceKind::UserText).1, "hello there");
    assert_eq!(find(PieceKind::Environment).1, "Today is Monday.");
    assert!(first.iter().any(|(p, t)| p.kind == PieceKind::Frame
        && t == "<system-reminder>\nContents of /p/CLAUDE.md (project):\n\n"));
    assert_eq!(
        trace.steps[0].sent.as_ref().unwrap().marks,
        vec![0, first.len() as u32 - 1]
    );

    // r3 keeps three messages of r2 and replaces the fourth with a longer one whose first block
    // is r2's last: every piece of r2 is still in place.
    let second = pieces(&open, 1);
    let third = pieces(&open, 2);
    let result = second.last().unwrap();
    assert_eq!(
        (result.0.kind, result.0.label.as_deref(), result.1.as_str()),
        (PieceKind::ToolResult, Some("Echo"), "echoed 1")
    );
    let call = &second[second.len() - 2];
    assert_eq!(
        (call.0.kind, call.0.label.as_deref()),
        (PieceKind::ToolCall, Some("Echo"))
    );
    assert_eq!(
        trace.steps[2].sent.as_ref().unwrap().shared,
        second.len() as u32
    );
    assert_eq!(third.len(), second.len() + 1);
    assert_eq!(third.last().unwrap().1, "and also this");
}

#[test]
fn a_tail_hash_that_resolves_nowhere_makes_the_step_inexact() {
    let fx = Fixture::new();
    let mut r = Record::new(&fx.project());
    r.user(json!("hi"));
    r.shape();
    let u1 = r.blob(json!({"role": "user", "content": "a reminder"}));
    let missing = blocks(&json!({"role": "system", "content": "never recorded"}));
    let tail = vec![message_hash(&u1), message_hash(&missing)];
    r.request_with("r1", None, 0, &[u1, missing], tail, None);
    r.reply(
        Some("r1"),
        "m1",
        vec![json!({"type": "text", "text": "ok"})],
        0,
    );
    r.write(&fx, "sess-2");
    let open = open(&fx, "sess-2");
    check(&open, &[], &secrets());
    let trace = open.trace();
    let sent = trace.steps[0].sent.as_ref().unwrap();
    assert!(!sent.exact);
    assert_eq!(trace.problems, vec![TraceProblem::Inexact { steps: 1 }]);
    // The message that resolved is still shown; the one that did not has no piece.
    let all = pieces(&open, 0);
    assert!(all.iter().any(|(_, t)| t == "a reminder"));
    assert!(
        all.iter()
            .all(|(p, _)| !matches!(p.place, PiecePlace::Message { message: 1, .. }))
    );
}

#[test]
fn a_digest_that_does_not_match_makes_the_step_inexact() {
    let fx = Fixture::new();
    let mut r = Record::new(&fx.project());
    r.user(json!("hi"));
    r.shape();
    let u1 = r.blob(json!({"role": "user", "content": "hi with context"}));
    let tail = vec![message_hash(&u1)];
    r.request_with("r1", None, 0, &[u1], tail, Some(sha("another body")));
    r.reply(
        Some("r1"),
        "m1",
        vec![json!({"type": "text", "text": "ok"})],
        0,
    );
    r.write(&fx, "sess-3");
    let open = open(&fx, "sess-3");
    assert_eq!(check(&open, &r.sent, &secrets()), r.sent.len());
    assert!(!open.trace().steps[0].sent.as_ref().unwrap().exact);
    assert_eq!(
        open.trace().problems,
        vec![TraceProblem::Inexact { steps: 1 }]
    );
}

#[test]
fn a_partial_last_line_is_ignored_and_a_broken_line_is_counted() {
    let fx = Fixture::new();
    let r = conversation(&fx);
    let file = r.write(&fx, "sess-4");
    let open_whole = open(&fx, "sess-4");
    // A record still being written: its last line has no newline and does not parse yet.
    let mut bytes = std::fs::read(file.as_path()).unwrap();
    bytes.extend_from_slice(b"{\"type\":\"assistant\",\"message\":{\"id\":\"m9\",\"ro");
    std::fs::write(file.as_path(), &bytes).unwrap();
    let partial = open(&fx, "sess-4");
    assert_eq!(check(&partial, &r.sent, &secrets()), r.sent.len());
    assert_eq!(partial.trace().problems, Vec::<TraceProblem>::new());
    assert_eq!(partial.trace().steps, open_whole.trace().steps);
    assert_eq!(partial.read(), bytes.len() as u64);

    // The same text with a newline after it is a damaged line.
    bytes.push(b'\n');
    std::fs::write(file.as_path(), &bytes).unwrap();
    let damaged = open(&fx, "sess-4");
    assert_eq!(
        damaged.trace().problems,
        vec![TraceProblem::UnreadableLines { lines: 1 }]
    );
    assert_eq!(damaged.trace().steps, open_whole.trace().steps);
}

#[test]
fn a_prefix_of_a_grown_record_gives_the_trace_the_record_had_at_that_length() {
    let fx = Fixture::new();
    let r = conversation(&fx);
    let file = r.write(&fx, "sess-g");
    let whole = std::fs::read(file.as_path()).unwrap();
    // The record as it was while the session ran: cut inside the line of the second request.
    let line = r
        .lines
        .iter()
        .position(|l| l.contains(r#""id":"r2""#))
        .unwrap();
    let cut = r.lines[..line].iter().map(|l| l.len() + 1).sum::<usize>() + 20;
    std::fs::write(file.as_path(), &whole[..cut]).unwrap();
    let then = open(&fx, "sess-g");
    assert_eq!(then.read(), cut as u64);

    std::fs::write(file.as_path(), &whole).unwrap();
    let now = open(&fx, "sess-g");
    assert!(now.trace().steps.len() > then.trace().steps.len());
    let again = open_prefix(&fx, "sess-g", Some(then.read()));
    assert_eq!(again.read(), then.read());
    assert_eq!(again.trace().steps, then.trace().steps);
    for step in 0..then.trace().steps.len() as u32 {
        assert_eq!(pieces(&again, step), pieces(&then, step));
    }
}

#[test]
fn replies_without_request_records_are_steps_without_a_request() {
    let fx = Fixture::new();
    let mut r = Record::new(&fx.project());
    // Turns written before 2.1.302 (spec section 13): no request records, no requestRef.
    r.user(json!("first"));
    r.reply(None, "m1", vec![json!({"type": "text", "text": "one"})], 0);
    r.user(json!("second"));
    r.reply(
        None,
        "m2",
        vec![
            json!({"type": "thinking", "thinking": "hmm", "signature": "x"}),
            json!({"type": "text", "text": "two"}),
        ],
        1,
    );
    // A `<synthetic>` record is an error the CLI wrote, not a reply.
    r.push(
        json!({"type": "assistant", "cwd": r.cwd, "message": {"id": "m3", "role": "assistant",
        "model": "<synthetic>", "content": [{"type": "text", "text": "rate limited"}]}}),
    );
    r.write(&fx, "sess-5");
    let open = open(&fx, "sess-5");
    check(&open, &[], &secrets());
    let trace = open.trace();
    assert_eq!(trace.steps.len(), 2);
    assert!(
        trace
            .steps
            .iter()
            .all(|s| s.sent.is_none() && s.usage.is_some())
    );
    assert_eq!(trace.problems, vec![TraceProblem::NoRequests { steps: 2 }]);
    let kinds: Vec<_> = trace.steps[1].reply.iter().map(|p| p.kind).collect();
    assert_eq!(kinds, [PieceKind::Thinking, PieceKind::ModelText]);
    assert_eq!(trace.steps[1].usage.as_ref().unwrap().thinking, Some(1));
    assert_eq!(open.step_pieces(0), Some(Vec::new()));
    let reply = open.piece_text(1, PieceRef::Reply { piece: 1 });
    assert_eq!(
        reply,
        Some(PieceText::Current {
            text: "two".into(),
            bytes: 3,
            cut: false
        })
    );
}

#[test]
fn a_record_over_the_limit_keeps_the_steps_read_so_far() {
    let fx = Fixture::new();
    let r = conversation(&fx);
    let file = r.write(&fx, "sess-6");
    let whole = std::fs::read_to_string(file.as_path()).unwrap();
    // The limit falls inside the line of r2, after r1's reply and its tool result.
    let r2_at = whole.find("\"id\":\"r2\"").unwrap() as u64;
    let env = env(&fx);
    let fs = ProbeFs::with_line_limits(LineLimits {
        record: r2_at,
        line: 1 << 20,
    });
    let data = Claude
        .trace(
            &ScanCx::new(&env, &fs),
            &TraceId("sess-6".into()),
            None,
            None,
        )
        .unwrap();
    let open = OpenTrace::new(data);
    assert_eq!(check(&open, &r.sent, &secrets()), 1);
    let trace = open.trace();
    assert_eq!(trace.steps.len(), 1);
    assert!(trace.steps[0].sent.as_ref().unwrap().exact);
    assert_eq!(
        trace.problems,
        vec![TraceProblem::RecordTooLarge {
            bytes: whole.len() as u64,
            limit: r2_at
        }]
    );
    assert!(open.read() <= r2_at);
}

#[test]
fn an_id_outside_the_listing_is_not_found_and_opens_no_other_file() {
    let fx = Fixture::new();
    let r = conversation(&fx);
    let file = r.write(&fx, "sess-7");
    // A record one folder up, which `../` would reach from the session's folder.
    let outside = file
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("secret.jsonl");
    std::fs::copy(file.as_path(), outside.as_path()).unwrap();
    let env = env(&fx);
    for id in ["sess-unknown", "../secret", "sess-7/../../secret"] {
        let fs = ProbeFs::new();
        let got = Claude.trace(&ScanCx::new(&env, &fs), &TraceId(id.into()), None, None);
        assert_eq!(got.err(), Some(ApiError::NotFound), "{id}");
        assert!(
            fs.probed()
                .iter()
                .all(|p| *p != outside && !p.as_str().contains("..")),
            "{id} reached a path outside the listing"
        );
    }
    let fs = ProbeFs::new();
    let got = Claude.trace(
        &ScanCx::new(&env, &fs),
        &TraceId("sess-7".into()),
        Some("../sess-7"),
        None,
    );
    assert_eq!(got.err(), Some(ApiError::NotFound));
}

#[test]
fn the_list_holds_the_sessions_started_here_or_below_with_their_titles() {
    let fx = Fixture::new();
    let here = conversation(&fx);
    here.write(&fx, "sess-here");

    let mut below = Record::new(&fx.project_path("sub"));
    below.user(json!("a prompt typed below"));
    below.push(json!({"type": "ai-title", "aiTitle": "A title the model wrote"}));
    below.push(json!({"type": "custom-title", "customTitle": "A title the operator set"}));
    below.write(&fx, "sess-below");

    // `project-other` has a key that starts with the key of `project`, but started elsewhere.
    let other_dir = fx.root().join("project-other");
    let mut other = Record::new(&other_dir);
    other.user(json!("elsewhere"));
    other.write(&fx, "sess-other");

    let env = env(&fx);
    let fs = ProbeFs::new();
    let mut list = Claude.traces(&ScanCx::new(&env, &fs));
    list.sort_by(|a, b| a.id.cmp(&b.id));
    let ids: Vec<&str> = list.iter().map(|t| t.id.0.as_str()).collect();
    assert_eq!(ids, ["sess-below", "sess-here"]);
    assert_eq!(list[0].title.as_deref(), Some("A title the operator set"));
    assert_eq!(list[0].cwd, Some(fx.project_path("sub")));
    assert_eq!(list[1].title.as_deref(), Some("hello there"));
    assert_eq!(list[1].cwd, Some(fx.project()));
    assert_eq!(list[1].started.as_deref(), Some("2026-11-20T10:00:01.000Z"));
    assert!(list[1].last > list[1].started);
    assert!(list.iter().all(|t| t.bytes > 0 && t.agent == Claude.id()));
    let json = serde_json::to_string(&list).unwrap();
    assert!(secrets().iter().all(|s| !json.contains(s.as_str())));
}

#[test]
fn a_record_written_after_its_request_still_labels_the_blob() {
    let fx = Fixture::new();
    let mut r = Record::new(&fx.project());
    r.shape();
    // Spec section 11: the request record is written on its own schedule, so the prompt and the
    // attachment the blob holds can come after it.
    let u1 = r.blob(json!({"role": "user", "content": [
        {"type": "text", "text": "<system-reminder>\nA hook said hello.\n</system-reminder>\n"},
        {"type": "text", "text": "late prompt"}]}));
    r.request("r1", None, 0, &[u1]);
    r.attachment(
        json!({"type": "hook_additional_context", "hookEvent": "UserPromptSubmit"}),
        "<system-reminder>\nA hook said hello.\n</system-reminder>",
        "user",
    );
    r.user(json!("late prompt"));
    r.reply(
        Some("r1"),
        "m1",
        vec![json!({"type": "text", "text": "ok"})],
        0,
    );
    r.write(&fx, "sess-late");
    let open = open(&fx, "sess-late");
    assert_eq!(check(&open, &r.sent, &secrets()), 1);
    let all = pieces(&open, 0);
    let kinds: Vec<(PieceKind, &str)> = all
        .iter()
        .filter(|(p, _)| matches!(p.place, PiecePlace::Message { .. }))
        .map(|(p, t)| (p.kind, t.as_str()))
        .collect();
    assert_eq!(
        kinds,
        [
            (PieceKind::Frame, "<system-reminder>\n"),
            (PieceKind::HookOutput, "A hook said hello."),
            (PieceKind::Frame, "\n</system-reminder>\n"),
            (PieceKind::UserText, "late prompt"),
        ]
    );
}
