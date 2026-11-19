// One realistic snapshot, typed against the generated model so the compiler checks its shape.
// Only the mock host reads this file. It names the real agents on purpose: the mock stands in for a real scan.
import type { AbsPath } from "../gen/AbsPath";
import type { Activation } from "../gen/Activation";
import type { AgentInfo } from "../gen/AgentInfo";
import type { AgentView } from "../gen/AgentView";
import type { BlockLabel } from "../gen/BlockLabel";
import type { ContextBlock } from "../gen/ContextBlock";
import type { Diagnostic } from "../gen/Diagnostic";
import type { DocKind } from "../gen/DocKind";
import type { Extension } from "../gen/Extension";
import type { FileEntry } from "../gen/FileEntry";
import type { Format } from "../gen/Format";
import type { Hook } from "../gen/Hook";
import type { HookSchema } from "../gen/HookSchema";
import type { Load } from "../gen/Load";
import type { LoadStatus } from "../gen/LoadStatus";
import type { MemoryStore } from "../gen/MemoryStore";
import type { Part } from "../gen/Part";
import type { RuleSpec } from "../gen/RuleSpec";
import type { Scope } from "../gen/Scope";
import type { Slot } from "../gen/Slot";
import type { Snapshot } from "../gen/Snapshot";
import type { Via } from "../gen/Via";
import * as T from "./texts";
import { CWD, HOME } from "./texts";

export { CWD, HOME };
export const FINGERPRINT = "fp-mock-0001";

const enc = new TextEncoder();
export const estimateTokens = (text: string) => Math.max(1, Math.round(text.length / 3.8));
const rel = (path: string) => (path.startsWith(CWD) ? `./${path.slice(CWD.length + 1)}` : path.startsWith(HOME) ? `~/${path.slice(HOME.length + 1)}` : path);
const formatOf = (path: string): Format =>
  path.endsWith(".md") ? "markdown" : path.endsWith(".jsonc") ? "jsonc" : path.endsWith(".json") ? "json" : path.endsWith(".toml") ? "toml" : "text";

export const texts: Record<AbsPath, string> = {};
export const files: Record<AbsPath, FileEntry> = {};

/** Registers a file. `text === null` means the file does not exist. */
function reg(path: AbsPath, text: string | null, extra: Partial<FileEntry> = {}): AbsPath {
  if (files[path]) return path;
  const state: FileEntry["state"] =
    text === null
      ? { kind: "absent" }
      : { kind: "present", rev: `sha256:${(enc.encode(text).length * 2654435761 >>> 0).toString(16).padStart(8, "0")}`, bytes: enc.encode(text).length, lines: text === "" ? 0 : text.split("\n").length, tokens: text === "" ? 0 : estimateTokens(text), mtimeMs: 1_763_000_000_000 };
  if (text !== null) texts[path] = text;
  files[path] = { display: rel(path), realPath: null, format: formatOf(path), state, policy: { kind: "editable" }, agents: [], ...extra };
  return path;
}

const P = (relPath: string) => `${CWD}/${relPath}`;
const H = (relPath: string) => `${HOME}/${relPath}`;

// ---------- files ----------
const CLAUDE_MEM = H(".claude/projects/-Users-dev-acme-app/memory");
reg(H(".claude/CLAUDE.md"), T.claudeUser);
reg(P("CLAUDE.md"), T.claudeProject);
reg(P("CLAUDE.local.md"), "");
reg(P("AGENTS.md"), T.agentsMd);
reg(P("docs/missing-style.md"), null);
reg(H("shared/team-notes.md"), "# Team notes\n\nRelease freeze starts on the 20th.\n");
reg(P(".claude/rules/always.md"), T.claudeRuleAlways);
reg(P(".claude/rules/typescript.md"), T.claudeRuleTs);
reg(P(".claude/rules/broken.md"), T.claudeRuleBroken);
reg(P("src/api/CLAUDE.md"), T.claudeApi);
reg(P("vendor/lib/CLAUDE.md"), T.claudeVendor);
reg(P("docs/architecture.md"), T.architecture);
reg(P("docs/adr/0003-storage.md"), T.adr3);
reg(`${CLAUDE_MEM}/MEMORY.md`, T.memoryIndex);
reg(`${CLAUDE_MEM}/debugging.md`, T.memoryDebugging);
reg(`${CLAUDE_MEM}/user-prefs.md`, T.memoryPrefs);
reg(`${CLAUDE_MEM}/tauri-notes.md`, T.memoryTauri);
reg(P(".claude/agent-memory/reviewer/notes.md"), "# Reviewer notes\n\nThe storage module has no tests yet.\n");
reg(H(".claude/skills/review-pr/SKILL.md"), T.skillReview);
reg(H(".claude/plugins/superpowers/skills/review-pr/SKILL.md"), T.skillReview, { policy: { kind: "readOnly", why: { kind: "pluginCache", plugin: "superpowers" } } });
reg(P(".claude/skills/db-migrate/SKILL.md"), T.skillMigrate);
reg(P(".claude/agents/reviewer.md"), T.agentReviewer);
reg(P(".claude/commands/release.md"), T.commandRelease);
reg(P(".claude/settings.json"), T.settingsJson);
reg(H(".claude/settings.json"), '{\n  "hooks": {}\n}\n');
reg(P(".claude/settings.local.json"), '{\n  "hooks": {}\n}\n');

reg(H(".codex/AGENTS.md"), T.codexUser);
reg(P("AGENTS.override.md"), "");
reg(P("TEAM_GUIDE.md"), null);
files[P("TEAM_GUIDE.md")] = { ...files[P("TEAM_GUIDE.md")]!, state: { kind: "tooBig", bytes: 41_200 } };
reg(P(".codex/instructions.md"), T.codexInstructions);
reg(H(".codex/memories/memory_summary.md"), T.codexMemorySummary, { policy: { kind: "readOnly", why: { kind: "generated" } } });
reg(H(".agents/skills/lint-fix/SKILL.md"), T.codexSkill);
reg(H(".agents/skills/release-notes/SKILL.md"), "---\nname: release-notes\ndescription: Draft release notes\n---\n\nSummarize merged pull requests by area.\n");
reg(H(".codex/config.toml"), '[features]\nmemories = false\n\n[projects."/Users/dev/acme-app"]\ntrust_level = "untrusted"\n');
reg(P(".codex/hooks.json"), '{\n  "hooks": {}\n}\n');
reg(H(".codex/hooks.json"), '{\n  "hooks": {}\n}\n');
reg(H(".gemini/settings.json"), null);
reg(H(".gemini/GEMINI.md"), null);
reg(P("GEMINI.md"), null);

// ---------- loads ----------
function builder(agent: string) {
  const loads: Load[] = [];
  function add(
    path: AbsPath,
    kind: DocKind,
    scope: Scope,
    via: Via,
    when: Activation,
    status: LoadStatus = { kind: "loaded" },
    transformed = false,
  ): number {
    const entry = files[path]!;
    if (!entry.agents.includes(agent)) entry.agents.push(agent);
    const state = entry.state;
    let delivered: Load["delivered"] = null;
    if (state.kind === "present" && status.kind === "loaded") delivered = { bytes: state.bytes, tokens: state.tokens, transformed };
    if (state.kind === "present" && status.kind === "truncated") {
      const kept = (texts[path] ?? "").split("\n").slice(0, status.kept).join("\n");
      delivered = { bytes: enc.encode(kept).length, tokens: estimateTokens(kept), transformed };
    }
    const id = loads.length;
    loads.push({ id, file: path, kind, scope, via, when, status, delivered });
    return id;
  }
  return { loads, add };
}

const discovery = (rule: string): Via => ({ kind: "discovery", rule });
const start: Activation = { kind: "sessionStart" };
const skipped = (skip: Extract<LoadStatus, { kind: "skipped" }>["skip"]): LoadStatus => ({ kind: "skipped", skip });
const user: Scope = { kind: "user" };
const project: Scope = { kind: "project", dir: CWD };

// Claude
const c = builder("claude");
const cUser = c.add(H(".claude/CLAUDE.md"), "instructions", user, discovery("claude.memory.user"), start);
const cProj = c.add(P("CLAUDE.md"), "instructions", project, discovery("claude.memory.project"), start);
const cAgents = c.add(P("AGENTS.md"), "instructions", project, { kind: "import", host: cProj, line: 4, depth: 1 }, { kind: "withHost" });
c.add(P("docs/missing-style.md"), "instructions", project, { kind: "import", host: cProj, line: 21, depth: 1 }, { kind: "withHost" }, skipped({ kind: "missing" }));
c.add(H("shared/team-notes.md"), "instructions", user, { kind: "import", host: cProj, line: 22, depth: 1 }, { kind: "withHost" }, skipped({ kind: "needsApproval" }));
c.add(P("CLAUDE.local.md"), "instructions", { kind: "local" }, discovery("claude.memory.local"), start, skipped({ kind: "empty" }));
c.add(P(".claude/rules/always.md"), "rule", project, discovery("claude.rules.project"), start);
c.add(P(".claude/rules/broken.md"), "rule", project, discovery("claude.rules.project"), start, skipped({ kind: "invalid", detail: "paths must be a list of patterns" }));
c.add(P(".claude/rules/typescript.md"), "rule", project, discovery("claude.rules.project"), { kind: "onAccess", rule: { kind: "glob", base: CWD, patterns: ["**/*.ts", "**/*.tsx"] } }, { kind: "loaded" }, true);
const cApi = c.add(P("src/api/CLAUDE.md"), "instructions", project, discovery("claude.memory.project"), { kind: "onAccess", rule: { kind: "underDir", dir: P("src/api") } });
c.add(P("AGENTS.md"), "instructions", project, { kind: "import", host: cApi, line: 2, depth: 1 }, { kind: "withHost" }, skipped({ kind: "alreadyLoaded", first: cAgents }));
c.add(P("vendor/lib/CLAUDE.md"), "instructions", project, discovery("claude.memory.project"), { kind: "onAccess", rule: { kind: "underDir", dir: P("vendor/lib") } }, skipped({ kind: "excluded", pattern: "**/vendor/**" }));
const cArch = c.add(P("docs/architecture.md"), "reference", project, { kind: "reference", host: cProj, line: 14, depth: 1 }, { kind: "modelMayRead" }, skipped({ kind: "notRead", why: "claude.reference.not_read" }));
c.add(P("docs/adr/0003-storage.md"), "reference", project, { kind: "reference", host: cArch, line: 33, depth: 2 }, { kind: "modelMayRead" }, skipped({ kind: "notRead", why: "claude.reference.not_read" }));
const cMem = c.add(`${CLAUDE_MEM}/MEMORY.md`, "memoryIndex", user, discovery("claude.memory.auto"), start, { kind: "truncated", limit: { kind: "indexLines", value: 200, unit: "lines" }, kept: 200, total: 214 });
for (const name of ["debugging", "user-prefs", "tauri-notes"]) {
  c.add(`${CLAUDE_MEM}/${name}.md`, "memoryTopic", user, discovery("claude.memory.auto"), { kind: "modelMayRead" }, skipped({ kind: "notRead", why: "claude.memory.topic_on_demand" }));
}
const cExt: Extension[] = [];
function ext(b: ReturnType<typeof builder>, store: Extension[], id: string, extKind: Extension["extKind"], name: string, description: string | null, file: AbsPath, scope: Scope, listingWhen: Activation | null, listingStatus: LoadStatus, bodyKind: DocKind, status: Extension["status"]): void {
  const listing = listingWhen ? b.add(file, "listing", scope, { kind: "listing", ext: id }, listingWhen, listingStatus) : null;
  const body = b.add(file, bodyKind, scope, { kind: "listing", ext: id }, { kind: "onInvoke" });
  store.push({ id, extKind, name, description, file, scope, listing, body, status });
}
ext(c, cExt, "e-review", "skill", "review-pr", "Review a pull request for correctness and style", H(".claude/skills/review-pr/SKILL.md"), user, start, { kind: "loaded" }, "skillBody", { kind: "active" });
ext(c, cExt, "e-review-plugin", "skill", "review-pr", "Review a pull request for correctness and style", H(".claude/plugins/superpowers/skills/review-pr/SKILL.md"), { kind: "plugin", id: "superpowers" }, start, skipped({ kind: "shadowed", by: H(".claude/skills/review-pr/SKILL.md"), how: "nameCollision" }), "skillBody", { kind: "shadowed", by: "e-review" });
ext(c, cExt, "e-migrate", "skill", "db-migrate", "Write and check a database migration", P(".claude/skills/db-migrate/SKILL.md"), project, { kind: "onAccess", rule: { kind: "glob", base: CWD, patterns: ["db/**"] } }, { kind: "loaded" }, "skillBody", { kind: "conditional", patterns: ["db/**"] });
ext(c, cExt, "e-reviewer", "subagent", "reviewer", "Reads a change and reports bugs, not style", P(".claude/agents/reviewer.md"), project, start, { kind: "loaded" }, "subagentPrompt", { kind: "active" });
ext(c, cExt, "e-release", "command", "release", "Cut a release", P(".claude/commands/release.md"), project, start, { kind: "loaded" }, "commandBody", { kind: "active" });
void cUser;

// Codex
const x = builder("codex");
x.add(H(".codex/AGENTS.md"), "instructions", user, discovery("codex.agents_md.user"), start);
x.add(P("AGENTS.override.md"), "instructions", project, discovery("codex.agents_md.override"), start, skipped({ kind: "empty" }));
x.add(P("AGENTS.md"), "instructions", project, discovery("codex.agents_md.project"), start, skipped({ kind: "shadowed", by: P("AGENTS.override.md"), how: "emptyOverride" }));
x.add(P("TEAM_GUIDE.md"), "instructions", project, discovery("codex.agents_md.fallback"), start, skipped({ kind: "tooLarge", limit: { kind: "maxFileBytes", value: 32768, unit: "bytes" } }));
x.add(P(".codex/instructions.md"), "instructions", project, discovery("codex.agents_md.project"), start, skipped({ kind: "untrusted" }));
x.add(H(".codex/memories/memory_summary.md"), "memorySummary", user, discovery("codex.memories.summary"), start, skipped({ kind: "featureOff", name: "memories" }));
const xExt: Extension[] = [];
ext(x, xExt, "e-lint", "skill", "lint-fix", "Run the linters and fix what they report", H(".agents/skills/lint-fix/SKILL.md"), user, start, { kind: "loaded" }, "skillBody", { kind: "active" });
ext(x, xExt, "e-notes", "skill", "release-notes", "Draft release notes", H(".agents/skills/release-notes/SKILL.md"), user, start, skipped({ kind: "budgetExhausted", limit: { kind: "listingBudgetChars", value: 8000, unit: "chars" } }), "skillBody", { kind: "hidden", why: "codex.skills.listing_budget" });

// ---------- session blocks and budget ----------
const labelOf = (load: Load, exts: Extension[]): BlockLabel | null => {
  switch (load.kind) {
    case "instructions":
    case "rule":
      return "instructions";
    case "memoryIndex":
    case "memorySummary":
      return "memory";
    case "listing": {
      const e = exts.find((item) => load.via.kind === "listing" && item.id === load.via.ext);
      return e?.extKind === "subagent" ? "agentCatalog" : e?.extKind === "command" ? "commandCatalog" : "skillCatalog";
    }
    default:
      return null;
  }
};
const deliveryOf: Record<BlockLabel, ContextBlock["delivery"]> = {
  instructions: "systemPrompt",
  memory: "userMessage",
  skillCatalog: "toolSpec",
  agentCatalog: "toolSpec",
  commandCatalog: "toolSpec",
};

function blocksOf(loads: Load[], exts: Extension[]): ContextBlock[] {
  const order: BlockLabel[] = ["instructions", "memory", "skillCatalog", "agentCatalog", "commandCatalog"];
  const result: ContextBlock[] = [];
  for (const label of order) {
    const parts: Part[] = [];
    for (const load of loads) {
      if (!load.delivered) continue;
      const atStart = load.when.kind === "sessionStart" || (load.when.kind === "withHost" && load.via.kind === "import");
      if (!atStart || labelOf(load, exts) !== label && !(load.when.kind === "withHost" && label === "instructions")) continue;
      if (parts.length) parts.push({ kind: "glue", tokens: 6 });
      parts.push({ kind: "load", load: load.id, tokens: load.delivered.tokens });
    }
    if (!parts.length) continue;
    result.push({ delivery: deliveryOf[label], label, parts, tokens: parts.reduce((n, part) => n + part.tokens, 0), fidelity: label === "instructions" ? "exact" : "approximate" });
  }
  return result;
}

function budgetOf(loads: Load[], session: ContextBlock[]): AgentView["budget"] {
  const byLabel = session.map((block) => [block.label, block.tokens] as [BlockLabel, number]);
  return {
    sessionStart: session.reduce((n, block) => n + block.tokens, 0),
    byLabel,
    onAccessMax: loads.filter((l) => l.when.kind === "onAccess").reduce((n, l) => n + (l.delivered?.tokens ?? 0), 0),
    method: "o200kEstimate",
  };
}

// ---------- hooks ----------
function hook(id: string, file: AbsPath, scope: Scope, pointer: string, event: string, matcher: Hook["matcher"], handlerKind: string, summary: string, state: Hook["state"], toggle: Hook["toggle"], registered: Hook["source"]["registered"] = { kind: "sessionStart" }): Hook {
  return {
    id,
    source: { file, at: file.endsWith(".toml") ? { kind: "toml", path: [{ kind: "key", name: "hooks" }, { kind: "key", name: event }, { kind: "index", index: 0 }] } : { kind: "json", pointer }, scope, registered },
    event,
    matcher,
    handler: { handlerKind, raw: handlerKind === "command" ? { type: "command", command: summary, timeout: 10 } : { type: handlerKind, prompt: summary }, summary, timeoutMs: handlerKind === "command" ? 10000 : null },
    state,
    toggle,
  };
}
const active: Hook["state"] = { kind: "active" };
const native: Hook["toggle"] = { kind: "native" };
const claudeHooks: Hook[] = [
  hook("h1", P(".claude/settings.json"), project, "/hooks/SessionStart/0/hooks/0", "SessionStart", { raw: "startup|resume", eval: { kind: "exact", values: ["startup", "resume"] } }, "command", "${CLAUDE_PLUGIN_ROOT}/hooks/session-start.sh", active, native),
  hook("h2", P(".claude/settings.json"), project, "/hooks/PreToolUse/0/hooks/0", "PreToolUse", { raw: "Bash", eval: { kind: "exact", values: ["Bash"] } }, "command", "./scripts/guard-bash.sh", active, native),
  hook("h3", H(".claude/settings.json"), { kind: "plugin", id: "superpowers" }, "/hooks/PreToolUse/1/hooks/0", "PreToolUse", { raw: "Edit|Write.*", eval: { kind: "regex" } }, "command", "node ~/.claude/plugins/superpowers/hooks/edit-check.js", { kind: "willNotRun", why: "pluginDisabled" }, { kind: "unavailable", why: "claude.hooks.plugin_toggle" }),
  hook("h4", P(".claude/settings.local.json"), { kind: "local" }, "/hooks/PostToolUse/0/hooks/0", "PostToolUse", { raw: null, eval: { kind: "all" } }, "command", "./scripts/format-on-save.sh", { kind: "disabled", by: { kind: "native", setIn: P(".claude/settings.local.json") } }, native),
  hook("h5", H(".claude/settings.json"), user, "/hooks/UserPromptSubmit/0/hooks/0", "UserPromptSubmit", { raw: null, eval: { kind: "all" } }, "command", "~/bin/log-prompt", { kind: "disabled", by: { kind: "parked" } }, { kind: "park" }),
  hook("h6", H(".claude/settings.json"), { kind: "managed" }, "/hooks/Stop/0/hooks/0", "Stop", { raw: "done", eval: { kind: "ignored" } }, "http", "https://audit.example.com/hooks/stop", { kind: "willNotRun", why: "managedOnly" }, { kind: "unavailable", why: "claude.hooks.managed" }),
  hook("h7", P(".claude/settings.json"), project, "/hooks/Notification/0/hooks/0", "Notification", { raw: "[", eval: { kind: "invalid", error: "unclosed character class" } }, "prompt", "Summarize the notification in one line.", { kind: "willNotRun", why: "unsupportedType" }, native),
];
const codexHooks: Hook[] = [
  hook("h8", P(".codex/hooks.json"), project, "/hooks/PreToolUse/0/hooks/0", "PreToolUse", { raw: "shell", eval: { kind: "exact", values: ["shell"] } }, "command", "./scripts/audit-shell.sh", { kind: "willNotRun", why: "folderUntrusted" }, { kind: "park" }),
  hook("h9", H(".codex/hooks.json"), user, "/hooks/SessionStart/0/hooks/0", "SessionStart", { raw: null, eval: { kind: "all" } }, "command", "~/bin/codex-start", { kind: "willNotRun", why: "needsTrust" }, { kind: "park" }),
  hook("h10", H(".codex/config.toml"), user, "", "Stop", { raw: null, eval: { kind: "all" } }, "command", "~/bin/notify-done", { kind: "willNotRun", why: "modified" }, { kind: "park" }),
  hook("h11", H(".codex/config.toml"), user, "", "PostToolUse", { raw: null, eval: { kind: "all" } }, "command", "~/bin/record-tool", { kind: "willNotRun", why: "featureOff" }, { kind: "park" }),
  hook("h12", H(".codex/hooks.json"), user, "/hooks/Stop/1/hooks/0", "Stop", { raw: null, eval: { kind: "all" } }, "command", "~/bin/tidy", { kind: "disabled", by: { kind: "allHooksOff", setIn: H(".codex/config.toml") } }, { kind: "park" }),
];

// ---------- memory ----------
const entry = (path: string, role: "index" | "topic" | "summary" | "note" | "generated", indexed: boolean | null, title: string, description: string | null) => ({ file: path, role, indexed, title, description });
const claudeMemory: MemoryStore[] = [
  { id: "claude.auto", label: "claude.memory.auto", root: CLAUDE_MEM, enabled: { kind: "on" }, index: `${CLAUDE_MEM}/MEMORY.md`, entries: [entry(`${CLAUDE_MEM}/MEMORY.md`, "index", null, "Memory index", null), entry(`${CLAUDE_MEM}/debugging.md`, "topic", true, "debugging", "how to read the host logs"), entry(`${CLAUDE_MEM}/user-prefs.md`, "topic", true, "user-prefs", "how the user likes answers formatted"), entry(`${CLAUDE_MEM}/tauri-notes.md`, "topic", false, "tauri-notes", "quirks of the desktop shell")], write: { kind: "direct" } },
  { id: "claude.agent:reviewer", label: "claude.memory.agent", root: P(".claude/agent-memory/reviewer"), enabled: { kind: "on" }, index: null, entries: [entry(P(".claude/agent-memory/reviewer/notes.md"), "note", null, "Reviewer notes", null)], write: { kind: "notesDir", dir: P(".claude/agent-memory/reviewer") } },
];
const codexMemory: MemoryStore[] = [
  { id: "codex.memories", label: "codex.memories.store", root: H(".codex/memories"), enabled: { kind: "off", by: "features.memories in ~/.codex/config.toml" }, index: null, entries: [entry(H(".codex/memories/memory_summary.md"), "summary", null, "Memory summary", null)], write: { kind: "readOnly", why: "codex.memories.generated" } },
];
const geminiMemory: MemoryStore[] = [
  { id: "gemini.private", label: "gemini.memory.private", root: H(".gemini"), enabled: { kind: "on" }, index: H(".gemini/GEMINI.md"), entries: [], write: { kind: "direct" } },
];

// ---------- rules, schemas ----------
const rule = (id: string, en: string, ko: string): RuleSpec => ({ id, en, ko, evidence: "docs/agents" });
const claudeRules: RuleSpec[] = [
  rule("claude.memory.user", "The user-level CLAUDE.md in your home directory loads at the start of every session, in every project.", "홈 디렉터리의 사용자 CLAUDE.md는 모든 프로젝트에서 세션을 시작할 때마다 불러옵니다."),
  rule("claude.memory.project", "CLAUDE.md in the project root loads at the start of every session. A CLAUDE.md in a subdirectory loads when the agent first reads a file there.", "프로젝트 루트의 CLAUDE.md는 세션을 시작할 때마다 불러옵니다. 하위 디렉터리의 CLAUDE.md는 에이전트가 그 안의 파일을 처음 읽을 때 불러옵니다."),
  rule("claude.memory.local", "CLAUDE.local.md holds your private notes for this project. It loads after CLAUDE.md and is not meant for version control.", "CLAUDE.local.md에는 이 프로젝트에 대한 개인 메모를 둡니다. CLAUDE.md 다음에 불러오며 버전 관리에 넣지 않는 것이 좋습니다."),
  rule("claude.rules.project", "A file in .claude/rules loads at session start, unless its frontmatter lists paths. Then it loads when the agent reads a file that matches.", "추가로 .claude/rules 안의 파일은 세션을 시작할 때 불러옵니다. 단, 앞부분에 paths가 있으면 맞는 파일을 읽을 때 불러옵니다."),
  rule("claude.reference.not_read", "A plain file path in the text is only a hint. The model opens the file when it decides it needs it.", "본문에 적힌 파일 경로는 힌트일 뿐입니다. 모델이 필요하다고 판단하면 그때 파일을 엽니다."),
  rule("claude.memory.auto", "Auto memory keeps notes the agent writes for itself. Its index file loads at session start, cut after 200 lines.", "자동 메모리는 에이전트가 스스로 쓴 노트를 보관합니다. 색인 파일은 세션을 시작할 때 불러오며 200줄 뒤는 잘라 냅니다."),
  rule("claude.memory.topic_on_demand", "Topic files in the memory directory are not loaded. The model reads one when the index points to it.", "메모리 디렉터리의 주제 파일은 불러오지 않습니다. 색인이 가리킬 때 모델이 직접 읽습니다."),
  rule("claude.memory.agent", "A subagent can keep its own memory directory. Its notes load only while that subagent runs.", "서브에이전트는 자기만의 메모리 디렉터리를 둘 수 있습니다. 그 노트는 해당 서브에이전트가 실행되는 동안에만 불러옵니다."),
  rule("claude.hooks.plugin_toggle", "A plugin owns this hook. Turn the plugin off to stop the hook.", "이 훅은 플러그인이 소유합니다. 훅을 멈추려면 플러그인을 꺼야 합니다."),
  rule("claude.hooks.managed", "A managed policy owns this hook, so nobody can switch it here.", "관리 정책이 이 훅을 소유하므로 여기서는 아무도 켜고 끌 수 없습니다."),
  rule("claude.home.config", "The configuration directory of the agent.", "에이전트의 설정 디렉터리입니다."),
  rule("host.read_only", "The host runs with --read-only, so it refuses every change.", "호스트가 --read-only로 실행 중이므로 모든 변경을 거부합니다."),
  rule("host.read_only_file", "The agent or a plugin owns this file, so this tool does not change it.", "에이전트나 플러그인이 이 파일을 소유하므로 이 도구는 파일을 바꾸지 않습니다."),
  rule("host.invalid_hook", "The hook entry has the wrong shape for this agent's settings file.", "이 훅 항목은 에이전트 설정 파일에 맞는 모양이 아닙니다."),
  rule("claude.effect.next_session", "A new session reads this file again. A running session keeps the old text until you run /memory reload.", "새 세션을 시작하면 이 파일을 다시 읽습니다. 이미 실행 중인 세션은 /memory reload를 실행할 때까지 이전 내용을 씁니다."),
  rule("claude.rules.glob_fires", "A path rule fires because this path matches its pattern.", "이 경로가 패턴에 맞기 때문에 경로 규칙이 발동합니다."),
];
const codexRules: RuleSpec[] = [
  rule("codex.agents_md.user", "AGENTS.md in the Codex home directory loads at the start of every session.", "Codex 홈 디렉터리의 AGENTS.md는 세션을 시작할 때마다 불러옵니다."),
  rule("codex.agents_md.project", "Codex loads one instructions file per directory, from the repository root down to the working directory.", "Codex는 저장소 루트부터 작업 디렉터리까지 디렉터리마다 지침 파일을 하나씩 불러옵니다."),
  rule("codex.agents_md.override", "AGENTS.override.md replaces AGENTS.md in the same directory. When the override is empty, Codex still skips the regular file.", "AGENTS.override.md는 같은 디렉터리의 AGENTS.md를 대신합니다. 덮어쓰기 파일이 비어 있어도 Codex는 일반 파일을 건너뜁니다."),
  rule("codex.agents_md.fallback", "A fallback file name in the Codex settings loads when AGENTS.md is missing, up to a size limit.", "Codex 설정에 적은 대체 파일 이름은 AGENTS.md가 없을 때 크기 한도 안에서 불러옵니다."),
  rule("codex.memories.summary", "When the memories feature is on, a summary of earlier sessions loads at session start.", "메모리 기능이 켜져 있으면 이전 세션의 요약을 세션을 시작할 때 불러옵니다."),
  rule("codex.memories.store", "Codex keeps generated memories in its home directory.", "Codex는 자동으로 만든 메모리를 홈 디렉터리에 보관합니다."),
  rule("codex.memories.generated", "Codex generates these memories, so edits would be overwritten.", "Codex가 이 메모리를 자동으로 만들기 때문에 고쳐도 덮어써집니다."),
  rule("codex.skills.listing_budget", "The skill catalog has a character budget. Skills that do not fit are hidden from the model.", "스킬 목록에는 글자 수 예산이 있습니다. 예산에 맞지 않는 스킬은 모델에게 보이지 않습니다."),
  rule("codex.effect.next_session", "A new session reads this file again.", "새 세션을 시작하면 이 파일을 다시 읽습니다."),
  rule("codex.home.config", "The configuration directory of the agent.", "에이전트의 설정 디렉터리입니다."),
];
const geminiRules: RuleSpec[] = [
  rule("gemini.memory.private", "Saved memories are appended to GEMINI.md in the Gemini home directory.", "저장한 메모리는 Gemini 홈 디렉터리의 GEMINI.md 끝에 덧붙입니다."),
  rule("gemini.effect.next_session", "A new session reads this file again, and so does /memory refresh in a running session.", "새 세션을 시작하면 이 파일을 다시 읽고, 실행 중인 세션에서는 /memory refresh를 실행하면 다시 읽습니다."),
  rule("gemini.home.config", "The configuration directory of the agent.", "에이전트의 설정 디렉터리입니다."),
  rule("gemini.context.project", "GEMINI.md in the project root loads at the start of every session.", "프로젝트 루트의 GEMINI.md는 세션을 시작할 때마다 불러옵니다."),
];

const schema = (events: [string, "none" | "exact" | "regex" | "exactOrRegex"][]): HookSchema => ({
  events: events.map(([name, matcher]) => ({ name, matcher })),
  handlerKinds: [
    { kind: "command", fields: [{ key: "command", ty: "string", required: true }, { key: "timeout", ty: "number", required: false }] },
    { kind: "http", fields: [{ key: "url", ty: "string", required: true }] },
  ],
});

export const agentInfos: AgentInfo[] = [
  { id: "claude", displayName: "Claude Code", rules: claudeRules, hookSchema: schema([["SessionStart", "exactOrRegex"], ["PreToolUse", "exactOrRegex"], ["PostToolUse", "exactOrRegex"], ["UserPromptSubmit", "none"], ["Stop", "none"], ["Notification", "exactOrRegex"]]) },
  { id: "codex", displayName: "Codex CLI", rules: codexRules, hookSchema: schema([["SessionStart", "none"], ["PreToolUse", "exact"], ["PostToolUse", "exact"], ["Stop", "none"]]) },
  { id: "gemini", displayName: "Gemini CLI", rules: geminiRules, hookSchema: schema([["BeforeTool", "regex"], ["AfterTool", "regex"]]) },
];

// ---------- slots and agent views ----------
const slots = (agent: "claude" | "codex" | "gemini"): Slot[] =>
  agent === "gemini"
    ? [
        { id: "gemini.project", place: { kind: "file", path: P("GEMINI.md") }, newKind: "instructions", scope: project, rule: "gemini.context.project" },
        { id: "gemini.user", place: { kind: "file", path: H(".gemini/GEMINI.md") }, newKind: "memory", scope: user, rule: "gemini.memory.private" },
      ]
    : agent === "claude"
      ? [{ id: "claude.rule", place: { kind: "namedFile", dir: P(".claude/rules"), ext: "md" }, newKind: "rule", scope: project, rule: "claude.rules.project" }]
      : [{ id: "codex.project", place: { kind: "file", path: P("AGENTS.md") }, newKind: "instructions", scope: project, rule: "codex.agents_md.project" }];

const claudeSession = blocksOf(c.loads, cExt);
const codexSession = blocksOf(x.loads, xExt);

const agents: AgentView[] = [
  {
    agent: "claude",
    presence: { kind: "installed", bin: "/opt/homebrew/bin/claude" },
    homes: [{ role: "claude.home.config", path: H(".claude"), fromEnv: null, exists: true }],
    trust: { level: "trusted", recordedIn: H(".claude.json") },
    assumptions: [{ kind: "contextWindow", tokens: 200_000 }, { kind: "feature", name: "autoMemory", on: true, setIn: null }],
    loads: c.loads,
    session: claudeSession,
    budget: budgetOf(c.loads, claudeSession),
    hooks: claudeHooks,
    hookFiles: [
      { file: P(".claude/settings.json"), scope: project, format: "claudeSettingsJson", exists: true, policy: { kind: "editable" } },
      { file: P(".claude/settings.local.json"), scope: { kind: "local" }, format: "claudeSettingsJson", exists: true, policy: { kind: "editable" } },
      { file: H(".claude/settings.json"), scope: user, format: "claudeSettingsJson", exists: true, policy: { kind: "editable" } },
    ],
    memory: claudeMemory,
    extensions: cExt,
    slots: slots("claude"),
  },
  {
    agent: "codex",
    presence: { kind: "configOnly" },
    homes: [{ role: "codex.home.config", path: H(".codex"), fromEnv: "CODEX_HOME", exists: true }],
    trust: { level: "untrusted", recordedIn: H(".codex/config.toml") },
    assumptions: [{ kind: "feature", name: "memories", on: false, setIn: H(".codex/config.toml") }, { kind: "mode", name: "approval policy", value: "on-request", setIn: null }],
    loads: x.loads,
    session: codexSession,
    budget: budgetOf(x.loads, codexSession),
    hooks: codexHooks,
    hookFiles: [
      { file: P(".codex/hooks.json"), scope: project, format: "codexHooksJson", exists: true, policy: { kind: "editable" } },
      { file: H(".codex/config.toml"), scope: user, format: "codexConfigToml", exists: true, policy: { kind: "editable" } },
    ],
    memory: codexMemory,
    extensions: xExt,
    slots: slots("codex"),
  },
  {
    agent: "gemini",
    presence: { kind: "absent" },
    homes: [{ role: "gemini.home.config", path: H(".gemini"), fromEnv: null, exists: false }],
    trust: { level: "unknown", recordedIn: null },
    assumptions: [],
    loads: [],
    session: [],
    budget: { sessionStart: 0, byLabel: [], onAccessMax: 0, method: "o200kEstimate" },
    hooks: [],
    hookFiles: [{ file: H(".gemini/settings.json"), scope: user, format: "geminiSettingsJsonc", exists: false, policy: { kind: "editable" } }],
    memory: geminiMemory,
    extensions: [],
    slots: slots("gemini"),
  },
];

// ---------- diagnostics ----------
const fileAt = (path: string, line: number | null = null) => ({ kind: "file" as const, path, line });
const diagnostics: Diagnostic[] = [
  { severity: "error", agent: "claude", at: fileAt(P("CLAUDE.md"), 21), problem: { kind: "importMissing", raw: "@docs/missing-style.md" }, fix: null },
  { severity: "error", agent: "codex", at: fileAt(H(".codex/config.toml")), problem: { kind: "parseError", format: "toml", detail: "expected a table key at line 7", fatalForAgent: true }, fix: null },
  { severity: "error", agent: "claude", at: { kind: "hook", hook: "h7" }, problem: { kind: "hookMatcherInvalid", error: "unclosed character class" }, fix: null },
  { severity: "warning", agent: "claude", at: { kind: "load", agent: "claude", load: cMem }, problem: { kind: "truncated", limit: { kind: "indexLines", value: 200, unit: "lines" }, kept: 200, total: 214 }, fix: null },
  { severity: "warning", agent: "claude", at: fileAt(`${CLAUDE_MEM}/tauri-notes.md`), problem: { kind: "memoryNotInIndex", index: `${CLAUDE_MEM}/MEMORY.md` }, fix: { kind: "addMemory", agent: "claude", store: "claude.auto", title: "tauri-notes", description: "quirks of the desktop shell", body: "" } },
  { severity: "warning", agent: "claude", at: fileAt(P("CLAUDE.md"), 22), problem: { kind: "importNeedsApproval", raw: "@~/shared/team-notes.md" }, fix: null },
  { severity: "warning", agent: "claude", at: fileAt(P(".claude/rules/broken.md"), 2), problem: { kind: "frontmatterInvalid", field: "paths" }, fix: null },
  { severity: "warning", agent: "codex", at: fileAt(P("AGENTS.override.md")), problem: { kind: "shadowedByEmptyOverride" }, fix: { kind: "trash", path: P("AGENTS.override.md"), base: "sha256:00000000" } },
  { severity: "warning", agent: "codex", at: fileAt(P("TEAM_GUIDE.md")), problem: { kind: "tooLarge", limit: { kind: "maxFileBytes", value: 32768, unit: "bytes" } }, fix: null },
  { severity: "warning", agent: "codex", at: { kind: "agent" }, problem: { kind: "projectUntrusted", skipped: ["codex.agents_md.project", "codex.hooks.project"] }, fix: null },
  { severity: "warning", agent: "codex", at: { kind: "extension", ext: "e-notes" }, problem: { kind: "catalogOverBudget", used: 9120, limit: { kind: "listingBudgetChars", value: 8000, unit: "chars" } }, fix: null },
  { severity: "warning", agent: "codex", at: { kind: "hook", hook: "h8" }, problem: { kind: "hookWillNotRun", why: "folderUntrusted" }, fix: null },
  { severity: "info", agent: "claude", at: { kind: "extension", ext: "e-review-plugin" }, problem: { kind: "extensionRejected", why: "claude.skills.shadowed" }, fix: null },
  { severity: "info", agent: "claude", at: fileAt(P(".claude/rules/typescript.md")), problem: { kind: "globMatchesNothing", glob: "**/*.tsx" }, fix: null },
  { severity: "info", agent: "claude", at: fileAt(P("AGENTS.md")), problem: { kind: "agentsMdIgnored", because: P("CLAUDE.md") }, fix: null },
  { severity: "info", agent: "gemini", at: { kind: "agent" }, problem: { kind: "legacyLocation", why: "gemini.home.config" }, fix: null },
];

export const snapshot: Snapshot = {
  cwd: CWD,
  gitRoot: CWD,
  fingerprint: FINGERPRINT,
  files,
  agents,
  diagnostics,
  walk: { dirs: 412, files: 3_180, capped: false },
};

export const allRules: RuleSpec[] = agentInfos.flatMap((info) => info.rules);
