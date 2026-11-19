// The mock host's in-memory disk and its history. `texts` from the snapshot module is the disk, so
// reads, search and render see every write. Loaded only when the URL has `?mock=1`.
import type { AbsPath } from "../gen/AbsPath";
import type { ApiError } from "../gen/ApiError";
import type { Base } from "../gen/Base";
import type { ChangeDetail } from "../gen/ChangeDetail";
import type { Hook } from "../gen/Hook";
import type { Load } from "../gen/Load";
import type { Revision } from "../gen/Revision";
import { agentInfos, CWD, files, snapshot, texts } from "./snapshot";
import { CWD as ROOT } from "./texts";

export const DATA_DIR = "/mock-data";
const enc = new TextEncoder();

/** A stand-in for `sha256:<hex>`: any change of text changes it, the same text gives the same value. */
export function revOf(text: string): Revision {
  let a = 0x811c9dc5;
  let b = 0x01000193;
  for (let i = 0; i < text.length; i++) {
    a = Math.imul(a ^ text.charCodeAt(i), 0x01000193) >>> 0;
    b = Math.imul(b + text.charCodeAt(i), 0x85ebca6b) >>> 0;
  }
  return `sha256:${a.toString(16).padStart(8, "0")}${b.toString(16).padStart(8, "0")}`;
}

export const baseOf = (text: string | null): Base => (text === null ? { kind: "absent" } : { kind: "rev", rev: revOf(text) });
export const sameBase = (a: Base, b: Base): boolean => JSON.stringify(a) === JSON.stringify(b);
export const digestOf = (value: unknown): string => revOf(JSON.stringify(value)).replace("sha256:", "plan:");

export function reject(error: ApiError): never {
  throw error;
}

const estimate = (text: string) => Math.max(1, Math.round(text.length / 3.8));
const originalLoads = new Map<string, Load[]>();

/** Rewrites the file entry and the loads that read it after the disk changed. */
export function refreshEntry(path: AbsPath, agent: string | null = null): void {
  const text = texts[path];
  const entry = files[path];
  if (!entry) return;
  entry.state =
    text === undefined
      ? { kind: "absent" }
      : { kind: "present", rev: revOf(text), bytes: enc.encode(text).length, lines: text === "" ? 0 : text.split("\n").length, tokens: text === "" ? 0 : estimate(text), mtimeMs: Date.now() };
  if (agent && !entry.agents.includes(agent)) entry.agents.push(agent);
  for (const view of snapshot.agents) {
    view.loads.forEach((load, index) => {
      if (load.file !== path) return;
      const original = originalLoads.get(view.agent)?.[index];
      if (text === undefined) {
        load.status = { kind: "skipped", skip: { kind: "missing" } };
        load.delivered = null;
      } else if (original) {
        load.status = original.status.kind === "skipped" && original.status.skip.kind === "missing" ? original.status : original.status;
        load.delivered = original.delivered && entry.state.kind === "present" ? { ...original.delivered, bytes: entry.state.bytes, tokens: entry.state.tokens } : original.delivered;
      }
    });
  }
}

export function setText(path: AbsPath, text: string | null, agent: string | null = null): void {
  if (text === null) delete texts[path];
  else texts[path] = text;
  if (!files[path]) {
    files[path] = { display: path.startsWith(`${ROOT}/`) ? `./${path.slice(ROOT.length + 1)}` : path, realPath: null, format: formatOf(path), state: { kind: "absent" }, policy: { kind: "editable" }, agents: [] };
  }
  refreshEntry(path, agent);
}

export function formatOf(path: string): "markdown" | "json" | "jsonc" | "toml" | "text" {
  return path.endsWith(".md") ? "markdown" : path.endsWith(".jsonc") ? "jsonc" : path.endsWith(".json") ? "json" : path.endsWith(".toml") ? "toml" : "text";
}

// ---------- hook files ----------
const parked = (hook: Hook) => hook.state.kind === "disabled" && hook.state.by.kind === "parked";

export function hooksIn(file: AbsPath): Hook[] {
  return snapshot.agents.flatMap((view) => view.hooks).filter((hook) => hook.source.file === file);
}

/** The settings text with its `hooks` key rebuilt from the hook list. Other keys stay. */
export function renderHooks(file: AbsPath, hooks: Hook[]): string {
  let doc: Record<string, unknown> = {};
  try {
    const parsed: unknown = JSON.parse(texts[file] ?? "{}");
    if (typeof parsed === "object" && parsed !== null && !Array.isArray(parsed)) doc = parsed as Record<string, unknown>;
  } catch {
    doc = {};
  }
  const events: Record<string, unknown[]> = {};
  for (const hook of hooks.filter((h) => !parked(h))) {
    const group: Record<string, unknown> = hook.matcher.raw === null ? {} : { matcher: hook.matcher.raw };
    group.hooks = [hook.handler.raw];
    (events[hook.event] ??= []).push(group);
  }
  return `${JSON.stringify({ ...doc, hooks: events }, null, 2)}\n`;
}

export function agentOfHook(id: string): { agent: string; hook: Hook } | undefined {
  for (const view of snapshot.agents) {
    const hook = view.hooks.find((h) => h.id === id);
    if (hook) return { agent: view.agent, hook };
  }
  return undefined;
}

// ---------- history ----------
export const history: ChangeDetail[] = [];
export const pendingIds = (): string[] => history.filter((detail) => detail.record.state !== "sealed").map((detail) => detail.record.id);

let counter = 100;
export const nextId = (): string => `01JD${String(++counter).padStart(22, "0")}`;

// ---------- test controls ----------
export interface MockControls {
  /** Every file on the mock disk. */
  disk: () => Record<string, string>;
  /** Changes a file from outside the page, as a shell would. `null` deletes it. */
  write: (path: string, text: string | null) => void;
  /** The next `apply` fails with this error. */
  failNext: (error: ApiError) => void;
  /** The next `apply` answers `stale` with a fresh preview. */
  staleNext: () => void;
  /** Makes the OS trash unavailable, so `apply` reports `notTrashed`. */
  noTrash: (on: boolean) => void;
  /** Every `apply` so far, newest last. */
  applied: () => { digest: string; edit: unknown }[];
}

export const controls = { fail: null as ApiError | null, stale: false, noTrash: false, applied: [] as { digest: string; edit: unknown }[] };
const waiters: (() => void)[] = [];
export const mockState = { fingerprint: "fp-mock-0001", tick: 1 };
export const bump = (): void => {
  mockState.fingerprint = `fp-mock-${String(++mockState.tick).padStart(4, "0")}`;
  snapshot.fingerprint = mockState.fingerprint;
  waiters.splice(0).forEach((resolve) => resolve());
};
export const waitForBump = (ms: number): Promise<void> =>
  new Promise((resolve) => {
    const done = () => resolve();
    waiters.push(done);
    setTimeout(done, ms);
  });

export function initDisk(): void {
  for (const view of snapshot.agents) originalLoads.set(view.agent, structuredClone(view.loads));
  // Make every JSON hook file match the hooks the snapshot lists, so a hook edit shows only its own change.
  const hookFiles = new Set(snapshot.agents.flatMap((view) => view.hooks).filter((h) => h.source.file.endsWith(".json")).map((h) => h.source.file));
  for (const file of hookFiles) texts[file] = renderHooks(file, hooksIn(file));
  for (const path of Object.keys(files)) refreshEntry(path);
  for (const diagnostic of snapshot.diagnostics) {
    if (diagnostic.fix?.kind === "trash") diagnostic.fix = { ...diagnostic.fix, base: revOf(texts[diagnostic.fix.path] ?? "") };
  }
  const seed = (record: ChangeDetail["record"], path: string, before: string | null, after: string | null): void => {
    history.push({ record, files: [{ path, before, after }] });
  };
  const base = { cwd: CWD, ops: [] };
  const settings = `${CWD}/.claude/settings.local.json`;
  seed({ ...base, id: "01JD0000000000000000000004", at: "2026-11-19T09:12:00Z", summary: { edit: { kind: "setHookEnabled" }, agent: "claude", targets: [settings] }, state: "pending" }, settings, '{\n  "hooks": {\n    "PostToolUse": [{ "hooks": [{ "type": "command", "command": "./scripts/format-on-save.sh" }] }]\n  }\n}\n', '{\n  "hooks": {}\n}\n');
  const claudeMd = `${CWD}/CLAUDE.md`;
  seed({ ...base, id: "01JD0000000000000000000003", at: "2026-11-18T16:40:00Z", summary: { edit: { kind: "save" }, agent: "claude", targets: [claudeMd] }, state: "sealed" }, claudeMd, "# acme-app\n\nA Tauri desktop app.\n", (texts[claudeMd] ?? "").split("\n").slice(0, 8).join("\n"));
  const memDir = Object.keys(files).find((p) => p.endsWith("/memory/MEMORY.md"))?.replace("MEMORY.md", "") ?? "";
  seed({ ...base, id: "01JD0000000000000000000002", at: "2026-11-17T11:03:00Z", summary: { edit: { kind: "addMemory" }, agent: "claude", targets: [`${memDir}debugging.md`] }, state: "sealed" }, `${memDir}debugging.md`, null, texts[`${memDir}debugging.md`] ?? "");
  const old = `${CWD}/docs/old-notes.md`;
  seed({ ...base, id: "01JD0000000000000000000001", at: "2026-11-15T08:21:00Z", summary: { edit: { kind: "trash" }, agent: "codex", targets: [old] }, state: "needsAttention" }, old, "# Old notes\n\nThese are stale.\n", null);
  // The trashed note was edited in between: it exists again with other text.
  setText(old, "# Old notes\n\nThese are stale, but one line was rewritten by hand.\n");
}

export function installControls(): void {
  const api: MockControls = {
    disk: () => ({ ...texts }),
    write: (path, text) => {
      setText(path, text);
      bump();
    },
    failNext: (error) => {
      controls.fail = error;
    },
    staleNext: () => {
      controls.stale = true;
    },
    noTrash: (on) => {
      controls.noTrash = on;
    },
    applied: () => controls.applied,
  };
  (window as unknown as { __cmMock: MockControls }).__cmMock = api;
}

export const slotOf = (agent: string, id: string) => snapshot.agents.find((view) => view.agent === agent)?.slots.find((slot) => slot.id === id);
export const ruleFor = (agent: string, suffix: string): string => agentInfos.find((info) => info.id === agent)?.rules.find((r) => r.id.endsWith(suffix))?.id ?? `${agent}.${suffix}`;
