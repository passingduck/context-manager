"""Oracle: rebuild every request Claude Code recorded in a transcript and compare it with the
request the CLI really sent.

Usage (from the repository root):

    python3 tools/oracle/claude/session_oracle.py [scenario ...] [--keep]

Each scenario builds a sandbox under $TMPDIR with its own HOME and CLAUDE_CONFIG_DIR, starts a
loopback endpoint that speaks the streaming Messages API, and runs the installed `claude -p`
against it with a dummy key. The endpoint answers from a script, reports usage numbers from a
stand-in tokenizer (`block_tokens`), and stores every request with its headers. Every proxy variable points at a tripwire
listener, so a request that honours a proxy is counted instead of leaving the machine. Nothing
under ~/.claude is read or written.

The comparison follows docs/agents/claude-code-sessions.md: it pairs each recorded request with
a received one by `requestDigest` (section 3.3), rebuilds the recorded one from the transcript
(sections 3 and 4), and compares system blocks, tool definitions, messages, cache marks and
parameters. It then compares the usage in the reply records with the usage the endpoint sent
(section 6). The script exits 0 when every reached scenario passes, and skips with a message
when `claude` is not installed. --keep leaves the sandbox in place and prints its path.
"""
import http.server
import json
import os
import shutil
import socket
import subprocess
import sys
import tempfile
import threading
import time
import uuid

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import session_record as sr  # noqa: E402
from project_key import project_key  # noqa: E402

BILLING_PREFIX = "x-anthropic-billing-header:"
SUBAGENT_MARK = "ORACLE-SUBAGENT-TASK"


def find_claude():
    for d in os.environ.get("PATH", "").split(":") + [os.path.expanduser("~/.local/bin")]:
        p = os.path.join(d, "claude")
        if d and os.path.isfile(p) and os.access(p, os.X_OK):
            return p
    return None


# --------------------------------------------------------------------------------------------
# The loopback endpoint
# --------------------------------------------------------------------------------------------

def sse(events):
    return "".join(f"event: {e['type']}\ndata: {json.dumps(e)}\n\n" for e in events).encode()


def block_tokens(b):
    """The endpoint's stand-in tokenizer: a quarter of a block's JSON length, without its cache
    mark. A thinking block costs a quarter of its signature, as if the signature grew with the
    thinking it holds."""
    if isinstance(b, dict):
        b = {k: v for k, v in b.items() if k != "cache_control"}
        if b.get("type") == "thinking":
            return len(b.get("signature", "")) // 4
    return (len(json.dumps(b, separators=(",", ":"), ensure_ascii=False)) + 3) // 4


def prompt_tokens(body):
    """The prompt size of a request body: system blocks, tools, and each message's blocks plus 4."""
    system = body.get("system") or []
    n = sum(block_tokens(b) for b in (system if isinstance(system, list) else [system]))
    n += sum(block_tokens(t) for t in body.get("tools") or [])
    for m in body.get("messages") or []:
        n += 4 + sum(block_tokens(b) for b in sr.as_blocks(m.get("content")))
    return n


def usage_of(body, blocks, n):
    """Usage that follows the request: the prompt split into input, cache read and cache write,
    and the reply's blocks as output, with its thinking blocks counted apart."""
    prompt = prompt_tokens(body)
    inp = min(prompt, 3 + n)
    write = (prompt - inp) // 3
    hour = min(write, 200)
    usage = {"input_tokens": inp, "cache_read_input_tokens": prompt - inp - write,
             "cache_creation_input_tokens": write,
             "cache_creation": {"ephemeral_5m_input_tokens": write - hour, "ephemeral_1h_input_tokens": hour},
             "output_tokens": sum(block_tokens(b) for b in blocks), "service_tier": "standard"}
    thinking = sum(block_tokens(b) for b in blocks if b.get("type") == "thinking")
    if thinking:
        usage["output_tokens_details"] = {"thinking_tokens": thinking}
    return usage


def stream_events(msg):
    start = dict(msg, content=[], stop_reason=None, stop_sequence=None,
                 usage=dict(msg["usage"], output_tokens=1))
    ev = [{"type": "message_start", "message": start}]
    for i, b in enumerate(msg["content"]):
        if b["type"] == "thinking":
            # The thinking is not displayed: the stream carries the signature only.
            ev.append({"type": "content_block_start", "index": i,
                       "content_block": {"type": "thinking", "thinking": "", "signature": ""}})
            ev.append({"type": "content_block_delta", "index": i,
                       "delta": {"type": "signature_delta", "signature": b["signature"]}})
        elif b["type"] == "tool_use":
            ev.append({"type": "content_block_start", "index": i,
                       "content_block": {"type": "tool_use", "id": b["id"], "name": b["name"], "input": {}}})
            ev.append({"type": "content_block_delta", "index": i,
                       "delta": {"type": "input_json_delta", "partial_json": json.dumps(b["input"])}})
        else:
            ev.append({"type": "content_block_start", "index": i, "content_block": {"type": "text", "text": ""}})
            ev.append({"type": "content_block_delta", "index": i, "delta": {"type": "text_delta", "text": b["text"]}})
        ev.append({"type": "content_block_stop", "index": i})
    keys = ("input_tokens", "cache_read_input_tokens", "cache_creation_input_tokens", "output_tokens",
            "output_tokens_details")
    ev.append({"type": "message_delta", "delta": {"stop_reason": msg["stop_reason"], "stop_sequence": None},
               "usage": {k: msg["usage"][k] for k in keys if k in msg["usage"]}})
    ev.append({"type": "message_stop"})
    return ev


class Endpoint:
    """Answers POST /v1/messages from `script(body, n)`, which returns (blocks, stop_reason)."""

    def __init__(self, script):
        self.script = script
        self.received = []   # dicts: n, path, headers, body, reply
        self.lock = threading.Lock()
        outer = self

        class H(http.server.BaseHTTPRequestHandler):
            def do_POST(self):
                raw = self.rfile.read(int(self.headers.get("content-length") or 0))
                try:
                    body = json.loads(raw) if raw else None
                except ValueError:
                    body = None
                with outer.lock:
                    n = len(outer.received)
                    rec = {"n": n, "path": self.path, "headers": {k.lower(): v for k, v in self.headers.items()},
                           "body": body}
                    outer.received.append(rec)
                if "count_tokens" in self.path:
                    return self._json({"input_tokens": 100})
                if not self.path.startswith("/v1/messages") or not isinstance(body, dict):
                    return self._json({"type": "error", "error": {"type": "not_found_error", "message": "oracle"}}, 404)
                blocks, stop = outer.script(body, n)
                usage = usage_of(body, blocks, n)
                msg = {"id": f"msg_oracle_{n:03d}", "type": "message", "role": "assistant",
                       "model": body.get("model", "claude-opus-5-5"), "content": blocks,
                       "stop_reason": stop, "stop_sequence": None, "usage": usage}
                rec["reply"] = msg
                if body.get("stream"):
                    data = sse(stream_events(msg))
                    self.send_response(200)
                    self.send_header("content-type", "text/event-stream")
                    self.send_header("content-length", str(len(data)))
                    self.end_headers()
                    self.wfile.write(data)
                else:
                    self._json(msg)

            do_GET = do_POST

            def _json(self, obj, status=200):
                data = json.dumps(obj).encode()
                self.send_response(status)
                self.send_header("content-type", "application/json")
                self.send_header("content-length", str(len(data)))
                self.end_headers()
                self.wfile.write(data)

            def log_message(self, *a):
                pass

        self.server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), H)
        threading.Thread(target=self.server.serve_forever, daemon=True).start()

    @property
    def url(self):
        return f"http://127.0.0.1:{self.server.server_address[1]}"

    def close(self):
        self.server.shutdown()
        self.server.server_close()


class Tripwire:
    """Counts connections. Every proxy variable points here."""

    def __init__(self):
        self.count = 0
        self.sock = socket.socket()
        self.sock.bind(("127.0.0.1", 0))
        self.sock.listen(16)
        threading.Thread(target=self._loop, daemon=True).start()

    def _loop(self):
        while True:
            try:
                c, _ = self.sock.accept()
            except OSError:
                return
            self.count += 1
            c.close()

    @property
    def url(self):
        return f"http://127.0.0.1:{self.sock.getsockname()[1]}"

    def close(self):
        self.sock.close()


# --------------------------------------------------------------------------------------------
# Scripted replies
# --------------------------------------------------------------------------------------------

def text(t):
    return [{"type": "text", "text": t}], "end_turn"


def tool(name, inp, n):
    return [{"type": "tool_use", "id": f"toolu_oracle_{n:03d}", "name": name, "input": inp}], "tool_use"


def first_user_text(body):
    msgs = body.get("messages") or []
    if not msgs:
        return ""
    c = msgs[0].get("content")
    return c if isinstance(c, str) else "".join(b.get("text", "") for b in c if isinstance(b, dict))


def turns_taken(body):
    return sum(1 for m in body.get("messages") or [] if m.get("role") == "assistant")


def tool_names(body):
    return [t.get("name") for t in body.get("tools") or []]


def plan(steps):
    """Main-thread replies by the number of assistant turns already in the conversation.
    A request without tools is auxiliary and gets a plain text reply."""
    def script(body, n):
        last = (body.get("messages") or [{}])[-1]
        if not body.get("tools") or any("Respond with TEXT ONLY" in x for x in block_texts(last.get("content"))):
            return text("<analysis>Oracle analysis.</analysis>\n<summary>Oracle summary of the conversation so far.</summary>")
        if SUBAGENT_MARK in first_user_text(body):
            return text("Subagent finished the oracle task.")
        k = turns_taken(body)
        if k < len(steps):
            return steps[k](body, n)
        return text("Oracle final answer.")
    return script


# --------------------------------------------------------------------------------------------
# Scenarios
# --------------------------------------------------------------------------------------------

def w(path, content):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "w") as f:
        f.write(content)


SCENARIOS = {}


def scenario(name):
    def reg(fn):
        SCENARIOS[name] = fn
        return fn
    return reg


@scenario("oneshot")
def s_oneshot(box):
    """One prompt, one reply, with a user and a project instruction file."""
    w(os.path.join(box["cfg"], "CLAUDE.md"), "ORACLE-USER-INSTRUCTIONS: answer briefly.\n")
    w(os.path.join(box["cwd"], "CLAUDE.md"), "ORACLE-PROJECT-INSTRUCTIONS: greet the oracle.\n")
    return [{"prompt": "Say hello to the oracle.", "script": plan([lambda b, n: text("Hello, oracle.")])}]


def text_then_tool(note, name, inp, n):
    """A reply with two content blocks, so the transcript holds two reply records for one request."""
    blocks, stop = tool(name, inp, n)
    return [{"type": "text", "text": note}] + blocks, stop


def thinking_then(reply):
    """`reply` led by a thinking block whose text is not displayed (session spec, section 6)."""
    blocks, stop = reply
    return [{"type": "thinking", "thinking": "", "signature": "oracle-signature-" + "s" * 183}] + blocks, stop


@scenario("bash")
def s_bash(box):
    steps = [lambda b, n: thinking_then(text_then_tool(
                 "Running it.", "Bash", {"command": "echo oracle-bash-output", "description": "Print a marker"}, n)),
             lambda b, n: text("The command printed the marker.")]
    return [{"prompt": "Run the echo command.", "script": plan(steps), "args": ["--allowedTools", "Bash"]}]


@scenario("bigbash")
def s_bigbash(box):
    """A Bash output long enough that Claude Code saves it under tool-results/ (section 9)."""
    cmd = "yes oracle-long-output-line | head -n 4000"
    steps = [lambda b, n: tool("Bash", {"command": cmd, "description": "Print many lines"}, n),
             lambda b, n: text("The output was long.")]
    return [{"prompt": "Print many lines.", "script": plan(steps), "args": ["--allowedTools", "Bash"]}]


@scenario("skill")
def s_skill(box):
    w(os.path.join(box["cwd"], ".claude/skills/oracle-skill/SKILL.md"),
      "---\nname: oracle-skill\ndescription: Prints the oracle skill marker. Use when asked for the oracle skill.\n---\n\n"
      "ORACLE-SKILL-BODY: reply with the word done.\n")
    steps = [lambda b, n: tool("Skill", {"skill": "oracle-skill"}, n), lambda b, n: text("done")]
    return [{"prompt": "Use the oracle skill.", "script": plan(steps), "args": ["--allowedTools", "Skill"]}]


@scenario("mention")
def s_mention(box):
    w(os.path.join(box["cwd"], "notes.txt"), "ORACLE-MENTIONED-FILE line one\nline two\n")
    return [{"prompt": "Summarise @notes.txt please.", "script": plan([lambda b, n: text("It has two lines.")])}]


@scenario("hook")
def s_hook(box):
    w(os.path.join(box["cfg"], "settings.json"), json.dumps({"hooks": {"SessionStart": [
        {"hooks": [{"type": "command", "command": "echo ORACLE-SESSIONSTART-HOOK-LINE"}]}]}}))
    return [{"prompt": "Say hello.", "script": plan([lambda b, n: text("Hello.")])}]


@scenario("subagent")
def s_subagent(box):
    def spawn(b, n):
        name = "Agent" if "Agent" in tool_names(b) else "Task"
        return tool(name, {"description": "Oracle subtask", "prompt": f"{SUBAGENT_MARK}: reply with one line.",
                           "subagent_type": "general-purpose"}, n)
    steps = [spawn, lambda b, n: text("The subagent replied.")]
    return [{"prompt": "Delegate the oracle subtask.", "script": plan(steps), "args": ["--allowedTools", "Agent,Task"]}]


@scenario("compact")
def s_compact(box):
    talk = plan([lambda b, n: text("First answer.")])
    return [
        {"prompt": "Say something to compact later.", "script": talk},
        {"prompt": "/compact", "script": talk, "resume": True},
        {"prompt": "Say something after the compaction.", "script": talk, "resume": True},
    ]


@scenario("projectkey")
def s_projectkey(box):
    """Sessions started in a subdirectory and in a nested git worktree (section 1.2)."""
    git = ["git", "-c", "user.name=oracle", "-c", "user.email=oracle", "-c", "commit.gpgsign=false", "-C", box["cwd"]]
    genv = {"HOME": box["home"], "PATH": "/usr/bin:/bin:/usr/sbin:/sbin"}
    subprocess.run(git + ["commit", "-q", "--allow-empty", "-m", "oracle"], env=genv, check=True)
    worktree = os.path.join(box["cwd"], ".claude", "worktrees", "oracle-wt")
    subprocess.run(git + ["worktree", "add", "-q", worktree], env=genv, check=True)
    sub = os.path.join(box["cwd"], "sub")
    os.makedirs(sub)
    box["expect_keys"] = [project_key(os.path.realpath(d)) for d in (sub, worktree)]
    box["refuse_keys"] = [project_key(os.path.realpath(box["cwd"]))]
    talk = plan([lambda b, n: text("Hello.")])
    return [{"prompt": "Say hello.", "script": talk, "cwd": d, "new_session": True} for d in (sub, worktree)]


# --------------------------------------------------------------------------------------------
# Running and comparing
# --------------------------------------------------------------------------------------------

SECURITY_STUB = """#!/bin/sh
[ "$1" = "-i" ] && cat >/dev/null
exit 44
"""


def run_scenario(claude, name, keep):
    root = tempfile.mkdtemp(prefix=f"cm-session-oracle-{name}-")
    box = {"root": root, "home": os.path.join(root, "home"), "cfg": os.path.join(root, "cfg"),
           "cwd": os.path.join(root, "work", "proj")}
    for d in (box["home"], box["cfg"], box["cwd"]):
        os.makedirs(d, exist_ok=True)
    # Claude Code asks the macOS `security` tool, found on PATH, for its credentials. Under a
    # sandbox home there is no keychain, so the real tool makes macOS show a "keychain not found"
    # dialog and the CLI waits on it. This stub, first on PATH, answers every keychain question
    # with "not found" (exit 44), so no real `security` process starts and no keychain is touched.
    stub_dir = os.path.join(root, "bin")
    os.makedirs(stub_dir)
    w(os.path.join(stub_dir, "security"), SECURITY_STUB)
    os.chmod(os.path.join(stub_dir, "security"), 0o755)
    subprocess.run(["git", "init", "-q", box["cwd"]], check=False)
    runs = SCENARIOS[name](box)
    session = str(uuid.uuid4())
    received, tripped, exits = [], 0, []
    for i, step in enumerate(runs):
        if step.get("new_session"):
            session = str(uuid.uuid4())
        ep, trip = Endpoint(step["script"]), Tripwire()
        env = {"HOME": box["home"], "PATH": stub_dir + ":/usr/bin:/bin:/usr/sbin:/sbin", "TERM": "dumb",
               "CLAUDE_CONFIG_DIR": box["cfg"], "ANTHROPIC_API_KEY": "sk-ant-dummy-oracle",
               "ANTHROPIC_BASE_URL": ep.url, "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC": "1",
               "DISABLE_AUTOUPDATER": "1", "NO_PROXY": "127.0.0.1,localhost", "no_proxy": "127.0.0.1,localhost"}
        for k in ("HTTP_PROXY", "HTTPS_PROXY", "http_proxy", "https_proxy", "ALL_PROXY"):
            env[k] = trip.url
        args = [claude, "-p", step["prompt"]] + step.get("args", [])
        args += ["--resume", session] if step.get("resume") else ["--session-id", session]
        args += ["--debug-file", os.path.join(root, f"debug-{i}.log")]
        try:
            p = subprocess.run(args, cwd=step.get("cwd", box["cwd"]), env=env, stdin=subprocess.DEVNULL,
                               stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=120)
            exits.append(p.returncode)
        except subprocess.TimeoutExpired:
            exits.append("timeout")
        time.sleep(0.3)
        received += [dict(r, run=i) for r in ep.received]
        tripped += trip.count
        ep.close()
        trip.close()
    result = compare(box, received)
    result["extra"], missing = facts(box, EXPECT.get(name, ()))
    result["failures"] += missing
    result.update(exits=exits, tripwire=tripped, root=root)
    if not keep:
        shutil.rmtree(root, ignore_errors=True)
    return result


def transcripts(cfg):
    out = []
    for d, _, fs in os.walk(os.path.join(cfg, "projects")):
        for f in fs:
            if f.endswith(".jsonl"):
                out.append(os.path.join(d, f))
    return sorted(out)


def sent_view(r):
    """The request as the record would describe it: the billing block dropped from `system`,
    `betas` taken from the header, and the keys Claude Code leaves out removed (section 3.3)."""
    body = dict(r["body"])
    if isinstance(body.get("system"), list):
        body["system"] = [b for b in body["system"] if not str(b.get("text", "")).startswith(BILLING_PREFIX)]
    beta = r["headers"].get("anthropic-beta")
    if beta:
        body["betas"] = [x.strip() for x in beta.split(",") if x.strip()]
    return {k: v for k, v in body.items() if k not in sr.UNRECORDED_KEYS}


def norm_messages(msgs):
    return [dict(m, content=sr.as_blocks(m.get("content"))) for m in msgs]


def diff(a, b, path="", out=None, limit=8):
    out = [] if out is None else out
    if len(out) >= limit:
        return out
    if isinstance(a, dict) and isinstance(b, dict):
        for k in sorted(set(a) | set(b)):
            if k not in a or k not in b:
                out.append(f"{path}/{k}: only in {'rebuilt' if k in a else 'sent'}")
            else:
                diff(a[k], b[k], f"{path}/{k}", out, limit)
    elif isinstance(a, list) and isinstance(b, list):
        if len(a) != len(b):
            out.append(f"{path}: length {len(a)} rebuilt, {len(b)} sent")
        for i, (x, y) in enumerate(zip(a, b)):
            diff(x, y, f"{path}/{i}", out, limit)
    elif a != b:
        out.append(f"{path}: differs")
    return out


def compare(box, received):
    files = transcripts(box["cfg"])
    loaded = [sr.Transcript(p) for p in files]
    records = [(t, r) for t in loaded for r in t.requests]
    msgs_sent = [r for r in received if r["path"].startswith("/v1/messages") and isinstance(r["body"], dict)
                 and "count_tokens" not in r["path"]]
    by_digest = {}
    for r in msgs_sent:
        by_digest.setdefault(sr.request_digest(sent_view(r)), []).append(r)
    checks, failures = [], []
    paired = set()
    for t, rec in records:
        want = rec.get("requestDigest") or rec.get("cutRequestDigest")
        sent = by_digest.get(want, [])
        label = f"{os.path.basename(t.path)}#{rec.get('querySource')}"
        if not sent:
            failures.append(f"{label}: no received request has this requestDigest")
            continue
        s = sent[0]
        paired.add(s["n"] + 1000 * s["run"])
        body, missing = t.rebuild(rec)
        if body is None:
            failures.append(f"{label}: could not rebuild ({','.join(sorted(set(missing)))})")
            continue
        view = sent_view(s)
        d = []
        for key in ("system", "tools"):
            d += diff(sr.strip_cache_control(body.get(key)), sr.strip_cache_control(view.get(key)), "/" + key)
        d += diff(norm_messages(sr.strip_cache_control(body["messages"])),
                  norm_messages(sr.strip_cache_control(view["messages"])), "/messages")
        marks_rebuilt = sr.cache_marks(dict(body, messages=norm_messages(body["messages"])))
        marks_sent = sr.cache_marks(dict(view, messages=norm_messages(view["messages"])))
        if marks_rebuilt != marks_sent:
            d.append("cache marks differ")
        params_r = {k: v for k, v in body.items() if k not in ("system", "tools", "messages")}
        params_s = {k: v for k, v in view.items() if k not in ("system", "tools", "messages")}
        d += diff(params_r, params_s, "/params")
        if sr.request_digest(body) != want:
            d.append("digest of the rebuilt body differs")
        reply = s.get("reply") or {}
        sent_usage = reply.get("usage", {})
        replies = t.replies.get(rec["id"], [])
        if not replies:
            d.append("no reply record")
        for rr in replies:
            u = rr["message"].get("usage") or {}
            keys = ("input_tokens", "cache_read_input_tokens", "cache_creation_input_tokens", "output_tokens",
                    "cache_creation")
            # The CLI records `output_tokens_details` with a thinking count of 0 when the endpoint sent none.
            details = sent_usage.get("output_tokens_details", {"thinking_tokens": 0})
            if any(u.get(k) != sent_usage.get(k) for k in keys) or rr["message"].get("id") != reply.get("id") \
                    or u.get("output_tokens_details") != details:
                d.append(f"usage of reply record {rr.get('apiBlockIndex')} differs")
        if len(replies) != len(reply.get("content", [])):
            d.append(f"{len(replies)} reply records for {len(reply.get('content', []))} content blocks")
        sources = {}
        for h in t.message_hashes(rec):
            src = t.resolve(h)[0]
            sources[src] = sources.get(src, 0) + 1
        checks.append((label, len(body["messages"]), len(rec.get("cacheControl", [])), d, sources, len(replies)))
        if d:
            failures.append(f"{label}: " + "; ".join(d))
    unrecorded = [r for r in msgs_sent if r["n"] + 1000 * r["run"] not in paired]
    return {"files": [os.path.relpath(p, box["cfg"]) for p in files], "checks": checks, "failures": failures,
            "received": len(msgs_sent),
            "unrecorded": [{"run": r["run"], "n": r["n"], "tools": len(r["body"].get("tools") or []),
                            "messages": len(r["body"].get("messages") or [])} for r in unrecorded]}


REMINDER_OPEN, REMINDER_CLOSE = "<system-reminder>\n", "\n</system-reminder>"

# The facts each scenario exists to show. A scenario fails when one of its facts does not hold.
EXPECT = {
    "bash": ("two reply records",),
    "bigbash": ("persisted output",),
    "skill": ("merged user records",),
    "mention": ("rendered user",),
    "hook": ("rendered system",),
    "subagent": ("subagent link",),
    "compact": ("compaction",),
    "projectkey": ("folder per cwd",),
}


def block_texts(content):
    out = []
    for b in sr.as_blocks(content):
        if not isinstance(b, dict):
            continue
        if b.get("type") == "text":
            out.append(b.get("text", ""))
        elif b.get("type") == "tool_result":
            c = b.get("content")
            out.append(c if isinstance(c, str) else "".join(x.get("text", "") for x in c or [] if isinstance(x, dict)))
    return out


def facts(box, expected):
    """Scenario facts from the sandbox transcripts (sections 7 to 10). Returns (lines, failures)."""
    lines, held = [], set()
    files = transcripts(box["cfg"])
    for p in files:
        t = sr.Transcript(p)
        recs = [json.loads(x) for x in open(p, encoding="utf-8") if x.strip()]
        # 10: each rendered attachment reaches the next recorded request, user text as is, system text unwrapped
        pending, found, total = [], {"user": 0, "system": 0}, {"user": 0, "system": 0}
        for r in recs:
            if r.get("type") == "attachment" and isinstance(r.get("rendered"), list) and r.get("renderedRole"):
                pending.append(r)
            elif r.get("type") == "api-request" and pending:
                msgs = [t.resolve(h)[1] for h in t.message_hashes(r) or []]
                for a in pending:
                    role = a["renderedRole"]
                    for piece in a["rendered"]:
                        c = piece.get("content", "")
                        if role == "system" and c.startswith(REMINDER_OPEN) and c.endswith(REMINDER_CLOSE):
                            c = c[len(REMINDER_OPEN):-len(REMINDER_CLOSE)]
                        total[role] = total.get(role, 0) + 1
                        if any(m and m.get("role") == role and any(c in x for x in block_texts(m.get("content")))
                               for m in msgs):
                            found[role] = found.get(role, 0) + 1
                pending = []
        for role in ("user", "system"):
            if total.get(role):
                lines.append(f"{os.path.basename(p)}: rendered {role} attachments found in the next request: "
                             f"{found[role]} of {total[role]}")
                if found[role] == total[role]:
                    held.add(f"rendered {role}")
        # 6: a request with two content blocks leaves two reply records with the same usage
        if any(len(v) >= 2 for v in t.replies.values()):
            held.add("two reply records")
        # 4.2: a message resolved from a run of two or more user records
        for r in t.requests:
            for h in t.message_hashes(r) or []:
                src, m = t.resolve(h)
                if src == "transcript" and m["role"] == "user":
                    n = sum(1 for x in recs if x.get("type") == "user" and isinstance((x.get("message") or {}).get("content"), list)
                            and any(b in m["content"] for b in x["message"]["content"]))
                    if n >= 2:
                        held.add("merged user records")
        # 7: compaction
        kinds = [r.get("type") + ("/" + r.get("subtype", "") if r.get("type") == "system" else "") for r in recs]
        if "system/compact_boundary" in kinds:
            i = kinds.index("system/compact_boundary")
            after = [r for r in recs[i:] if r.get("type") == "api-request"]
            summary = any(r.get("isCompactSummary") for r in recs[i:])
            ok = bool(after) and after[0]["keep"] == 0 and "base" not in after[0] and summary
            lines.append(f"{os.path.basename(p)}: compact boundary, summary record {summary}, first request after it "
                         f"keep={after[0]['keep'] if after else None} base={'base' in after[0] if after else None}")
            if ok:
                held.add("compaction")
        # 9: persisted tool output
        for r in t.requests:
            for h in t.message_hashes(r) or []:
                m = t.resolve(h)[1]
                for x in block_texts(m.get("content")) if m else []:
                    if x.startswith("<persisted-output>"):
                        path = next((w.strip() for w in x.split("Full output saved to:")[1:2]), "").split("\n")[0]
                        size = os.path.getsize(path) if path and os.path.isfile(path) else None
                        inside = path.startswith(os.path.join(os.path.dirname(p), t.requests[0]["sessionId"], "tool-results"))
                        lines.append(f"{os.path.basename(p)}: request carries a {len(x)}-char stand-in; the saved file "
                                     f"is in tool-results/: {inside}, holds {size} bytes")
                        if size and size > len(x) and inside:
                            held.add("persisted output")
        # 8: subagent links
        if "/subagents/" in p:
            agent = os.path.basename(p)[len("agent-"):-len(".jsonl")]
            meta = json.load(open(p[:-len(".jsonl")] + ".meta.json"))
            parent = os.path.dirname(os.path.dirname(p)) + ".jsonl"
            precs = [json.loads(x) for x in open(parent, encoding="utf-8") if x.strip()]
            uses = {b.get("id") for r in precs if r.get("type") == "assistant"
                    for b in r["message"]["content"] if isinstance(b, dict) and b.get("type") == "tool_use"}
            results = {(r.get("toolUseResult") or {}).get("agentId") for r in precs
                       if r.get("type") == "user" and isinstance(r.get("toolUseResult"), dict)}
            ok = meta.get("toolUseId") in uses and agent in results and all(rq.get("agentId") == agent for rq in t.requests)
            lines.append(f"{os.path.basename(p)}: meta.json toolUseId names a parent tool_use {meta.get('toolUseId') in uses}, "
                         f"parent toolUseResult.agentId names this file {agent in results}, "
                         f"{len(t.requests)} request records carry this agentId")
            if ok:
                held.add("subagent link")
    if "expect_keys" in box:
        folders = sorted({os.path.basename(os.path.dirname(p)) for p in files if "/subagents/" not in p})
        lines.append(f"transcript folders: {len(folders)}; one per session cwd "
                     f"{folders == sorted(box['expect_keys'])}; any under the main checkout's key "
                     f"{any(k in folders for k in box['refuse_keys'])}")
        if folders == sorted(box["expect_keys"]):
            held.add("folder per cwd")
    missing = [f"expected fact not shown: {e}" for e in expected if e not in held]
    return lines, missing


def main(argv):
    keep = "--keep" in argv
    names = [a for a in argv if not a.startswith("--")] or list(SCENARIOS)
    claude = find_claude()
    if claude is None:
        print("SKIP: claude is not installed, so the session oracle cannot run")
        return 0
    version = subprocess.run([claude, "--version"], capture_output=True, text=True).stdout.strip()
    print(f"claude {version}")
    bad = 0
    for name in names:
        res = run_scenario(claude, name, keep)
        ok = not res["failures"] and res["checks"] and res["tripwire"] == 0
        bad += 0 if ok else 1
        print(f"\n[{name}] {'PASS' if ok else 'FAIL'}  exits={res['exits']} received={res['received']} "
              f"recorded={len(res['checks'])} tripwire={res['tripwire']}")
        for f in res["files"]:
            print(f"  transcript {f}")
        for label, nmsg, nmarks, d, sources, nrep in res["checks"]:
            src = ", ".join(f"{k} {v}" for k, v in sorted(sources.items()))
            print(f"  {label}: {nmsg} messages ({src}), {nmarks} cache marks, {nrep} reply records, "
                  f"{'match' if not d else 'MISMATCH'}")
        for f in res["extra"]:
            print(f"  {f}")
        for f in res["failures"]:
            print(f"  FAIL {f}")
        for u in res["unrecorded"]:
            print(f"  not recorded: run {u['run']} request {u['n']} (tools={u['tools']}, messages={u['messages']})")
        if keep:
            print(f"  sandbox kept at {res['root']}")
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
