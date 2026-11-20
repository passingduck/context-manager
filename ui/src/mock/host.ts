// Answers every operation from the mock snapshot. Loaded only when the URL has `?mock=1`.
import type { AbsPath } from "../gen/AbsPath";
import type { HookRun } from "../gen/HookRun";
import type { Load } from "../gen/Load";
import type { LoadText } from "../gen/LoadText";
import type { RenderedBlock } from "../gen/RenderedBlock";
import type { Request } from "../gen/Request";
import type { SearchHit } from "../gen/SearchHit";
import type { Simulation } from "../gen/Simulation";
import { agentOfHook, controls, history, initDisk, installControls, mockState, pendingIds, reject, waitForBump } from "./disk";
import { apply, plan } from "./plan";
import { agentInfos, coreRules, CWD, files, HOME, snapshot, texts } from "./snapshot";

const globToRegExp = (glob: string) =>
  new RegExp(
    `^${glob
      .split(/(\*\*\/|\*\*|\*)/)
      .map((part) => (part === "**/" ? "(?:.*/)?" : part === "**" ? ".*" : part === "*" ? "[^/]*" : part.replace(/[.+^${}()|[\]\\?]/g, "\\$&")))
      .join("")}$`,
  );

function fires(load: Load, path: AbsPath): boolean {
  if (load.when.kind !== "onAccess") return false;
  const rule = load.when.rule;
  if (rule.kind === "underDir") return path.startsWith(`${rule.dir}/`);
  const relPath = path.startsWith(`${rule.base}/`) ? path.slice(rule.base.length + 1) : path;
  return rule.patterns.some((pattern) => globToRegExp(pattern).test(relPath));
}

function simulate(path: AbsPath): Simulation[] {
  const abs = path.startsWith("/") ? path : `${CWD}/${path.replace(/^\.\//, "")}`;
  const nested = /^(.*\/packages\/[^/]+)\//.exec(abs);
  return snapshot.agents.map((view) => {
    const fired = view.loads.filter((load) => fires(load, abs) && load.status.kind === "loaded");
    const extra: Load[] =
      nested && view.loads.length
        ? [{ id: 900, file: `${nested[1]}/CLAUDE.md`, kind: "instructions", scope: { kind: "project", dir: nested[1]! }, via: { kind: "discovery", rule: "claude.memory.project" }, when: { kind: "onAccess", rule: { kind: "underDir", dir: nested[1]! } }, status: { kind: "loaded" }, delivered: { bytes: 900, tokens: 240, transformed: false } }]
        : [];
    const addedTokens = [...fired, ...extra].reduce((n, load) => n + (load.delivered?.tokens ?? 0), 0);
    return { agent: view.agent, fired: fired.map((load) => load.id), extra, addedTokens, notes: fired.length ? ["claude.rules.glob_fires"] : [] };
  });
}

/** The mock's stand-in for an adapter's text of one load: the file, or its kept lines. */
function loadText(load: Load): string | null {
  if (load.status.kind === "skipped") return null;
  const text = texts[load.file] ?? "";
  return load.status.kind === "truncated" ? text.split("\n").slice(0, load.status.kept).join("\n") : text;
}

function render(agent: string): RenderedBlock[] {
  const view = snapshot.agents.find((item) => item.agent === agent);
  if (!view) return [];
  return view.session.map((block) => ({
    delivery: block.delivery,
    label: block.label,
    tokens: block.tokens,
    fidelity: block.fidelity,
    pieces: block.parts.map((part) =>
      part.kind === "glue" ? { kind: "frame" as const, text: "\n\n" } : { kind: "load" as const, load: part.load, text: loadText(view.loads[part.load]!) ?? "" },
    ),
  }));
}

function loadTexts(agent: string, file: AbsPath): LoadText[] {
  const view = snapshot.agents.find((item) => item.agent === agent);
  return (view?.loads ?? [])
    .filter((load) => load.file === file)
    .map((load) => {
      const text = loadText(load);
      return { load: load.id, text, wholeFile: text !== null && text.trim() === (texts[file] ?? "").trim() };
    });
}

function search(query: string, caseSensitive: boolean, regex: boolean): { hits: SearchHit[]; capped: boolean } {
  let test: (line: string) => number;
  try {
    const re = regex ? new RegExp(query, caseSensitive ? "" : "i") : null;
    const needle = caseSensitive ? query : query.toLowerCase();
    test = (line) => (re ? line.search(re) : (caseSensitive ? line : line.toLowerCase()).indexOf(needle));
  } catch {
    return { hits: [], capped: false };
  }
  const hits: SearchHit[] = [];
  if (!query) return { hits, capped: false };
  for (const [file, text] of Object.entries(texts)) {
    text.split("\n").forEach((line, index) => {
      const column = test(line);
      if (column >= 0 && hits.length < 500) hits.push({ file, line: index + 1, column: column + 1, text: line.slice(0, 200) });
    });
  }
  return { hits, capped: hits.length >= 500 };
}

const readOnlyHost = () => new URLSearchParams(window.location.search).get("readonly") === "1";

const SAMPLES: Record<string, unknown> = {
  PreToolUse: { hook_event_name: "PreToolUse", tool_name: "Bash", tool_input: { command: "ls -la" } },
  PostToolUse: { hook_event_name: "PostToolUse", tool_name: "Edit", tool_input: { file_path: "src/main.ts" }, tool_response: { success: true } },
  SessionStart: { hook_event_name: "SessionStart", source: "startup" },
  UserPromptSubmit: { hook_event_name: "UserPromptSubmit", prompt: "Explain this file" },
};

function runHook(hookId: string, input: unknown): HookRun {
  const found = agentOfHook(hookId);
  if (!found) return reject({ kind: "notFound" });
  const command = String((found.hook.handler.raw as Record<string, unknown>).command ?? found.hook.handler.summary);
  const stdin = JSON.stringify(input);
  const blocked = stdin.includes("rm -rf");
  const slow = stdin.includes("sleep");
  return {
    spec: { argv: ["/bin/sh", "-c", command], cwd: CWD, env: [["CLAUDE_PROJECT_DIR", CWD], ["HOOK_EVENT", found.hook.event]], stdin },
    exitCode: slow ? null : blocked ? 2 : 0,
    stdout: slow || blocked ? "" : '{"decision":"approve","reason":"checked by the mock"}\n',
    stderr: blocked ? "blocked: the command deletes recursively\n" : "",
    ms: slow ? 10_000 : 42,
    timedOut: slow,
  };
}

let started = false;
function start(): void {
  if (started) return;
  started = true;
  initDisk();
  installControls();
}

/** Same signature as the real transport's answer. */
export async function handle(request: Request): Promise<unknown> {
  start();
  switch (request.op) {
    case "session":
      return { version: "0.1.0-mock", shell: "serve", readOnly: readOnlyHost(), launchCwd: CWD, home: HOME, os: "macos", pending: pendingIds(), coreRules };
    case "agents":
      return agentInfos;
    case "projects":
      return [
        { path: CWD, exists: true, agents: ["claude", "codex"] },
        { path: `${HOME}/blog`, exists: true, agents: ["claude"] },
        { path: `${HOME}/old-api`, exists: false, agents: [] },
      ];
    case "scan":
      return structuredClone(snapshot);
    case "read_file": {
      const entry = files[request.args.path];
      if (!entry) return reject({ kind: "notFound" });
      return { path: request.args.path, state: entry.state, text: texts[request.args.path] ?? null };
    }
    case "render":
      return render(request.args.agent);
    case "load_texts":
      return request.args.fingerprint === mockState.fingerprint ? { kind: "current", texts: loadTexts(request.args.agent, request.args.file) } : { kind: "stale" };
    case "simulate":
      return simulate(request.args.path);
    case "search":
      return search(request.args.query, request.args.caseSensitive, request.args.regex);
    case "watch":
      if (request.args.since === mockState.fingerprint) await waitForBump(20_000);
      return mockState.fingerprint;
    case "history":
      return history.map((detail) => structuredClone(detail.record));
    case "change": {
      const found = history.find((detail) => detail.record.id === request.args.id);
      return found ? structuredClone(found) : reject({ kind: "notFound" });
    }
    case "plan":
      if (readOnlyHost()) return reject({ kind: "readOnly", why: { kind: "host" } });
      return plan(request.args.edit).preview;
    case "apply":
      if (readOnlyHost()) return reject({ kind: "readOnly", why: { kind: "host" } });
      return apply(request.args.edit, request.args.digest);
    case "hook_sample":
      return SAMPLES[request.args.event] ?? { hook_event_name: request.args.event };
    case "run_hook":
      return runHook(request.args.hook, request.args.input);
  }
}
