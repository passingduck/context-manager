// `plan`, `apply`, `history` and `change` for the mock host, with the answers the real host gives.
import type { AbsPath } from "../gen/AbsPath";
import type { ApplyResult } from "../gen/ApplyResult";
import type { ChangeDetail } from "../gen/ChangeDetail";
import type { Conflict } from "../gen/Conflict";
import type { Edit } from "../gen/Edit";
import type { FileOp } from "../gen/FileOp";
import type { Hook } from "../gen/Hook";
import type { HookDraft } from "../gen/HookDraft";
import type { Preview } from "../gen/Preview";
import type { Warning } from "../gen/Warning";
import { baseOf, controls, DATA_DIR, digestOf, history, hooksIn, agentOfHook, nextId, renderHooks, reject, revOf, ruleFor, sameBase, setText, bump, slotOf } from "./disk";
import { files, snapshot, texts } from "./snapshot";

interface Plan {
  preview: Preview;
  /** Mock only: changes the lists the snapshot keeps (hooks, memory entries) once the files are written. */
  commit: () => void;
  agent: string | null;
}

const dirOf = (path: string) => path.slice(0, path.lastIndexOf("/"));
const baseName = (path: string) => path.slice(path.lastIndexOf("/") + 1);
const slug = (text: string) => text.toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-|-$/g, "");
const treeHash = (dir: string) => revOf(Object.keys(texts).filter((p) => p.startsWith(`${dir}/`)).sort().map((p) => `${p}=${revOf(texts[p]!)}`).join("\n"));

function agentsOf(path: string): string[] {
  return files[path]?.agents ?? [];
}

function warningsFor(ops: FileOp[], before: Record<string, string | null>, agent: string | null): Warning[] {
  const out: Warning[] = [];
  const shared = new Set<string>();
  for (const op of ops) {
    const entry = files[op.path];
    const agents = agentsOf(op.path);
    if (agents.length > 1) agents.forEach((a) => shared.add(a));
    if (entry?.realPath) out.push({ kind: "throughSymlink", target: entry.realPath });
    if (op.action.kind === "write" && agents.includes("gemini")) {
      const old = new Set(before[op.path]?.match(/(?:^|\s)@[\w./~-]+/g) ?? []);
      for (const token of op.action.text.match(/(?:^|\s)@[\w./~-]+/g) ?? []) if (!old.has(token)) out.push({ kind: "newImportToken", raw: token.trim() });
    }
    if (op.path.endsWith("/.codex/config.toml")) out.push({ kind: "agentAlsoWrites", agent: "codex" });
    if (op.path.endsWith("/.claude/settings.local.json")) out.push({ kind: "agentAlsoWrites", agent: "claude" });
    if (op.action.kind === "trashDir" && Object.keys(texts).filter((p) => p.startsWith(`${op.path}/`)).reduce((n, p) => n + texts[p]!.length, 0) > 16 * 1024 * 1024) out.push({ kind: "notRevertible" });
  }
  if (shared.size > 0) out.push({ kind: "sharedFile", agents: [...shared] });
  const takers = new Set(ops.flatMap((op) => agentsOf(op.path)));
  if (agent) takers.add(agent);
  for (const a of takers) out.push({ kind: "takesEffect", agent: a, when: ruleFor(a, "effect.next_session") });
  return out;
}

function finish(summaryEdit: Edit["kind"], agent: string | null, ops: FileOp[], extra: Warning[] = [], conflicts: Conflict[] = [], commit: () => void = () => undefined): Plan {
  const before: Record<string, string | null> = {};
  for (const op of ops) {
    before[op.path] = texts[op.path] ?? null;
    if (op.action.kind === "trashDir") for (const p of Object.keys(texts)) if (p.startsWith(`${op.path}/`)) before[p] = texts[p]!;
  }
  const changeSet = { summary: { edit: { kind: summaryEdit }, agent, targets: ops.map((op) => op.path) }, ops };
  const warnings = [...extra, ...warningsFor(ops, before, agent)];
  return { preview: { digest: digestOf({ changeSet, conflicts: conflicts.map((c) => c.path) }), changeSet, before, warnings, conflicts }, commit, agent };
}

/** The file must be known to the scan and editable. */
function editable(path: AbsPath, allowAbsent = false): void {
  const entry = files[path];
  if (!entry && !allowAbsent) reject({ kind: "outsideBoundary", path });
  if (entry?.policy.kind === "readOnly") reject({ kind: "readOnly", why: { kind: "file", why: entry.policy.why } });
}

function writeOp(path: AbsPath, text: string, expect = baseOf(texts[path] ?? null)): FileOp[] {
  return texts[path] === text ? [] : [{ path, expect, action: { kind: "write", text } }];
}

function conflictFor(path: AbsPath, expected: ReturnType<typeof baseOf>): never {
  const current = texts[path] ?? null;
  return reject({ kind: "conflict", path, expected, found: baseOf(current), current });
}

// ---------- the edits ----------
function planSave(edit: Extract<Edit, { kind: "save" }>): Plan {
  editable(edit.path, edit.base.kind === "absent");
  if (!sameBase(edit.base, baseOf(texts[edit.path] ?? null))) conflictFor(edit.path, edit.base);
  return finish("save", agentsOf(edit.path)[0] ?? null, writeOp(edit.path, edit.text, edit.base));
}

function planTrash(edit: Extract<Edit, { kind: "trash" }>): Plan {
  editable(edit.path);
  const text = texts[edit.path];
  if (text === undefined) return finish("trash", null, []);
  if (edit.base !== revOf(text)) conflictFor(edit.path, { kind: "rev", rev: edit.base });
  const ext = snapshot.agents.flatMap((view) => view.extensions).find((e) => e.file === edit.path && e.extKind === "skill");
  const agent = agentsOf(edit.path)[0] ?? null;
  if (ext) {
    const dir = dirOf(edit.path);
    return finish("trash", agent, [{ path: dir, expect: { kind: "tree", hash: treeHash(dir) }, action: { kind: "trashDir" } }]);
  }
  // A memory topic also loses its index line.
  const store = snapshot.agents.flatMap((view) => view.memory).find((s) => s.entries.some((e) => e.file === edit.path));
  const ops: FileOp[] = [{ path: edit.path, expect: { kind: "rev", rev: edit.base }, action: { kind: "trash" } }];
  if (store?.index && texts[store.index] !== undefined) {
    const without = texts[store.index]!.split("\n").filter((line) => !line.includes(`(${baseName(edit.path)})`)).join("\n");
    ops.push(...writeOp(store.index, without));
  }
  return finish("trash", agent, ops, [], [], () => {
    if (store) store.entries = store.entries.filter((e) => e.file !== edit.path);
  });
}

function newItemTemplate(kind: string, name: string): string {
  switch (kind) {
    case "skill":
      return `---\nname: ${name}\ndescription: Describe when the agent should use this skill\n---\n\nSteps the agent follows.\n`;
    case "rule":
      return `---\npaths:\n  - "src/**"\n---\n\n# ${name}\n\nWrite the rule here.\n`;
    case "subagent":
      return `---\nname: ${name}\ndescription: Describe when to hand work to this agent\n---\n\nYou are a focused helper.\n`;
    case "command":
      return `# ${name}\n\nWhat the command does.\n`;
    default:
      return `# ${name}\n\n`;
  }
}

function planNewItem(edit: Extract<Edit, { kind: "newItem" }>): Plan {
  const slot = slotOf(edit.agent, edit.slot);
  if (!slot) return reject({ kind: "notFound" });
  let path: AbsPath;
  if (slot.place.kind === "file") path = slot.place.path;
  else {
    const name = (edit.name ?? "").trim();
    if (!name || /[/\\]/.test(name) || name.startsWith(".")) return reject({ kind: "invalidEdit", field: "name", why: slot.rule });
    path = slot.place.kind === "namedFile" ? `${slot.place.dir}/${name}.${slot.place.ext}` : `${slot.place.dir}/${name}/${slot.place.file}`;
  }
  const name = edit.name?.trim() || baseName(path).replace(/\.[^.]+$/, "");
  return finish("newItem", edit.agent, writeOp(path, newItemTemplate(slot.newKind, name)));
}

function planAddMemory(edit: Extract<Edit, { kind: "addMemory" }>): Plan {
  const store = snapshot.agents.find((view) => view.agent === edit.agent)?.memory.find((s) => s.id === edit.store);
  if (!store) return reject({ kind: "notFound" });
  if (store.write.kind === "readOnly") return reject({ kind: "readOnly", why: { kind: "rule", rule: store.write.why } });
  const title = edit.title.trim();
  if (!title) return reject({ kind: "invalidEdit", field: "title", why: store.label });
  const dir = store.write.kind === "notesDir" ? store.write.dir : store.root;
  const file = `${dir}/${slug(title) || "note"}.md`;
  const topic = `---\nname: ${slug(title)}\ndescription: ${edit.description}\n---\n\n${edit.body}\n`;
  const ops: FileOp[] = texts[file] === undefined ? writeOp(file, topic, { kind: "absent" }) : [];
  if (store.index) {
    const line = `- [${title}](${baseName(file)}) - ${edit.description}`;
    const old = texts[store.index] ?? "";
    if (!old.includes(`(${baseName(file)})`)) ops.push(...writeOp(store.index, `${old.replace(/\n*$/, "")}\n${line}\n`));
  }
  return finish("addMemory", edit.agent, ops, [], [], () => {
    if (!store.entries.some((e) => e.file === file)) store.entries.push({ file, role: store.write.kind === "notesDir" ? "note" : "topic", indexed: store.index ? true : null, title, description: edit.description });
    else store.entries.forEach((e) => { if (e.file === file) e.indexed = true; });
    setText(file, texts[file] ?? topic, edit.agent);
  });
}

// ---------- hooks ----------
const handlerSummary = (handler: Record<string, unknown>): string => String(handler.command ?? handler.url ?? handler.prompt ?? "");

function hookFrom(base: Hook | null, id: string, file: AbsPath, draft: HookDraft): Hook {
  const handler = draft.handler as Record<string, unknown>;
  const timeout = typeof handler.timeout === "number" ? handler.timeout : null;
  return {
    id,
    source: base?.source ?? { file, at: { kind: "json", pointer: `/hooks/${draft.event}/0/hooks/0` }, scope: { kind: "project", dir: dirOf(file) }, registered: { kind: "sessionStart" } },
    event: draft.event,
    matcher: { raw: draft.matcher, eval: draft.matcher ? { kind: "regex" } : { kind: "all" } },
    handler: { handlerKind: String(handler.type ?? "command"), raw: draft.handler, summary: handlerSummary(handler), timeoutMs: timeout === null ? null : timeout * 1000 },
    state: { kind: "active" },
    toggle: base?.toggle ?? { kind: "native" },
  };
}

function validateDraft(draft: HookDraft): void {
  const handler = draft.handler;
  if (typeof handler !== "object" || handler === null || Array.isArray(handler)) reject({ kind: "invalidEdit", field: "handler", why: "edit.unsupported" });
}

function hookPlan(kind: "addHook" | "updateHook" | "removeHook" | "setHookEnabled", agent: string, file: AbsPath, next: Hook[], commit: () => void): Plan {
  if (!file.endsWith(".json")) return reject({ kind: "invalidEdit", field: "file", why: "edit.unsupported" });
  editable(file, true);
  const extra: Warning[] = agent === "codex" ? [{ kind: "hookTrustReset" }] : [];
  return finish(kind, agent, writeOp(file, renderHooks(file, next)), extra, [], commit);
}

function planAddHook(edit: Extract<Edit, { kind: "addHook" }>): Plan {
  validateDraft(edit.draft);
  const view = snapshot.agents.find((v) => v.agent === edit.agent);
  if (!view) return reject({ kind: "notFound" });
  const id = `h${100 + history.length + view.hooks.length}`;
  const hook = hookFrom(null, id, edit.file, edit.draft);
  return hookPlan("addHook", edit.agent, edit.file, [...hooksIn(edit.file), hook], () => view.hooks.push(hook));
}

function planUpdateHook(edit: Extract<Edit, { kind: "updateHook" }>): Plan {
  const found = agentOfHook(edit.hook);
  if (!found) return reject({ kind: "notFound" });
  if (found.hook.toggle.kind === "unavailable") return reject({ kind: "forbidden", why: found.hook.toggle.why });
  validateDraft(edit.draft);
  const next = hookFrom(found.hook, found.hook.id, found.hook.source.file, edit.draft);
  next.state = found.hook.state;
  const view = snapshot.agents.find((v) => v.agent === found.agent)!;
  return hookPlan("updateHook", found.agent, found.hook.source.file, hooksIn(found.hook.source.file).map((h) => (h.id === edit.hook ? next : h)), () => {
    view.hooks = view.hooks.map((h) => (h.id === edit.hook ? next : h));
  });
}

function planRemoveHook(edit: Extract<Edit, { kind: "removeHook" }>): Plan {
  const found = agentOfHook(edit.hook);
  if (!found) return reject({ kind: "notFound" });
  if (found.hook.toggle.kind === "unavailable") return reject({ kind: "forbidden", why: found.hook.toggle.why });
  const view = snapshot.agents.find((v) => v.agent === found.agent)!;
  return hookPlan("removeHook", found.agent, found.hook.source.file, hooksIn(found.hook.source.file).filter((h) => h.id !== edit.hook), () => {
    view.hooks = view.hooks.filter((h) => h.id !== edit.hook);
  });
}

function planSetEnabled(edit: Extract<Edit, { kind: "setHookEnabled" }>): Plan {
  const found = agentOfHook(edit.hook);
  if (!found) return reject({ kind: "notFound" });
  const { hook, agent } = found;
  if (hook.toggle.kind === "unavailable") return reject({ kind: "forbidden", why: hook.toggle.why });
  const file = hook.source.file;
    const nativeOff = hook.state.kind === "disabled" && hook.state.by.kind === "native";
  if (edit.enabled && hook.state.kind !== "disabled") return finish("setHookEnabled", agent, []);
  if (!edit.enabled && hook.state.kind === "disabled") return finish("setHookEnabled", agent, []);
  const raw = { ...(hook.handler.raw as Record<string, unknown>) };
  let nextState: Hook["state"] = { kind: "active" };
  const extra: Warning[] = agent === "codex" ? [{ kind: "hookTrustReset" }] : [];
  if (!edit.enabled && hook.toggle.kind === "park") {
    nextState = { kind: "disabled", by: { kind: "parked" } };
    extra.push({ kind: "parkedOnThisMachine" });
  } else if (!edit.enabled) {
    raw.disabled = true;
    nextState = { kind: "disabled", by: { kind: "native", setIn: file } };
  } else if (nativeOff) delete raw.disabled;
  const next: Hook = { ...hook, state: nextState, handler: { ...hook.handler, raw: raw as Hook["handler"]["raw"] } };
  const view = snapshot.agents.find((v) => v.agent === agent)!;
  const list = hooksIn(file).map((h) => (h.id === hook.id ? next : h));
  const ops = writeOp(file, renderHooks(file, list));
  if (hook.toggle.kind === "park") ops.push(...writeOp(`${DATA_DIR}/parked/${revOf(file).slice(7)}.json`, JSON.stringify(list.filter((h) => h.state.kind === "disabled").map((h) => h.handler.raw), null, 2) + "\n", baseOf(texts[`${DATA_DIR}/parked/${revOf(file).slice(7)}.json`] ?? null)));
  return finish("setHookEnabled", agent, ops, extra, [], () => {
    view.hooks = view.hooks.map((h) => (h.id === hook.id ? next : h));
  });
}

// ---------- revert ----------
function planRevert(edit: Extract<Edit, { kind: "revert" }>): Plan {
  const detail = history.find((d) => d.record.id === edit.change);
  if (!detail) return reject({ kind: "notFound" });
  const ops: FileOp[] = [];
  const conflicts: Conflict[] = [];
  for (const file of detail.files) {
    const now = texts[file.path] ?? null;
    if (now === file.before) continue;
    if (now !== file.after) {
      conflicts.push({ path: file.path, expected: baseOf(file.after), found: baseOf(now), current: now });
      continue;
    }
    ops.push({ path: file.path, expect: baseOf(now), action: file.before === null ? { kind: "trash" } : { kind: "write", text: file.before } });
  }
  return finish("revert", detail.record.summary.agent as string | null, ops, [], conflicts, () => {
    if (detail.record.state !== "sealed") detail.record.state = "sealed";
  });
}

export function plan(edit: Edit): Plan {
  switch (edit.kind) {
    case "save":
      return planSave(edit);
    case "trash":
      return planTrash(edit);
    case "newItem":
      return planNewItem(edit);
    case "addMemory":
      return planAddMemory(edit);
    case "addHook":
      return planAddHook(edit);
    case "updateHook":
      return planUpdateHook(edit);
    case "removeHook":
      return planRemoveHook(edit);
    case "setHookEnabled":
      return planSetEnabled(edit);
    case "revert":
      return planRevert(edit);
  }
}

export function apply(edit: Edit, digest: string): ApplyResult {
  if (controls.fail) {
    const error = controls.fail;
    controls.fail = null;
    return reject(error);
  }
  const now = plan(edit);
  if (now.preview.changeSet.ops.length === 0 && now.preview.conflicts.length === 0) return { kind: "alreadyApplied" };
  if (controls.stale) {
    controls.stale = false;
    return { kind: "stale", now: now.preview };
  }
  if (now.preview.digest !== digest) return { kind: "stale", now: now.preview };

  const ops = now.preview.changeSet.ops;
  const detail: ChangeDetail = { record: { id: nextId(), at: new Date().toISOString(), cwd: snapshot.cwd, summary: now.preview.changeSet.summary, ops: [], state: "sealed" }, files: [] };
  const done: [AbsPath, string | null][] = [];
  const notTrashed: AbsPath[] = [];
  for (const op of ops) {
    const before = texts[op.path] ?? null;
    if (op.action.kind === "write") {
      detail.files.push({ path: op.path, before, after: op.action.text });
      setText(op.path, op.action.text, now.agent);
      done.push([op.path, revOf(op.action.text)]);
    } else if (op.action.kind === "trash") {
      detail.files.push({ path: op.path, before, after: null });
      setText(op.path, null);
      done.push([op.path, null]);
      if (controls.noTrash) notTrashed.push(op.path);
    } else {
      for (const p of Object.keys(texts).filter((x) => x.startsWith(`${op.path}/`))) {
        detail.files.push({ path: p, before: texts[p]!, after: null });
        setText(p, null);
        done.push([p, null]);
        if (controls.noTrash) notTrashed.push(p);
      }
    }
  }
  now.commit();
  history.push(detail);
  controls.applied.push({ digest, edit });
  bump();
  return { kind: "applied", change: detail.record.id, files: done, notTrashed };
}

