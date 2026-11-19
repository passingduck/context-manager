// The one apply flow every edit goes through: plan, show the diff and the warnings, confirm, apply.
import { useQueryClient, type QueryClient } from "@tanstack/react-query";
import { createContext, useCallback, useContext, useEffect, useMemo, useState, type ReactNode } from "react";
import { call } from "../api/client";
import { useProject } from "../app/project";
import { CodeEditor, DiffView } from "../components/CodeEditor";
import { Badge, Button, Mono, Modal, PrimaryButton } from "../components/ui";
import type { AbsPath } from "../gen/AbsPath";
import type { ApiError } from "../gen/ApiError";
import type { ApplyResult } from "../gen/ApplyResult";
import type { Base } from "../gen/Base";
import type { Edit } from "../gen/Edit";
import type { Preview } from "../gen/Preview";
import type { Warning } from "../gen/Warning";
import { useI18n } from "../i18n";
import { say, sayWarning } from "../i18n/types";
import { useRoute } from "../lib/route";
import { guessFormat } from "./format";

type Conflicted = Extract<ApiError, { kind: "conflict" }>;
export type Applied = Extract<ApplyResult, { kind: "applied" }>;

export interface Proposal {
  edit: Edit;
  title: string;
  /** One line shown above the diff, such as "the whole skill directory goes". */
  notice?: string;
  /** The text of an editor, for the conflict view to set beside the disk text. */
  mine?: string;
  /** Explained once, before the first plan, and not again. */
  firstUse?: { key: string; title: string; body: string };
  onApplied?: (result: Applied) => void;
  /** The conflict view's "take theirs": the caller drops its own text. */
  onTakeTheirs?: () => void;
  /** The conflict view's "open both": the caller rebases its editor on the disk text. */
  onOpenBoth?: (found: Base, current: string | null) => void;
}

interface Write {
  propose: (proposal: Proposal) => void;
  /** Set when the host is read-only: why every write control is off. */
  blocked: string | null;
}

const Ctx = createContext<Write | null>(null);

export function useWrite(): Write {
  const write = useContext(Ctx);
  if (!write) throw new Error("WriteProvider is missing");
  return write;
}

const isApiError = (error: unknown): error is ApiError => typeof error === "object" && error !== null && "kind" in error;

/** Warnings the operator must tick before the confirm button works. */
const NEEDS_TICK: Warning["kind"][] = ["hookTrustReset", "agentAckReset", "sessionMayOverwrite", "newImportToken", "parkedOnThisMachine", "notRevertible"];

async function refresh(client: QueryClient): Promise<void> {
  await Promise.all(["scan", "file", "history", "session", "change", "render", "simulate"].map((key) => client.invalidateQueries({ queryKey: [key] })));
}

function seen(key: string): boolean {
  try {
    return localStorage.getItem(`context-manager.seen.${key}`) === "1";
  } catch {
    return false;
  }
}

function markSeen(key: string): void {
  try {
    localStorage.setItem(`context-manager.seen.${key}`, "1");
  } catch {
    // Storage can be blocked. The explanation then shows again next time.
  }
}

export function WriteProvider({ children }: { children: ReactNode }) {
  const { t } = useI18n();
  const project = useProject();
  const [open, setOpen] = useState<{ proposal: Proposal; nonce: number } | null>(null);
  const [note, setNote] = useState<string | null>(null);
  const propose = useCallback((proposal: Proposal) => setOpen((old) => ({ proposal, nonce: (old?.nonce ?? 0) + 1 })), []);
  const value = useMemo<Write>(() => ({ propose, blocked: project.readOnly ? t.write.readOnlyHost : null }), [propose, project.readOnly, t]);
  useEffect(() => {
    if (note === null) return;
    const timer = setTimeout(() => setNote(null), 7000);
    return () => clearTimeout(timer);
  }, [note]);
  return (
    <Ctx.Provider value={value}>
      {children}
      {open && <ApplyDialog key={open.nonce} proposal={open.proposal} onClose={() => setOpen(null)} notify={setNote} />}
      {note && (
        <div role="status" className="fixed bottom-4 left-1/2 z-50 max-w-lg -translate-x-1/2 rounded border border-ok bg-ok-bg px-3 py-2 text-ok shadow">
          {note}
        </div>
      )}
    </Ctx.Provider>
  );
}

type Phase =
  | { kind: "intro" }
  | { kind: "planning" }
  | { kind: "review"; preview: Preview; stale: boolean }
  | { kind: "applying"; preview: Preview }
  | { kind: "conflict"; error: Conflicted }
  | { kind: "failed"; error: unknown }
  | { kind: "done"; result: Applied | { kind: "alreadyApplied" } };

function ApplyDialog({ proposal, onClose, notify }: { proposal: Proposal; onClose: () => void; notify: (text: string) => void }) {
  const { t } = useI18n();
  const project = useProject();
  const client = useQueryClient();
  const [, go] = useRoute();
  const [edit, setEdit] = useState<Edit>(proposal.edit);
  const [overwrite, setOverwrite] = useState(false);
  const [acks, setAcks] = useState<ReadonlySet<string>>(new Set());
  const [phase, setPhase] = useState<Phase>(proposal.firstUse && !seen(proposal.firstUse.key) ? { kind: "intro" } : { kind: "planning" });

  const fail = useCallback((error: unknown) => setPhase(isApiError(error) && error.kind === "conflict" ? { kind: "conflict", error } : { kind: "failed", error }), []);

  useEffect(() => {
    if (phase.kind !== "planning") return;
    let live = true;
    call("plan", { cwd: project.cwd, edit })
      .then((preview) => {
        if (!live) return;
        setAcks(new Set());
        setPhase({ kind: "review", preview, stale: false });
      })
      .catch((error: unknown) => live && fail(error));
    return () => {
      live = false;
    };
  }, [phase.kind, edit, project.cwd, fail]);

  const confirm = async (preview: Preview) => {
    setPhase({ kind: "applying", preview });
    try {
      const result = await call("apply", { cwd: project.cwd, edit, digest: preview.digest });
      if (result.kind === "stale") {
        setAcks(new Set());
        setPhase({ kind: "review", preview: result.now, stale: true });
        return;
      }
      await refresh(client);
      if (result.kind === "alreadyApplied") {
        setPhase({ kind: "done", result });
        return;
      }
      proposal.onApplied?.(result);
      if (result.notTrashed.length > 0) setPhase({ kind: "done", result });
      else {
        notify(t.write.applied(result.files.length));
        onClose();
      }
    } catch (error) {
      fail(error);
    }
  };

  const title = proposal.title;
  const busy = phase.kind === "planning" || phase.kind === "applying";
  const cancel = () => {
    if (phase.kind !== "applying") onClose();
  };

  let body: ReactNode = null;
  let footer: ReactNode = <Button onClick={cancel}>{t.write.cancel}</Button>;

  switch (phase.kind) {
    case "intro": {
      const intro = proposal.firstUse!;
      body = (
        <section className="rounded border border-info bg-info-bg p-3 text-info">
          <h3 className="font-semibold">{intro.title}</h3>
          <p className="mt-1 text-fg">{intro.body}</p>
        </section>
      );
      footer = (
        <>
          <Button onClick={cancel}>{t.write.cancel}</Button>
          <PrimaryButton
            onClick={() => {
              markSeen(intro.key);
              setPhase({ kind: "planning" });
            }}
          >
            {t.write.continue}
          </PrimaryButton>
        </>
      );
      break;
    }
    case "planning":
      body = <p className="text-muted">{t.write.planning}</p>;
      break;
    case "applying":
      body = <p className="text-muted">{t.write.applying}</p>;
      break;
    case "review": {
      const { preview } = phase;
      const warnings = preview.warnings.map((warning, index) => ({ warning, key: `w${index}`, tick: NEEDS_TICK.includes(warning.kind) }));
      const ticks = warnings.filter((w) => w.tick).map((w) => w.key);
      const ready = ticks.every((key) => acks.has(key));
      const empty = preview.changeSet.ops.length === 0;
      const refused = preview.conflicts.length > 0;
      body = (
        <ReviewBody preview={preview} stale={phase.stale} overwrite={overwrite} notice={proposal.notice} acks={acks} setAcks={setAcks} warnings={warnings} onOpenFile={(path) => { onClose(); go({ sel: `file:${path}`, ag: "", ln: 0 }); }} />
      );
      footer = (
        <>
          {!refused && !ready && !empty && <span className="mr-auto text-warn">{t.write.tickFirst}</span>}
          <Button onClick={cancel}>{t.write.cancel}</Button>
          {!refused && (
            <PrimaryButton disabled={!ready || empty} onClick={() => void confirm(preview)}>
              {t.write.confirmBy[preview.changeSet.summary.edit.kind]}
            </PrimaryButton>
          )}
        </>
      );
      break;
    }
    case "conflict": {
      const { error } = phase;
      body = <ConflictView error={error} mine={edit.kind === "save" && edit.path === error.path ? (proposal.mine ?? edit.text) : null} />;
      footer = (
        <ConflictActions
          canKeepMine={edit.kind === "save" && edit.path === error.path}
          onTakeTheirs={() => {
            proposal.onTakeTheirs?.();
            void refresh(client);
            onClose();
          }}
          onKeepMine={() => {
            if (edit.kind !== "save") return;
            setEdit({ ...edit, base: error.found });
            setOverwrite(true);
            setPhase({ kind: "planning" });
          }}
          onOpenBoth={() => {
            proposal.onOpenBoth?.(error.found, error.current);
            void refresh(client);
            onClose();
          }}
          onClose={() => {
            void refresh(client);
            onClose();
          }}
          hasEditor={proposal.mine !== undefined}
        />
      );
      break;
    }
    case "failed":
      body = <Failure error={phase.error} />;
      footer = <Button onClick={cancel}>{t.write.close}</Button>;
      break;
    case "done": {
      body =
        phase.result.kind === "alreadyApplied" ? (
          <p className="rounded border border-info bg-info-bg p-3 text-info">{t.write.alreadyApplied}</p>
        ) : (
          <div className="space-y-2">
            <p className="rounded border border-ok bg-ok-bg p-3 text-ok">{t.write.applied(phase.result.files.length)}</p>
            {phase.result.notTrashed.length > 0 && (
              <section className="rounded border border-warn bg-warn-bg p-3">
                <h3 className="font-semibold text-warn">{t.write.notTrashedTitle}</h3>
                <p className="mt-1">{t.write.notTrashedBody}</p>
                <ul className="mt-1">
                  {phase.result.notTrashed.map((path) => (
                    <li key={path}><Mono className="break-all">{project.display(path)}</Mono></li>
                  ))}
                </ul>
              </section>
            )}
          </div>
        );
      footer = <PrimaryButton onClick={onClose}>{t.write.close}</PrimaryButton>;
      break;
    }
  }

  return (
    <Modal title={title} label={title} onClose={cancel} footer={busy ? <Button disabled>{t.write.cancel}</Button> : footer} layer={5} wide>
      {body}
    </Modal>
  );
}

interface ReviewProps {
  preview: Preview;
  stale: boolean;
  overwrite: boolean;
  notice: string | undefined;
  acks: ReadonlySet<string>;
  setAcks: (next: ReadonlySet<string>) => void;
  warnings: { warning: Warning; key: string; tick: boolean }[];
  onOpenFile: (path: AbsPath) => void;
}

function ReviewBody({ preview, stale, overwrite, notice, acks, setAcks, warnings, onOpenFile }: ReviewProps) {
  const { t, d } = useI18n();
  const project = useProject();
  const toggle = (key: string) => {
    const next = new Set(acks);
    if (next.has(key)) next.delete(key);
    else next.add(key);
    setAcks(next);
  };
  const ctx = { name: project.name, rule: project.rule, d };
  const empty = preview.changeSet.ops.length === 0;
  const refused = preview.conflicts.length > 0;
  return (
    <div className="space-y-3">
      {stale && (
        <p role="alert" className="rounded border border-warn bg-warn-bg p-2.5 text-warn">
          <b>{t.write.staleTitle}</b> {t.write.staleBody}
        </p>
      )}
      {overwrite && (
        <p role="alert" className="rounded border border-bad bg-bad-bg p-2.5 text-bad">
          <b>{t.write.overwriteTitle}</b> {t.write.overwriteBody}
        </p>
      )}
      {notice && <p className="rounded border border-warn bg-warn-bg p-2.5 text-warn">{notice}</p>}
      {empty && preview.conflicts.length === 0 && <p className="rounded border border-info bg-info-bg p-2.5 text-info">{t.write.nothingChanges}</p>}

      {preview.conflicts.length > 0 && (
        <section aria-label={t.write.conflictsTitle} className="rounded border border-bad bg-bad-bg p-2.5">
          <h3 className="font-semibold text-bad">{t.write.conflictsTitle}</h3>
          <p className="mt-0.5">{t.write.conflictsBody}</p>
          <p className="mt-1 font-semibold">{t.write.conflictsRefused}</p>
          <ul className="mt-1.5 space-y-1">
            {preview.conflicts.map((conflict) => (
              <li key={conflict.path}>
                <div className="flex items-center gap-2">
                  <Mono className="min-w-0 flex-1 break-all font-semibold">{project.display(conflict.path)}</Mono>
                  {conflict.current !== null && <Button onClick={() => onOpenFile(conflict.path)}>{t.write.conflictOpen}</Button>}
                </div>
                <details className="mt-0.5">
                  <summary className="cursor-pointer text-[12px]">{conflict.current === null ? t.write.conflictAbsent : t.write.conflictShow}</summary>
                  {conflict.current !== null && (
                    <div className="mt-1 h-40 overflow-hidden rounded border border-line bg-bg">
                      <CodeEditor key={conflict.path} text={conflict.current} format={guessFormat(conflict.path, project.snapshot.files[conflict.path]?.format)} />
                    </div>
                  )}
                </details>
              </li>
            ))}
          </ul>
        </section>
      )}

      {!refused && warnings.length > 0 && (
        <section aria-label={t.write.warningsTitle}>
          <h3 className="mb-1 text-[12px] font-semibold uppercase tracking-wide text-faint">{t.write.warningsTitle}</h3>
          <ul className="space-y-1.5">
            {warnings.map(({ warning, key, tick }) => (
              <li key={key} className={`rounded border p-2 ${tick ? "border-warn bg-warn-bg" : "border-line bg-subtle"}`}>
                <p className="flex items-start gap-2">
                  {tick && <Badge tone="warn">{t.write.needsAttention}</Badge>}
                  <span>{sayWarning(t.warning, warning, ctx)}</span>
                </p>
                {tick && <Tick checked={acks.has(key)} onChange={() => toggle(key)} label={t.write.warningTick} />}
              </li>
            ))}
          </ul>
        </section>
      )}

      {!refused && (
        <section aria-label={t.write.diffTitle}>
          <h3 className="mb-1 text-[12px] font-semibold uppercase tracking-wide text-faint">{t.write.diffTitle}</h3>
          <Diffs preview={preview} />
        </section>
      )}
    </div>
  );
}

function Tick({ checked, onChange, label }: { checked: boolean; onChange: () => void; label: string }) {
  return (
    <label className="mt-1.5 flex cursor-pointer items-center gap-2 font-semibold">
      <input type="checkbox" checked={checked} onChange={onChange} className="size-4 accent-[var(--accent)]" />
      {label}
    </label>
  );
}

interface Entry {
  path: AbsPath;
  before: string | null;
  after: string | null;
  dir: boolean;
}

function entriesOf(preview: Preview): Entry[] {
  return preview.changeSet.ops.flatMap<Entry>((op) => {
    const before = preview.before[op.path] ?? null;
    switch (op.action.kind) {
      case "write":
        return [{ path: op.path, before, after: op.action.text, dir: false }];
      case "trash":
        return [{ path: op.path, before, after: null, dir: false }];
      case "trashDir":
        return [
          { path: op.path, before: null, after: null, dir: true },
          ...Object.keys(preview.before)
            .filter((path) => path.startsWith(`${op.path}/`))
            .map((path) => ({ path, before: preview.before[path] ?? null, after: null, dir: false })),
        ];
    }
  });
}

function Diffs({ preview }: { preview: Preview }) {
  const { t } = useI18n();
  const project = useProject();
  const entries = entriesOf(preview);
  return (
    <div className="space-y-3">
      {entries.map((entry) => (
        <div key={entry.path}>
          <h4 className="mb-1 flex flex-wrap items-center gap-2">
            <Mono className="break-all font-semibold">{project.display(entry.path)}</Mono>
            {entry.dir && <Badge tone="bad">{t.write.wholeDir}</Badge>}
            {!entry.dir && entry.before === null && entry.after !== null && <Badge tone="ok">{t.write.newFile}</Badge>}
            {!entry.dir && entry.after === null && <Badge tone="bad">{t.write.toTrash}</Badge>}
          </h4>
          {!entry.dir && (
            <>
              <div className="mb-0.5 grid grid-cols-2 gap-4 text-[11px] text-faint">
                <span>{t.history.before}</span>
                <span>{t.history.after}</span>
              </div>
              <DiffView before={entry.before ?? ""} after={entry.after ?? ""} format={guessFormat(entry.path, project.snapshot.files[entry.path]?.format)} />
            </>
          )}
        </div>
      ))}
    </div>
  );
}

function ConflictView({ error, mine }: { error: Conflicted; mine: string | null }) {
  const { t } = useI18n();
  const project = useProject();
  const format = guessFormat(error.path, project.snapshot.files[error.path]?.format);
  return (
    <div className="space-y-3">
      <section role="alert" className="rounded border border-bad bg-bad-bg p-3">
        <h3 className="font-semibold text-bad">{t.write.conflictTitle}</h3>
        <p className="mt-0.5">
          <Mono className="break-all font-semibold">{project.display(error.path)}</Mono> {t.write.conflictBody}
        </p>
      </section>
      {error.current === null && <p className="rounded border border-warn bg-warn-bg p-2.5 text-warn">{t.write.conflictAbsent}</p>}
      {mine !== null ? (
        <>
          <div className="grid grid-cols-2 gap-4 text-[12px] font-semibold">
            <span>{t.write.theirs}</span>
            <span>{t.write.mine}</span>
          </div>
          <DiffView before={error.current ?? ""} after={mine} format={format} />
        </>
      ) : (
        error.current !== null && (
          <div>
            <p className="mb-1 text-[12px] font-semibold">{t.write.theirs}</p>
            <div className="h-60 overflow-hidden rounded border border-line">
              <CodeEditor text={error.current} format={format} />
            </div>
          </div>
        )
      )}
    </div>
  );
}

function ConflictActions({ canKeepMine, hasEditor, onTakeTheirs, onKeepMine, onOpenBoth, onClose }: { canKeepMine: boolean; hasEditor: boolean; onTakeTheirs: () => void; onKeepMine: () => void; onOpenBoth: () => void; onClose: () => void }) {
  const { t } = useI18n();
  if (!canKeepMine && !hasEditor) return <Button onClick={onClose}>{t.write.close}</Button>;
  return (
    <div className="grid w-full grid-cols-3 gap-3">
      <div>
        <Button className="w-full" onClick={onTakeTheirs}>{t.write.takeTheirs}</Button>
        <p className="mt-1 text-[11px] text-muted">{t.write.takeTheirsHint}</p>
      </div>
      <div>
        <Button className="w-full" onClick={onKeepMine}>{t.write.keepMine}</Button>
        <p className="mt-1 text-[11px] text-muted">{t.write.keepMineHint}</p>
      </div>
      <div>
        <PrimaryButton className="w-full" onClick={onOpenBoth}>{t.write.openBoth}</PrimaryButton>
        <p className="mt-1 text-[11px] text-muted">{t.write.openBothHint}</p>
      </div>
    </div>
  );
}

/** What went wrong, and which rule the host cites for it. */
export function Failure({ error }: { error: unknown }) {
  const { t, d } = useI18n();
  const project = useProject();
  if (!isApiError(error)) return <p role="alert" className="rounded border border-bad bg-bad-bg p-3 text-bad">{String(error)}</p>;
  const why = error.kind === "readOnly" || error.kind === "invalidEdit" || error.kind === "forbidden" ? project.rule(error.why) : null;
  return (
    <section role="alert" className="rounded border border-bad bg-bad-bg p-3">
      <h3 className="font-semibold text-bad">{t.write.failedTitle}</h3>
      <p className="mt-0.5">{say(t.apiError, error, d)}</p>
      {why && <p className="mt-1 text-muted">{why}</p>}
      <p className="mt-1 text-muted">{t.write.nothingWritten}</p>
    </section>
  );
}
