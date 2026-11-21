// Writes a hand-built Claude Code session record into a sandbox home, so the sessions view has something to show.
// The record follows docs/agents/claude-code-sessions.md: sections 3 and 4 for the request records and their
// hashes, section 6 for replies and usage, section 8 for a compaction, section 9 for a subagent. Every text is a
// short stand-in. Nothing here is copied from a real session or a real system prompt.
import { createHash, randomUUID } from "node:crypto";
import { appendFileSync, mkdirSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";

export const SESSION_ID = "5e55e55e-0000-4000-8000-000000000001";
export const AGENT_ID = "a0b1c2d3e4f5a6b7";
const VERSION = "2.1.329";
const MODEL = "model-stand-in";

/** Section 3.2: SHA-256 of canonical JSON, with keys sorted by UTF-16 code units and no whitespace. */
function canon(value, omitCacheControl) {
  if (Array.isArray(value)) return `[${value.map((v) => canon(v, omitCacheControl)).join(",")}]`;
  if (value && typeof value === "object") {
    const keys = Object.keys(value)
      .filter((k) => value[k] !== undefined && !(omitCacheControl && k === "cache_control"))
      .sort();
    return `{${keys.map((k) => `${JSON.stringify(k)}:${canon(value[k], omitCacheControl)}`).join(",")}}`;
  }
  return JSON.stringify(value);
}
const sha = (text) => createHash("sha256").update(text, "utf8").digest("hex");

const blocks = (message) => (typeof message.content === "string" ? { ...message, content: [{ type: "text", text: message.content }] } : message);
/** Section 3.2: the message hash turns string content into one text block and leaves out `cache_control`. */
export const messageHash = (message) => sha(canon(blocks(message), true));

/** The project's folder name under `<config>/projects/`: every character outside [A-Za-z0-9] becomes `-` (section 1.2). */
export const projectKey = (cwd) => cwd.replace(/[^A-Za-z0-9]/g, "-");

const text = (t) => ({ type: "text", text: t });
const user = (content) => ({ role: "user", content });
const model = (content) => ({ role: "assistant", content });

/** The texts the test compares a piece against. */
export const TEXTS = {
  system0: "STAND-IN SYSTEM BLOCK ONE. It names the tool and the rules.",
  system1: "STAND-IN SYSTEM BLOCK TWO. It holds the project rules.",
  system1Changed: "STAND-IN SYSTEM BLOCK TWO, EDITED. It holds the project rules and one more line.",
  toolRead: "Reads a stand-in file.",
  toolBash: "Runs a stand-in command.",
  promptOne: "Prompt one: list the stand-in files.",
  readOut: "stand-in file text, line one\nstand-in file text, line two",
  promptTwo: "Prompt two: now change the stand-in file.",
  summary: "Summary of the earlier work: the stand-in files were listed and one was changed.",
  promptThree: "Prompt three: continue.",
};

const tools = (read = TEXTS.toolRead) => [
  { name: "Read", description: read, input_schema: { type: "object", properties: { path: { type: "string" } } } },
  { name: "Bash", description: TEXTS.toolBash, input_schema: { type: "object", properties: { command: { type: "string" } } } },
];

/**
 * The calls of the main session. `shape` picks the system blocks. `usage` is what the reply reports.
 * Call 4 edits the second system block, so its cache read falls to the first block. Call 6 follows a compaction.
 * Call 7 sends the request of call 6 and a further message after six minutes, and finds no cache.
 */
function script() {
  const sys = (second) => [text(TEXTS.system0), text(second)];
  const a1 = model([text("Reading the file."), { type: "tool_use", id: "toolu_1", name: "Read", input: { path: "a.txt" } }]);
  const r1 = user([{ type: "tool_result", tool_use_id: "toolu_1", content: TEXTS.readOut }]);
  const a2 = model([text("Done reading.")]);
  const u2 = user(TEXTS.promptTwo);
  const a3 = model([text("Changed it.")]);
  const sum = user(TEXTS.summary);
  const a4 = model([text("Continuing.")]);
  const u3 = user(TEXTS.promptThree);
  return {
    main: [
      { at: 0, system: sys(TEXTS.system1), messages: [user(TEXTS.promptOne)], reply: a1.content, usage: { input: 20, read: 0, write: 700, output: 40 } },
      { at: 30, system: sys(TEXTS.system1), messages: [user(TEXTS.promptOne), a1, r1], reply: a2.content, usage: { input: 15, read: 700, write: 120, output: 30 } },
      { at: 60, system: sys(TEXTS.system1), messages: [user(TEXTS.promptOne), a1, r1, a2, u2], reply: a3.content, usage: { input: 18, read: 820, write: 110, output: 35 } },
      // The second system block changed: only the first block is read from the cache.
      { at: 90, system: sys(TEXTS.system1Changed), messages: [user(TEXTS.promptOne), a1, r1, a2, u2, a3, user("Prompt two b: one more thing.")], reply: [text("Noted.")], usage: { input: 22, read: 150, write: 900, output: 25 } },
      { at: 120, system: sys(TEXTS.system1Changed), messages: [user(TEXTS.promptOne), a1, r1, a2, u2, a3, user("Prompt two b: one more thing."), model([text("Noted.")]), user("Prompt two c.")], reply: a4.content, usage: { input: 12, read: 1050, write: 80, output: 20 } },
      // A compaction came before this call: a new chain starts with the summary.
      { at: 150, compactBefore: true, system: sys(TEXTS.system1Changed), messages: [sum], reply: [text("Ready.")], usage: { input: 25, read: 300, write: 200, output: 10 } },
      // Six minutes later the cache is gone, though every piece is the same as before.
      { at: 150 + 360, system: sys(TEXTS.system1Changed), messages: [sum, model([text("Ready.")]), u3], reply: [text("Fine.")], usage: { input: 30, read: 0, write: 560, output: 12 } },
      // An interrupted call: the record holds its request and no reply, so it has no usage.
      { at: 150 + 400, system: sys(TEXTS.system1Changed), messages: [sum, model([text("Ready.")]), u3, model([text("Fine.")]), user("Prompt four: stop.")], reply: [], usage: null },
    ],
    sub: [
      { at: 40, system: [text("STAND-IN SUBAGENT SYSTEM BLOCK.")], messages: [user("Subagent task: find the stand-in files.")], reply: [text("Found two.")], usage: { input: 10, read: 0, write: 300, output: 8 } },
      { at: 55, system: [text("STAND-IN SUBAGENT SYSTEM BLOCK.")], messages: [user("Subagent task: find the stand-in files."), model([text("Found two.")]), user("Name them.")], reply: [text("a.txt and b.txt.")], usage: { input: 6, read: 300, write: 60, output: 9 } },
    ],
  };
}

/** One chain of requests: the records it writes and the replies' conversation records. */
function writeChain({ calls, start, lines, agentId, sessionId, cwd, state }) {
  const stamp = (seconds) => new Date(start + seconds * 1000).toISOString();
  let previous = state?.previous ?? null;
  let written = state?.written ?? new Set();
  const shapes = state?.shapes ?? new Set();
  const facts = [];
  calls.forEach((call, i) => {
    const at = stamp(call.at);
    if (call.compactBefore) {
      lines.push({ type: "system", subtype: "compact_boundary", sessionId, timestamp: stamp(call.at - 5), uuid: randomUUID(), compactMetadata: { trigger: "auto", preTokens: 1500, postTokens: 400, durationMs: 2000 } });
      previous = null;
      written = new Set();
    }
    const shape = { system: call.system, tools: tools() };
    const shapeHash = sha(canon(shape, true));
    if (!shapes.has(shapeHash)) {
      shapes.add(shapeHash);
      lines.push({ type: "api-request-shape", sessionId, shapeHash, timestamp: at, version: VERSION, ...(agentId ? { agentId } : {}), shape });
    }
    const hashes = call.messages.map(messageHash);
    let keep = 0;
    if (previous) while (keep < hashes.length && keep < previous.hashes.length && hashes[keep] === previous.hashes[keep]) keep++;
    const tail = hashes.slice(keep);
    call.messages.slice(keep).forEach((message, k) => {
      if (written.has(tail[k])) return;
      written.add(tail[k]);
      lines.push({ type: "api-request-blob", sessionId, hash: tail[k], ...(agentId ? { agentId } : {}), message });
    });
    // Section 5: one mark on each system block of this layout and one on the last block of the last message.
    const marks = [
      ...call.system.map((_, n) => ({ path: ["system", n], value: { type: "ephemeral" } })),
      { path: ["messages", call.messages.length - 1, "content", 0], value: { type: "ephemeral" } },
    ];
    // Section 3.3: the digest covers the body with every cache mark in place.
    const params = { model: MODEL, max_tokens: 1024 };
    const body = { ...params, system: structuredClone(call.system), tools: shape.tools, messages: call.messages.map((m) => structuredClone(blocks(m))) };
    for (const { path, value } of marks) {
      let node = body;
      for (const step of path) node = node[step];
      node.cache_control = value;
    }
    const id = randomUUID();
    lines.push({
      type: "api-request", sessionId, id, timestamp: at, version: VERSION, querySource: agentId ? "agent:builtin:general-purpose" : "repl_main_thread", ...(agentId ? { agentId } : {}),
      shapeHash, params, ...(previous && keep > 0 ? { base: previous.id, keep } : { keep: 0 }), tail, cacheControl: marks, requestDigest: sha(canon(body, false)),
    });
    // Section 6: one assistant record per content block, each with the final usage.
    const messageId = `msg_${id.slice(0, 8)}`;
    const usage = call.usage && { input_tokens: call.usage.input, cache_read_input_tokens: call.usage.read, cache_creation_input_tokens: call.usage.write, output_tokens: call.usage.output };
    call.reply.forEach((block, n) => {
      lines.push({
        type: "assistant", sessionId, uuid: randomUUID(), timestamp: stamp(call.at + 5), cwd, version: VERSION, requestRef: id, apiBlockIndex: n, ...(agentId ? { agentId, isSidechain: true } : {}),
        message: { id: messageId, role: "assistant", model: MODEL, content: [block], usage },
      });
    });
    previous = { id, hashes };
    facts.push({ id, at, hashes, keep, usage: call.usage, system: call.system.map((b) => b.text) });
  });
  return { facts, state: { previous, written, shapes } };
}

/**
 * Writes the session under `<home>/.claude/projects/<key>/` and returns what the test needs to know.
 * `cwd` is the project's real path: the folder key and the first record's `cwd` come from it.
 */
export function writeSession(home, cwd) {
  const dir = join(home, ".claude", "projects", projectKey(cwd));
  const start = Date.parse("2026-11-20T09:00:00.000Z");
  const { main, sub } = script();
  const lines = [];
  lines.push({ type: "user", sessionId: SESSION_ID, uuid: randomUUID(), timestamp: new Date(start).toISOString(), cwd, version: VERSION, message: { role: "user", content: TEXTS.promptOne } });
  lines.push({ type: "ai-title", sessionId: SESSION_ID, aiTitle: "Stand-in session for the e2e run" });
  const { facts: mainFacts, state } = writeChain({ calls: main, start, lines, agentId: null, sessionId: SESSION_ID, cwd });
  const subLines = [];
  const { facts: subFacts } = writeChain({ calls: sub, start, lines: subLines, agentId: AGENT_ID, sessionId: SESSION_ID, cwd });
  const file = join(dir, `${SESSION_ID}.jsonl`);
  const subFile = join(dir, SESSION_ID, "subagents", `agent-${AGENT_ID}.jsonl`);
  const meta = join(dir, SESSION_ID, "subagents", `agent-${AGENT_ID}.meta.json`);
  mkdirSync(dirname(subFile), { recursive: true });
  writeFileSync(file, lines.map((l) => JSON.stringify(l)).join("\n") + "\n");
  writeFileSync(subFile, subLines.map((l) => JSON.stringify(l)).join("\n") + "\n");
  writeFileSync(meta, JSON.stringify({ agentType: "explorer", description: "Find the stand-in files", toolUseId: "toolu_agent", finished: true }));
  const last = main[main.length - 1];
  const append = () => {
    const call = { at: 150 + 460, system: last.system, messages: [...last.messages, user("Prompt five: a call that arrives while the session is open.")], reply: [text("Appended.")], usage: { input: 14, read: 900, write: 60, output: 7 } };
    const more = [];
    writeChain({ calls: [call], start, lines: more, agentId: null, sessionId: SESSION_ID, cwd, state });
    appendFileSync(file, more.map((l) => JSON.stringify(l)).join("\n") + "\n");
  };
  return { file, append, calls: mainFacts, sub: subFacts, texts: TEXTS, compactBefore: 5, lostByChange: 3, lostByExpiry: 6, bytes: lines.length };
}
