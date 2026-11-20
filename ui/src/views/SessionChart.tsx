import { memo, useEffect, useMemo, useRef, useState, type CSSProperties, type KeyboardEvent } from "react";
import type { PieceKind } from "../gen/PieceKind";
import type { Step } from "../gen/Step";
import type { Trace } from "../gen/Trace";
import { useI18n } from "../i18n";
import * as fmt from "../lib/format";
import { exact } from "../lib/prefix";
import { axisTicks, cacheLoss, estimateOf, growth, KINDS, kindColor, promptOf } from "../lib/trace";
import { LossNote } from "./LossNote";

export type ChartMode = "cache" | "kind";

const BAR_H = 128;
const LANE_H = 10;
const GAP = 1;

/** Patterns, so that the three parts of a prompt differ by more than colour. */
export const CACHE_FILL = {
  read: "var(--c1)",
  write: "repeating-linear-gradient(135deg, var(--c4) 0 3px, color-mix(in srgb, var(--c4) 55%, var(--bg)) 3px 6px)",
  input: "repeating-linear-gradient(90deg, var(--c0) 0 2px, color-mix(in srgb, var(--c0) 45%, var(--bg)) 2px 4px)",
} as const;

const barWidth = (calls: number) => (calls <= 40 ? 16 : calls <= 150 ? 8 : 4);

interface Tip {
  index: number;
  x: number;
  y: number;
}

/** One bar for each call, in the order the calls were made. */
export function SessionChart({ trace, mode, selected, onSelect }: { trace: Trace; mode: ChartMode; selected: number | null; onSelect: (index: number) => void }) {
  const { t } = useI18n();
  const [tip, setTip] = useState<Tip | null>(null);
  const scroller = useRef<HTMLDivElement>(null);
  const steps = trace.steps;
  const width = barWidth(steps.length);
  const max = useMemo(() => steps.reduce((m, s) => Math.max(m, mode === "cache" ? (s.usage ? promptOf(s.usage) : 0) : estimateOf(s)), 1), [steps, mode]);
  const losses = useMemo(() => steps.map((_, i) => cacheLoss(trace, i)), [trace, steps]);

  useEffect(() => {
    scroller.current?.querySelector("[data-selected=true]")?.scrollIntoView({ inline: "nearest", block: "nearest" });
  }, [selected]);

  function onKeyDown(event: KeyboardEvent<HTMLDivElement>) {
    const to = event.key === "ArrowRight" ? 1 : event.key === "ArrowLeft" ? -1 : 0;
    if (to === 0 || steps.length === 0) return;
    event.preventDefault();
    onSelect(Math.max(0, Math.min(steps.length - 1, (selected ?? (to > 0 ? -1 : steps.length)) + to)));
  }

  const marks = new Map(trace.boundaries.map((b) => [b.before, b] as const));
  const cells = steps.flatMap((step, i) => {
    const out = [];
    const boundary = marks.get(i);
    if (boundary) {
      const title = `${t.sessions.chart.compaction}${boundary.preTokens !== null && boundary.postTokens !== null ? `: ${t.sessions.chart.compactionSizes(exact(boundary.preTokens), exact(boundary.postTokens))}` : ""}`;
      out.push(<div key={`c${i}`} data-testid="compaction" data-before={i} title={title} className="shrink-0 border-l-2 border-dashed border-warn" style={{ width: 4, height: BAR_H + LANE_H }} />);
    }
    out.push(<Bar key={i} step={step} index={i} lost={losses[i] !== null} selected={selected === i} mode={mode} max={max} width={width} onSelect={onSelect} onTip={setTip} />);
    return out;
  });

  const ticks = axisTicks(max);
  const bottom = (v: number) => LANE_H + Math.round((v / max) * BAR_H);
  const tipStep = tip ? steps[tip.index] : null;
  return (
    <div data-testid="chart" data-mode={mode} className="relative pt-3">
      <div className="flex">
        <div aria-hidden className="relative shrink-0 text-[10px] tabular-nums text-faint" style={{ width: 40, height: BAR_H + LANE_H }}>
          {ticks.map((v) => (
            <span key={v} className="absolute right-1 -translate-y-1/2" style={{ bottom: bottom(v) }}>
              {fmt.tokens(v)}
            </span>
          ))}
          {mode === "cache" && <span className="absolute right-1 text-[10px]" style={{ top: BAR_H + LANE_H + 2 }}>{fmt.tokens(costOf(trace).max)}</span>}
        </div>
        <div
          ref={scroller}
          role="group"
          tabIndex={0}
          aria-label={t.sessions.chart.label}
          onKeyDown={onKeyDown}
          className="min-w-0 flex-1 overflow-x-auto overflow-y-hidden border-b border-line"
        >
          <div className="relative flex items-end" style={{ width: "max-content", minWidth: "100%", height: BAR_H + LANE_H }}>
            {ticks.map((v) => (
              <div key={v} aria-hidden className="absolute inset-x-0 h-px bg-line" style={{ bottom: bottom(v) }} />
            ))}
            {cells}
          </div>
          {mode === "cache" && <CostRow trace={trace} width={width} />}
        </div>
      </div>
      {mode === "cache" && <p className="mt-0.5 pl-10 text-[11px] text-muted">{t.sessions.chart.costLane}</p>}
      {tip && tipStep && <Tooltip tip={tip} step={tipStep} index={tip.index} trace={trace} mode={mode} />}
    </div>
  );
}

interface BarProps {
  step: Step;
  index: number;
  lost: boolean;
  selected: boolean;
  mode: ChartMode;
  max: number;
  width: number;
  onSelect: (index: number) => void;
  onTip: (tip: Tip | null) => void;
}

const Bar = memo(function Bar({ step, index, lost, selected, mode, max, width, onSelect, onTip }: BarProps) {
  const { t } = useI18n();
  const px = (v: number) => (v <= 0 ? 0 : Math.max(1, Math.round((v / max) * BAR_H)));
  return (
    <button
      type="button"
      tabIndex={-1}
      data-testid="call-bar"
      data-index={index}
      data-lost={lost ? "true" : "false"}
      data-selected={selected}
      aria-label={`${t.sessions.tip.call(index + 1)}${lost ? `, ${t.sessions.lost.badge}` : ""}`}
      aria-pressed={selected}
      onClick={() => onSelect(index)}
      onPointerEnter={(e) => {
        const r = e.currentTarget.getBoundingClientRect();
        onTip({ index, x: r.left + r.width / 2, y: r.bottom });
      }}
      onPointerLeave={() => onTip(null)}
      className={`flex shrink-0 cursor-default flex-col justify-end p-0 ${selected ? "bg-accent-bg" : "hover:bg-subtle"}`}
      style={{ width: width + GAP, height: BAR_H + LANE_H, paddingRight: GAP }}
    >
      <Column step={step} mode={mode} px={px} />
      <span className="block w-full" style={{ height: LANE_H }}>
        {lost && <span aria-hidden data-testid="lost-mark" className="mx-auto block bg-bad" style={{ width: Math.max(3, width), height: LANE_H - 2, marginTop: 2, clipPath: "polygon(50% 0, 100% 100%, 0 100%)" }} />}
      </span>
    </button>
  );
});

function Column({ step, mode, px }: { step: Step; mode: ChartMode; px: (v: number) => number }) {
  if (mode === "cache") {
    if (!step.usage) return <span aria-hidden data-testid="no-usage" className="mb-0 block w-full border-2 border-dashed border-faint" style={{ height: 8 }} />;
    const u = step.usage;
    return (
      <span aria-hidden className="flex w-full flex-col justify-end" style={{ height: "auto" }}>
        <Seg h={px(u.input)} fill={CACHE_FILL.input} />
        <Seg h={px(u.cacheWrite)} fill={CACHE_FILL.write} />
        <Seg h={px(u.cacheRead)} fill={CACHE_FILL.read} />
      </span>
    );
  }
  if (!step.sent) return <span aria-hidden data-testid="no-request" className="block w-full border-2 border-dashed border-faint" style={{ height: 8 }} />;
  const byKind = new Map<PieceKind, number>(step.sent.byKind.map((k) => [k.kind, k.shared + k.fresh] as const));
  return (
    <span aria-hidden className="flex w-full flex-col justify-end">
      {[...KINDS].reverse().map((kind) => {
        const h = px(byKind.get(kind) ?? 0);
        return h > 0 ? <Seg key={kind} h={h} fill={kindColor(kind)} /> : null;
      })}
    </span>
  );
}

function Seg({ h, fill }: { h: number; fill: string }) {
  if (h <= 0) return null;
  const style: CSSProperties = { height: h, background: fill };
  return <span className="block w-full" style={style} />;
}

function Tooltip({ tip, step, index, trace, mode }: { tip: Tip; step: Step; index: number; trace: Trace; mode: ChartMode }) {
  const { t, lang } = useI18n();
  const s = t.sessions.tip;
  const loss = cacheLoss(trace, index);
  const grew = growth(trace, index);
  const u = step.usage;
  return (
    <div
      role="tooltip"
      data-testid="chart-tooltip"
      className="pointer-events-none fixed z-20 w-[19rem] rounded border border-line bg-bg p-2 text-[12px] shadow-lg"
      style={{ top: Math.min(tip.y + 6, window.innerHeight - 220), left: Math.max(8, Math.min(tip.x - 152, window.innerWidth - 312)) }}
    >
      <p className="mb-1 font-semibold">
        {s.call(index + 1)}, {fmt.clock(step.at, lang)}
      </p>
      <dl className="grid grid-cols-[7.5rem_1fr] gap-x-2 gap-y-0.5">
        <dt className="text-muted">{s.model}</dt>
        <dd className="break-all">{step.model ?? s.none}</dd>
        {u ? (
          <>
            <dt className="text-muted">{s.read}</dt>
            <dd className="tabular-nums">{exact(u.cacheRead)}</dd>
            <dt className="text-muted">{s.written}</dt>
            <dd className="tabular-nums">{exact(u.cacheWrite)}</dd>
            <dt className="text-muted">{s.uncached}</dt>
            <dd className="tabular-nums">{exact(u.input)}</dd>
            <dt className="text-muted">{s.output}</dt>
            <dd className="tabular-nums">{exact(u.output)}</dd>
            <dt className="text-muted">{s.grew}</dt>
            <dd className="tabular-nums">{grew === null ? s.none : `${grew > 0 ? "+" : ""}${exact(grew)}`}</dd>
          </>
        ) : (
          <>
            <dt className="text-muted">{s.usage}</dt>
            <dd>{s.noUsage}</dd>
          </>
        )}
        {mode === "kind" && (
          <>
            <dt className="text-muted">{s.estimate}</dt>
            <dd className="tabular-nums">{step.sent ? exact(estimateOf(step)) : s.noRequest}</dd>
          </>
        )}
      </dl>
      {loss && (
        <div className="mt-1.5 border-t border-line pt-1.5 text-bad">
          <p className="font-semibold">{t.sessions.lost.badge}</p>
          <LossNote loss={loss} />
        </div>
      )}
    </div>
  );
}

export function ChartLegend({ mode }: { mode: ChartMode }) {
  const { t } = useI18n();
  const c = t.sessions.chart;
  const swatch = (fill: string) => <span aria-hidden className="inline-block h-2.5 w-5" style={{ background: fill }} />;
  return (
    <ul data-testid="chart-legend" className="flex flex-wrap gap-x-4 gap-y-0.5 text-[11px] text-muted">
      {mode === "cache" ? (
        <>
          <li className="flex items-center gap-1">{swatch(CACHE_FILL.read)}{c.legendRead}</li>
          <li className="flex items-center gap-1">{swatch(CACHE_FILL.write)}{c.legendWrite}</li>
          <li className="flex items-center gap-1">{swatch(CACHE_FILL.input)}{c.legendUncached}</li>
          <li className="flex items-center gap-1">
            <span aria-hidden className="inline-block h-2 w-5 border-2 border-dashed border-faint" />
            {c.legendNoUsage}
          </li>
        </>
      ) : (
        <>
          <li className="w-full font-semibold text-warn">{c.estimateNote}</li>
          {KINDS.map((kind) => (
            <li key={kind} className="flex items-center gap-1">
              {swatch(kindColor(kind))}
              {t.sessions.kind[kind]}
            </li>
          ))}
          <li className="flex items-center gap-1">
            <span aria-hidden className="inline-block h-2 w-5 border-2 border-dashed border-faint" />
            {c.legendNoRequest}
          </li>
        </>
      )}
      <li className="flex items-center gap-1">
        <span aria-hidden className="inline-block h-3 border-l-2 border-dashed border-warn" />
        {c.legendCompaction}
      </li>
      <li className="flex items-center gap-1">
        <span aria-hidden className="inline-block h-2.5 w-3 bg-bad" style={{ clipPath: "polygon(50% 0, 100% 100%, 0 100%)" }} />
        {c.legendLost}
      </li>
    </ul>
  );
}

const COST_H = 36;

/** What each call paid in full, that is, everything it did not read from the cache, on a scale of its own. */
function costOf(trace: Trace): { paid: number[]; max: number } {
  const paid = trace.steps.map((s) => (s.usage ? s.usage.input + s.usage.cacheWrite : 0));
  return { paid, max: Math.max(1, ...paid) };
}

function CostRow({ trace, width }: { trace: Trace; width: number }) {
  const { paid, max } = useMemo(() => costOf(trace), [trace]);
  const marks = new Set(trace.boundaries.map((b) => b.before));
  return (
    <div aria-hidden data-testid="cost-lane" className="flex items-end border-t border-line" style={{ height: COST_H + 1 }}>
      {paid.map((v, i) => (
        <div key={i} className="flex shrink-0 items-end" style={{ width: width + GAP, height: COST_H, marginLeft: marks.has(i) ? 4 : 0, paddingRight: GAP }}>
          <span className="block w-full" style={{ height: v > 0 ? Math.max(1, Math.round((v / max) * COST_H)) : 0, background: CACHE_FILL.write }} />
        </div>
      ))}
    </div>
  );
}
