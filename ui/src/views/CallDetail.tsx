import { useQueryClient } from "@tanstack/react-query";
import { memo, useCallback, useEffect, useMemo, useRef, useState } from "react";
import { usePieceText, useStepPieces, useTrace } from "../api/queries";
import { useProject } from "../app/project";
import { Badge, Button, ErrorNote, Loading, Mono } from "../components/ui";
import type { PieceKind } from "../gen/PieceKind";
import type { Step } from "../gen/Step";
import type { Trace } from "../gen/Trace";
import type { TraceKey } from "../gen/TraceKey";
import type { TracePiece } from "../gen/TracePiece";
import { useI18n } from "../i18n";
import * as fmt from "../lib/format";
import { exact } from "../lib/prefix";
import { NO_SESSION, type Route } from "../lib/route";
import { cacheLoss, growth, KINDS, kindColor, parsePick, pickText, placeOf, promptOf, SHARED_HATCH, type Picked } from "../lib/trace";
import { CACHE_FILL } from "./SessionChart";
import { LossNote } from "./LossNote";
import { useTraceKey } from "./SessionsView";

type Go = (patch: Partial<Route>, replace?: boolean) => void;

export function CallDetail({ route, go }: { route: Route; go: Go }) {
  const { t } = useI18n();
  const project = useProject();
  const key = useTraceKey(route);
  const trace = useTrace(project.cwd, key);
  if (!key || !trace.data || route.call < 1 || route.call > trace.data.steps.length) {
    return (
      <div className="grid h-full place-items-center p-6 text-center text-muted">
        <p className="max-w-xs">{!key ? t.sessions.pickSession : trace.isPending ? t.loading : t.sessions.selectCall}</p>
      </div>
    );
  }
  return <Call key={`${key.id}/${key.part}/${route.call}`} trace={trace.data} keyOf={key} index={route.call - 1} route={route} go={go} />;
}

function Call({ trace, keyOf, index, route, go }: { trace: Trace; keyOf: TraceKey; index: number; route: Route; go: Go }) {
  const { t, lang } = useI18n();
  const project = useProject();
  const client = useQueryClient();
  const step = trace.steps[index]!;
  const pieces = useStepPieces(project.cwd, keyOf, trace.read, step.sent ? index : null);

  // A trace that changed since the client read it answers `stale`. The client reads it again and keeps the selection.
  const stale = pieces.data?.kind === "stale";
  useEffect(() => {
    if (stale) void client.invalidateQueries({ queryKey: ["trace", project.cwd, keyOf] });
  }, [stale, client, project.cwd, keyOf]);

  const list = pieces.data?.kind === "current" ? pieces.data.pieces : null;
  const loss = cacheLoss(trace, index);
  const first = loss?.why.kind === "changed" ? (list?.[loss.why.at] ?? null) : null;
  const picked = parsePick(route.pc);

  return (
    <div data-testid="call" data-call={index + 1} className="h-full space-y-3 overflow-y-auto px-3 py-2">
      <header>
        <h2 className="text-[13px] font-semibold">{t.sessions.call.title(index + 1, trace.steps.length)}</h2>
        <p className="text-muted">
          {fmt.clock(step.at, lang)}, {step.model ?? t.sessions.tip.none}
        </p>
      </header>
      <Usage trace={trace} index={index} />
      <Exactness step={step} />
      {loss && (
        <div data-testid="loss-panel" className="rounded border border-bad bg-bad-bg px-2 py-1.5 text-bad">
          <p className="font-semibold">{t.sessions.lost.badge}</p>
          <LossNote loss={loss} first={first ? pieceName(first, t) : null} testid="loss-note" />
        </div>
      )}
      {!step.sent && <p className="text-muted">{t.sessions.call.noRequest}</p>}
      {step.sent && pieces.isPending && <Loading />}
      {pieces.isError && <ErrorNote error={pieces.error} onRetry={() => void pieces.refetch()} />}
      {step.sent && list && <Request step={step} pieces={list} index={index} keyOf={keyOf} trace={trace} picked={picked} go={go} />}
      <Reply step={step} index={index} keyOf={keyOf} trace={trace} picked={picked} go={go} />
    </div>
  );
}

type Catalog = ReturnType<typeof useI18n>["t"];

function placeName(place: TracePiece["place"], t: Catalog): string {
  const n = placeOf(place).n;
  return place.kind === "message" ? t.sessions.place.message(n, t.sessions.role[place.role]) : place.kind === "system" ? t.sessions.place.system(n) : t.sessions.place.tool(n);
}

function pieceName(piece: TracePiece, t: Catalog): string {
  return `${placeName(piece.place, t)}, ${t.sessions.kind[piece.kind]}${piece.label ? `, ${piece.label}` : ""}`;
}

function Usage({ trace, index }: { trace: Trace; index: number }) {
  const { t } = useI18n();
  const u = trace.steps[index]!.usage;
  const c = t.sessions.call;
  if (!u) return <p data-testid="usage" className="text-muted">{t.sessions.tip.noUsage}</p>;
  const prompt = promptOf(u);
  const grew = growth(trace, index);
  const part = (v: number, fill: string) => (v > 0 ? <span className="block h-full" style={{ width: `${(v / prompt) * 100}%`, background: fill }} /> : null);
  return (
    <section data-testid="usage" aria-label={c.usage}>
      <h3 className="text-[12px] font-semibold">{c.usage}</h3>
      <div aria-hidden className="my-1 flex h-3 w-full overflow-hidden rounded-sm bg-sunken">
        {part(u.cacheRead, CACHE_FILL.read)}
        {part(u.cacheWrite, CACHE_FILL.write)}
        {part(u.input, CACHE_FILL.input)}
      </div>
      <dl className="grid grid-cols-[repeat(3,minmax(0,1fr))] gap-x-3 gap-y-0.5 tabular-nums">
        <Num label={t.sessions.tip.read} value={exact(u.cacheRead)} />
        <Num label={t.sessions.tip.written} value={exact(u.cacheWrite)} />
        <Num label={t.sessions.tip.uncached} value={exact(u.input)} />
        <Num label={c.prompt} value={exact(prompt)} />
        <Num label={t.sessions.tip.output} value={exact(u.output)} />
        <Num label={t.sessions.tip.grew} value={grew === null ? t.sessions.tip.none : `${grew > 0 ? "+" : ""}${exact(grew)}`} />
        {u.thinking !== null && <Num label={c.thinking} value={exact(u.thinking)} />}
      </dl>
    </section>
  );
}

function Num({ label, value }: { label: string; value: string }) {
  return (
    <div>
      <dt className="text-[11px] text-muted">{label}</dt>
      <dd>{value}</dd>
    </div>
  );
}

function Exactness({ step }: { step: Step }) {
  const { t } = useI18n();
  const c = t.sessions.call;
  const [tone, text, which] = !step.sent ? (["neutral", c.notRecorded, "none"] as const) : step.sent.exact ? (["ok", c.exact, "exact"] as const) : (["warn", c.inexact, "inexact"] as const);
  return (
    <p data-testid="exactness" data-exactness={which}>
      <Badge tone={tone}>{text}</Badge>
    </p>
  );
}

const ROW_H = 40;
const LIST_H = 360;
const OVERSCAN = 20;

function Request({ step, pieces, index, keyOf, trace, picked, go }: { step: Step; pieces: TracePiece[]; index: number; keyOf: TraceKey; trace: Trace; picked: Picked | null; go: Go }) {
  const { t } = useI18n();
  const sent = step.sent!;
  const [kind, setKind] = useState<PieceKind | "">("");
  const [onlyFresh, setOnlyFresh] = useState(false);
  const shared = sent.shared;
  const present = useMemo(() => KINDS.filter((k) => pieces.some((p) => p.kind === k)), [pieces]);
  const rows = pieces.map((piece, i) => ({ piece, i })).filter(({ piece, i }) => (kind === "" || piece.kind === kind) && (!onlyFresh || i >= shared));
  const select = useCallback((i: number) => go({ pc: pickText("sent", i) }), [go]);
  const open = picked?.side === "sent" ? picked.piece : null;
  // Only the rows in view exist. A call can send thousands of pieces.
  const listRef = useRef<HTMLDivElement>(null);
  const [top, setTop] = useState(0);
  const first = Math.max(0, Math.floor(top / ROW_H) - OVERSCAN);
  const last = Math.min(rows.length, Math.ceil((top + LIST_H) / ROW_H) + OVERSCAN);
  const openAt = open === null ? -1 : rows.findIndex((r) => r.i === open);
  useEffect(() => {
    const box = listRef.current;
    if (!box || openAt < 0) return;
    if (openAt * ROW_H < box.scrollTop) box.scrollTop = openAt * ROW_H;
    else if ((openAt + 1) * ROW_H > box.scrollTop + LIST_H) box.scrollTop = (openAt + 1) * ROW_H - LIST_H;
  }, [openAt]);

  return (
    <>
      <Strip pieces={pieces} sharedCount={shared} marks={sent.marks} open={open} select={select} present={present} />
      <section aria-label={t.sessions.call.piecesTitle}>
        <div className="mb-1 flex flex-wrap items-center gap-x-3 gap-y-1">
          <h3 className="text-[12px] font-semibold">{t.sessions.call.piecesTitle}</h3>
          <label className="flex items-center gap-1 text-[12px]">
            <span className="text-muted">{t.sessions.call.kindFilter}</span>
            <select data-testid="kind-filter" value={kind} onChange={(e) => setKind(e.target.value as PieceKind | "")} className="rounded border border-line bg-bg px-1 py-0.5">
              <option value="">{t.sessions.call.allKinds}</option>
              {present.map((k) => (
                <option key={k} value={k}>
                  {t.sessions.kind[k]}
                </option>
              ))}
            </select>
          </label>
          <label className="flex items-center gap-1 text-[12px]">
            <input data-testid="only-fresh" type="checkbox" checked={onlyFresh} onChange={(e) => setOnlyFresh(e.target.checked)} />
            {t.sessions.call.onlyFresh}
          </label>
          <span className="text-[11px] text-muted" data-testid="piece-count">{t.sessions.call.shown(rows.length, pieces.length)}</span>
        </div>
        <div className="grid grid-cols-[minmax(0,9rem)_minmax(0,6.5rem)_minmax(0,1fr)_4rem_3.5rem] gap-x-2 border-b border-line px-1 pb-0.5 text-[11px] text-muted">
          <span>{t.sessions.call.colPlace}</span>
          <span>{t.sessions.call.colKind}</span>
          <span>{t.sessions.call.colLabel}</span>
          <span className="text-right" title={t.sessions.estimatedTokens}>{t.sessions.call.colTokens}</span>
          <span>{t.sessions.call.colState}</span>
        </div>
        <div ref={listRef} data-testid="pieces" onScroll={(e) => setTop(e.currentTarget.scrollTop)} className="overflow-y-auto" style={{ height: Math.min(LIST_H, Math.max(rows.length, 1) * ROW_H) }}>
          <ul className="relative" style={{ height: rows.length * ROW_H }}>
            {rows.slice(first, last).map(({ piece, i }, k) => (
              <PieceRow key={i} piece={piece} index={i} fresh={i >= shared} selected={open === i} onSelect={select} top={(first + k) * ROW_H} />
            ))}
          </ul>
        </div>
      </section>
      {open !== null && pieces[open] && <PieceText piece={pieces[open]} pick={{ side: "sent", piece: open }} index={index} keyOf={keyOf} trace={trace} go={go} />}
    </>
  );
}

function Strip({ pieces, sharedCount, marks, open, select, present }: { pieces: TracePiece[]; sharedCount: number; marks: number[]; open: number | null; select: (i: number) => void; present: PieceKind[] }) {
  const { t } = useI18n();
  const total = Math.max(1, pieces.reduce((n, p) => n + p.tokens, 0));
  const markSet = new Set(marks);
  return (
    <section aria-label={t.sessions.call.stripTitle} className="pt-2">
      <h3 className="text-[12px] font-semibold">{t.sessions.call.stripTitle}</h3>
      <p className="mb-1 text-[11px] text-muted">{t.sessions.call.stripNote(exact(total))}</p>
      <div className="overflow-x-auto pb-1">
        <div data-testid="strip" onClick={(e) => { const at = (e.target as HTMLElement).closest("[data-index]")?.getAttribute("data-index"); if (at) select(Number(at)); }} className="relative flex h-7 bg-sunken" style={{ minWidth: "100%", width: Math.max(0, pieces.length * 3) }}>
          {pieces.map((piece, i) => (
            <StripPiece key={i} piece={piece} index={i} fresh={i >= sharedCount} mark={markSet.has(i)} freshStart={i === sharedCount && sharedCount > 0} selected={open === i} />
          ))}
        </div>
      </div>
      <ul className="mt-1 flex flex-wrap gap-x-3 gap-y-0.5 text-[11px] text-muted">
        {present.map((kind) => (
          <li key={kind} className="flex items-center gap-1">
            <span aria-hidden className="inline-block h-2.5 w-4" style={{ background: kindColor(kind) }} />
            {t.sessions.kind[kind]}
          </li>
        ))}
        <li className="flex items-center gap-1">
          <span aria-hidden className="inline-block h-2.5 w-4 bg-fg-faint opacity-55" style={{ backgroundImage: SHARED_HATCH }} />
          {t.sessions.call.legendShared}
        </li>
        <li className="flex items-center gap-1">
          <span aria-hidden className="inline-block h-3 w-0.5 bg-fg" />
          {t.sessions.call.legendMark}
        </li>
        <li className="flex items-center gap-1">
          <span aria-hidden className="inline-block h-3 border-l-2 border-dashed border-accent" />
          {t.sessions.call.legendFresh}
        </li>
      </ul>
    </section>
  );
}

const StripPiece = memo(function StripPiece({ piece, index, fresh, mark, freshStart, selected }: { piece: TracePiece; index: number; fresh: boolean; mark: boolean; freshStart: boolean; selected: boolean }) {
  const { t } = useI18n();
  const name = pieceName(piece, t);
  return (
    <span
      data-testid="strip-piece"
      data-index={index}
      data-fresh={fresh}
      data-mark={mark}
      title={`${name}, ${exact(piece.tokens)} ${t.sessions.estimatedTokens}, ${fresh ? t.sessions.call.fresh : t.sessions.call.shared}`}
      className="relative block h-full cursor-default"
      style={{ flex: `${piece.tokens} 1 0`, minWidth: 2, background: fresh ? kindColor(piece.kind) : `${SHARED_HATCH}, ${kindColor(piece.kind)}`, opacity: fresh ? 1 : 0.55, boxShadow: selected ? "inset 0 0 0 2px var(--fg)" : "inset -1px 0 0 var(--bg)" }}
    >
      {mark && <span aria-hidden data-testid="cache-mark" className="absolute -top-2 right-0 h-[calc(100%+8px)] w-0.5 bg-fg" />}
      {freshStart && <span aria-hidden data-testid="fresh-start" className="absolute -top-2 left-0 h-[calc(100%+8px)] border-l-2 border-dashed border-accent" />}
    </span>
  );
});

const PieceRow = memo(function PieceRow({ piece, index, fresh, selected, onSelect, top }: { piece: TracePiece; index: number; fresh: boolean; selected: boolean; onSelect: (i: number) => void; top: number }) {
  const { t } = useI18n();
  const place = placeName(piece.place, t);
  return (
    <li className="absolute inset-x-0" style={{ top, height: ROW_H }}>
      <button
        type="button"
        data-testid="piece-row"
        data-index={index}
        data-fresh={fresh}
        data-kind={piece.kind}
        aria-pressed={selected}
        onClick={() => onSelect(index)}
        className={`block h-full w-full border-l-2 px-1 py-0.5 text-left hover:bg-subtle ${selected ? "border-accent bg-accent-bg" : "border-transparent"}`}
      >
        <span className="grid grid-cols-[minmax(0,9rem)_minmax(0,6.5rem)_minmax(0,1fr)_4rem_3.5rem] items-baseline gap-x-2">
          <span className="truncate">{place}</span>
          <span className="flex items-center gap-1 truncate">
            <span aria-hidden className="inline-block size-2 shrink-0" style={{ background: kindColor(piece.kind) }} />
            {t.sessions.kind[piece.kind]}
          </span>
          <span className="truncate font-mono text-[12px]">{piece.label ?? ""}</span>
          <span className="text-right tabular-nums">{exact(piece.tokens)}</span>
          <span className={fresh ? "font-semibold" : "text-muted"}>{fresh ? t.sessions.call.fresh : t.sessions.call.shared}</span>
        </span>
        <span className="block truncate font-mono text-[11px] text-muted">{piece.head}</span>
      </button>
    </li>
  );
});

function PieceText({ piece, pick, index, keyOf, trace, go }: { piece: { file?: TracePiece["file"]; kind: PieceKind; label: string | null }; pick: Picked; index: number; keyOf: TraceKey; trace: Trace; go: Go }) {
  const { t } = useI18n();
  const project = useProject();
  const client = useQueryClient();
  const ref = useMemo(() => (pick.side === "sent" ? ({ kind: "sent", piece: pick.piece } as const) : ({ kind: "reply", piece: pick.piece } as const)), [pick.side, pick.piece]);
  const text = usePieceText(project.cwd, keyOf, trace.read, index, ref);
  const stale = text.data?.kind === "stale";
  useEffect(() => {
    if (stale) void client.invalidateQueries({ queryKey: ["trace", project.cwd, keyOf] });
  }, [stale, client, project.cwd, keyOf]);
  const file = piece.file ?? null;
  const known = file !== null && project.snapshot.files[file] !== undefined;
  return (
    <section data-testid="piece-detail" aria-label={t.sessions.call.textTitle}>
      <h3 className="text-[12px] font-semibold">
        {t.sessions.kind[piece.kind]}
        {piece.label ? <Mono className="ml-2 font-normal text-muted">{piece.label}</Mono> : null}
      </h3>
      {file && (
        <p className="mb-1 flex flex-wrap items-center gap-2">
          <Mono data-testid="piece-file">{project.display(file)}</Mono>
          {known && (
            <Button data-testid="piece-file-link" onClick={() => go({ view: "context", sel: `file:${file}`, ag: keyOf.agent, ...NO_SESSION })}>
              {t.sessions.call.openFile}
            </Button>
          )}
        </p>
      )}
      {text.isPending && <Loading />}
      {text.isError && <ErrorNote error={text.error} onRetry={() => void text.refetch()} />}
      {text.data?.kind === "current" && (
        <>
          <pre data-testid="piece-text" className="max-h-80 overflow-auto whitespace-pre-wrap break-words rounded border border-line bg-subtle p-2 font-mono text-[12px]">{text.data.text}</pre>
          {text.data.cut && <p data-testid="piece-cut" className="mt-1 text-warn">{t.sessions.call.cut(fmt.bytes(text.data.bytes))}</p>}
        </>
      )}
    </section>
  );
}

function Reply({ step, index, keyOf, trace, picked, go }: { step: Step; index: number; keyOf: TraceKey; trace: Trace; picked: Picked | null; go: Go }) {
  const { t } = useI18n();
  const open = picked?.side === "reply" ? picked.piece : null;
  const shown = open !== null ? step.reply[open] : undefined;
  return (
    <section aria-label={t.sessions.call.reply} className="pb-4">
      <h3 className="text-[12px] font-semibold">{t.sessions.call.reply}</h3>
      {step.reply.length === 0 && <p className="text-muted">{t.sessions.call.noReply}</p>}
      <ul data-testid="reply-pieces">
        {step.reply.map((piece, i) => (
          <li key={i}>
            <button
              type="button"
              data-testid="reply-piece"
              aria-pressed={open === i}
              onClick={() => go({ pc: pickText("reply", i) })}
              className={`grid w-full grid-cols-[minmax(0,7rem)_minmax(0,1fr)_4rem_4rem] items-baseline gap-x-2 border-l-2 px-1 py-0.5 text-left hover:bg-subtle ${open === i ? "border-accent bg-accent-bg" : "border-transparent"}`}
            >
              <span className="flex items-center gap-1 truncate">
                <span aria-hidden className="inline-block size-2 shrink-0" style={{ background: kindColor(piece.kind) }} />
                {t.sessions.kind[piece.kind]}
              </span>
              <span className="truncate font-mono text-[12px]">{piece.label ?? ""}</span>
              <span className="text-right tabular-nums" title={t.sessions.estimatedTokens}>{exact(piece.tokens)}</span>
              <span className="text-right text-[11px] text-muted">{fmt.bytes(piece.bytes)}</span>
            </button>
          </li>
        ))}
      </ul>
      {shown && open !== null && <PieceText piece={{ kind: shown.kind, label: shown.label }} pick={{ side: "reply", piece: open }} index={index} keyOf={keyOf} trace={trace} go={go} />}
    </section>
  );
}
