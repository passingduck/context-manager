// Answers every operation from the mock snapshot. Loaded only when the URL has `?mock=1`.
import type { AbsPath } from "../gen/AbsPath";
import type { ApiError } from "../gen/ApiError";
import type { ChangeDetail } from "../gen/ChangeDetail";
import type { ChangeRecord } from "../gen/ChangeRecord";
import type { Load } from "../gen/Load";
import type { RenderedBlock } from "../gen/RenderedBlock";
import type { Request } from "../gen/Request";
import type { SearchHit } from "../gen/SearchHit";
import type { Simulation } from "../gen/Simulation";
import { agentInfos, CWD, files, FINGERPRINT, HOME, snapshot, texts } from "./snapshot";

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

function render(agent: string): RenderedBlock[] {
  const view = snapshot.agents.find((item) => item.agent === agent);
  if (!view) return [];
  return view.session.map((block) => ({
    delivery: block.delivery,
    label: block.label,
    tokens: block.tokens,
    fidelity: block.fidelity,
    text: block.parts
      .map((part) => {
        if (part.kind === "glue") return "";
        const load = view.loads[part.load]!;
        const text = texts[load.file] ?? "";
        return load.status.kind === "truncated" ? text.split("\n").slice(0, load.status.kept).join("\n") : text;
      })
      .join("\n\n"),
  }));
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

const memIndex = Object.keys(files).find((path) => path.endsWith("/memory/MEMORY.md")) ?? "";
const history: ChangeDetail[] = [
  { id: "01JD0000000000000000000004", at: "2026-11-19T09:12:00Z", cwd: CWD, summary: { edit: { kind: "setHookEnabled" }, agent: "claude", targets: [`${CWD}/.claude/settings.local.json`] }, ops: [], state: "pending", before: '{\n  "hooks": {\n    "PostToolUse": [{ "hooks": [{ "type": "command", "command": "./scripts/format-on-save.sh" }] }]\n  }\n}\n', after: '{\n  "hooks": {}\n}\n', path: `${CWD}/.claude/settings.local.json` },
  { id: "01JD0000000000000000000003", at: "2026-11-18T16:40:00Z", cwd: CWD, summary: { edit: { kind: "save" }, agent: "claude", targets: [`${CWD}/CLAUDE.md`] }, ops: [], state: "sealed", before: "# acme-app\n\nA Tauri desktop app.\n", after: (texts[`${CWD}/CLAUDE.md`] ?? "").split("\n").slice(0, 8).join("\n"), path: `${CWD}/CLAUDE.md` },
  { id: "01JD0000000000000000000002", at: "2026-11-17T11:03:00Z", cwd: CWD, summary: { edit: { kind: "addMemory" }, agent: "claude", targets: [memIndex, memIndex.replace("MEMORY.md", "debugging.md")] }, ops: [], state: "sealed", before: null, after: texts[memIndex.replace("MEMORY.md", "debugging.md")] ?? "", path: memIndex.replace("MEMORY.md", "debugging.md") },
  { id: "01JD0000000000000000000001", at: "2026-11-15T08:21:00Z", cwd: CWD, summary: { edit: { kind: "trash" }, agent: "codex", targets: [`${CWD}/docs/old-notes.md`] }, ops: [], state: "needsAttention", before: "# Old notes\n\nThese are stale.\n", after: null, path: `${CWD}/docs/old-notes.md` },
].map(({ before, after, path, ...record }) => ({ record: record as ChangeRecord, files: [{ path, before, after }] }));

function reject(error: ApiError): never {
  throw error;
}

/** Same signature as the real transport's answer. */
export async function handle(request: Request): Promise<unknown> {
  switch (request.op) {
    case "session":
      return { version: "0.1.0-mock", shell: "serve", readOnly: false, launchCwd: CWD, home: HOME, os: "macos", pending: ["01JD0000000000000000000004"] };
    case "agents":
      return agentInfos;
    case "projects":
      return [
        { path: CWD, exists: true, agents: ["claude", "codex"] },
        { path: `${HOME}/blog`, exists: true, agents: ["claude"] },
        { path: `${HOME}/old-api`, exists: false, agents: [] },
      ];
    case "scan":
      return snapshot;
    case "read_file": {
      const entry = files[request.args.path];
      if (!entry) return reject({ kind: "notFound" });
      return { path: request.args.path, state: entry.state, text: texts[request.args.path] ?? null };
    }
    case "render":
      return render(request.args.agent);
    case "simulate":
      return simulate(request.args.path);
    case "search":
      return search(request.args.query, request.args.caseSensitive, request.args.regex);
    case "watch":
      await new Promise((resolve) => setTimeout(resolve, 20_000));
      return FINGERPRINT;
    case "history":
      return history.map((detail) => detail.record);
    case "change": {
      const found = history.find((detail) => detail.record.id === request.args.id);
      return found ?? reject({ kind: "notFound" });
    }
    default:
      return reject({ kind: "io", detail: `${request.op} is not part of the mock` });
  }
}
