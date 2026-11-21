import { useQueryClient } from "@tanstack/react-query";
import { useCallback, useEffect, useMemo, useState } from "react";
import { useTrace, useTraces } from "../api/queries";
import { useProject } from "../app/project";
import { Badge, Button, Empty, ErrorNote, Loading, PaneTitle } from "../components/ui";
import type { Trace } from "../gen/Trace";
import type { TraceInfo } from "../gen/TraceInfo";
import type { TraceKey } from "../gen/TraceKey";
import type { TraceProblem } from "../gen/TraceProblem";
import { useI18n } from "../i18n";
import { say } from "../i18n/types";
import * as fmt from "../lib/format";
import { exact } from "../lib/prefix";
import { NO_SESSION, type Route } from "../lib/route";
import { allFitted, cacheLoss, totals } from "../lib/trace";
import { ChartLegend, SessionChart } from "./SessionChart";

type Go = (patch: Partial<Route>, replace?: boolean) => void;

/** The key of the trace the route names: the session, or one of its subagents. Null on the list. */
export function useTraceKey(route: Route): TraceKey | null {
  const { ss, ag, pt } = route;
  return useMemo(() => (ss ? { agent: ag, id: ss, part: pt || null } : null), [ss, ag, pt]);
}

export function SessionsView({ route, go }: { route: Route; go: Go }) {
  return route.ss ? <SessionBody route={route} go={go} /> : <SessionList go={go} />;
}

const TOP_SHOWN = 8;

const byStart = (a: TraceInfo, b: TraceInfo) => (b.started ?? "").localeCompare(a.started ?? "") || b.id.localeCompare(a.id);

function SessionList({ go }: { go: Go }) {
  const { t, lang } = useI18n();
  const project = useProject();
  const traces = useTraces(project.cwd);
  const rows = [...(traces.data ?? [])].sort(byStart);
  return (
    <>
      <PaneTitle>{t.sessions.title}</PaneTitle>
      <div className="min-h-0 flex-1 overflow-y-auto">
        <p className="border-b border-line bg-subtle px-3 py-1.5 text-[12px] text-muted">{t.sessions.record}</p>
        {traces.isPending && <Loading />}
        {traces.isError && <ErrorNote error={traces.error} onRetry={() => void traces.refetch()} />}
        {traces.isSuccess && rows.length === 0 && <Empty title={t.sessions.empty} />}
        <ul>
          {rows.map((info) => {
            const seconds = fmt.secondsBetween(info.started, info.last);
            return (
              <li key={`${info.agent}/${info.id}`}>
                <button
                  type="button"
                  data-testid="session-row"
                  data-session={info.id}
                  onClick={() => go({ ss: info.id, ag: info.agent, pt: "", call: 0, pc: "" })}
                  className="block w-full border-l-2 border-transparent px-3 py-2 text-left hover:bg-subtle"
                >
                  <b className="block truncate">{info.title ?? t.sessions.untitled}</b>
                  <span className="flex items-center gap-1.5 text-[11px] text-muted">
                    <span className="size-2 rounded-full" style={{ background: project.color(info.agent) }} aria-hidden />
                    <span data-testid="session-agent">{project.name(info.agent)}</span>
                  </span>
                  <span className="flex flex-wrap gap-x-4 text-[11px] text-muted">
                    <span>{t.sessions.startedHead}: {info.started ? fmt.when(info.started, lang) : t.sessions.unknown}</span>
                    <span>{t.sessions.ranHead}: {seconds === null ? t.sessions.unknown : t.sessions.duration(seconds)}</span>
                    <span>{t.sessions.sizeHead}: {fmt.bytes(info.bytes)}</span>
                  </span>
                </button>
              </li>
            );
          })}
        </ul>
      </div>
    </>
  );
}

function SessionBody({ route, go }: { route: Route; go: Go }) {
  const { t } = useI18n();
  const project = useProject();
  const key = useTraceKey(route)!;
  const query = useTrace(project.cwd, key);
  const trace = query.data;
  const client = useQueryClient();

  // A selected call that the trace no longer has is dropped.
  useEffect(() => {
    if (trace && route.call > trace.steps.length) go({ call: 0, pc: "" }, true);
  }, [trace, route.call, go]);

  const inPart = route.pt !== "";
  const listed = useTraces(project.cwd);
  const info = listed.data?.find((i) => i.agent === key.agent && i.id === key.id);
  // The record of a running session keeps growing, and the trace the view holds does not.
  const grew = !inPart && trace !== undefined && info !== undefined && info.bytes > trace.read;
  const refresh = () => void Promise.all([query.refetch(), listed.refetch()]);
  const back = () => (inPart ? go({ pt: "", call: 0, pc: "" }) : go(NO_SESSION));
  return (
    <>
      <PaneTitle right={<><Button data-testid="sessions-refresh" onClick={refresh}>{t.sessions.refresh.button}</Button><Button data-testid="sessions-back" onClick={back}>{inPart ? t.sessions.backToSession : t.sessions.backToList}</Button></>}>
        {inPart ? t.sessions.subagentTitle(trace?.parts.find((p) => p.part === route.pt)?.label ?? null) : (trace?.info.title ?? t.sessions.untitled)}
      </PaneTitle>
      <div className="min-h-0 flex-1 overflow-y-auto">
        {grew && (
          <p data-testid="session-grew" role="status" className="border-b border-line bg-info-bg px-3 py-1 text-info">
            {t.sessions.refresh.grew} {t.sessions.refresh.hint}
          </p>
        )}
        {query.isPending && <Loading />}
        {query.isError && <ErrorNote error={query.error} onRetry={() => void client.invalidateQueries({ queryKey: ["trace", project.cwd, key] })} />}
        {trace && <TraceBody trace={trace} route={route} go={go} />}
      </div>
    </>
  );
}

function TraceBody({ trace, route, go }: { trace: Trace; route: Route; go: Go }) {
  const { t } = useI18n();
  const sum = totals(trace);
  const [allTop, setAllTop] = useState(false);
  const selected = route.call > 0 && route.call <= trace.steps.length ? route.call - 1 : null;
  // A session opens on its last call, so the right pane is never blank.
  useEffect(() => {
    if (route.call === 0 && trace.steps.length > 0) go({ call: trace.steps.length }, true);
  }, [route.call, trace.steps.length, go]);

  const select = useCallback((index: number) => go({ call: index + 1, pc: "" }), [go]);
  if (trace.steps.length === 0) {
    return (
      <>
        <Problems problems={trace.problems} />
        <Empty title={t.sessions.noCalls} />
      </>
    );
  }
  const fitted = allFitted(trace);
  const lostAt = trace.steps.flatMap((_, i) => (cacheLoss(trace, i) ? [i] : []));
  const lostCount = lostAt.length;
  const nextLost = () => select(lostAt.find((i) => selected === null || i > selected) ?? lostAt[0]!);
  return (
    <div className="space-y-3 px-3 py-2">
      <dl data-testid="stats" className="grid grid-cols-4 gap-2">
        <Stat id="calls" label={t.sessions.stats.calls} value={exact(sum.calls)} />
        <Stat id="largest" label={t.sessions.stats.largest} value={exact(sum.largest)} />
        <Stat id="output" label={t.sessions.stats.output} value={exact(sum.output)} />
        <Stat id="share" label={t.sessions.stats.cacheShare} value={sum.cacheShare === null ? t.sessions.unknown : `${Math.round(sum.cacheShare * 100)}%`} />
      </dl>
      <p className="text-[11px] text-muted">{t.sessions.stats.note}</p>

      <section aria-label={t.sessions.chart.label}>
        <div className="mb-1 flex items-center justify-between gap-2">
          <p className="text-[12px] font-semibold">
            <Badge tone={route.cm === "cache" ? "info" : "warn"}>{route.cm === "cache" ? t.sessions.tagReal : fitted ? t.sessions.tagFitted : t.sessions.tagEstimate}</Badge>{" "}
            {route.cm === "cache" ? t.sessions.chart.axisReal : fitted ? t.sessions.chart.axisFitted : t.sessions.chart.axisEstimate}
            {lostCount > 0 && (
              <span className="ml-2">
                <button type="button" data-testid="lost-next" title={t.sessions.lost.next} onClick={nextLost} className="cursor-pointer rounded hover:underline">
                  <Badge tone="bad">{t.sessions.lost.count(lostCount)}</Badge>
                </button>
              </span>
            )}
          </p>
          <div role="group" aria-label={t.sessions.chart.modeLabel} className="flex shrink-0 overflow-hidden rounded border border-line text-[12px]">
            {(["cache", "kind"] as const).map((mode) => (
              <button key={mode} type="button" data-testid={`chart-mode-${mode}`} aria-pressed={route.cm === mode} onClick={() => go({ cm: mode })} className="px-2 py-0.5 hover:bg-sunken aria-pressed:bg-accent-bg aria-pressed:font-semibold aria-pressed:text-accent">
                {mode === "cache" ? t.sessions.chart.modeCache : t.sessions.chart.modeKind}
              </button>
            ))}
          </div>
        </div>
        <SessionChart trace={trace} mode={route.cm} selected={selected} onSelect={select} />
        <div className="mt-1.5">
          <ChartLegend mode={route.cm} fitted={fitted} />
        </div>
      </section>

      <Problems problems={trace.problems} />

      {trace.top.length > 0 && (
        <section aria-label={t.sessions.top.title}>
          <h3 className="text-[12px] font-semibold">{t.sessions.top.title}</h3>
          <p className="mb-1 text-[11px] text-muted">{t.sessions.top.note}</p>
          <ul data-testid="top-pieces">
            {(allTop ? trace.top : trace.top.slice(0, TOP_SHOWN)).map((piece) => (
              <li key={`${piece.step}-${piece.piece}`}>
                <button
                  type="button"
                  data-testid="top-piece"
                  data-step={piece.step}
                  data-piece={piece.piece}
                  onClick={() => go({ call: piece.step + 1, pc: `s${piece.piece}` })}
                  className="grid w-full grid-cols-[minmax(0,8rem)_minmax(0,1fr)_4.5rem_5.5rem] items-center gap-x-2 px-1 py-0.5 text-left hover:bg-subtle"
                >
                  <span>{t.sessions.kind[piece.kind]}</span>
                  <span className="truncate font-mono text-[12px] text-muted">{piece.label ?? ""}</span>
                  <span className="text-right tabular-nums" title={t.sessions.estimatedTokens}>{exact(piece.tokens)}</span>
                  <span className="text-right text-[11px] text-muted">{t.sessions.tip.call(piece.step + 1)}</span>
                </button>
              </li>
            ))}
          </ul>
          {trace.top.length > TOP_SHOWN && (
            <Button className="mt-1" aria-expanded={allTop} onClick={() => setAllTop(!allTop)}>
              {allTop ? t.sessions.top.less : t.sessions.top.more(trace.top.length)}
            </Button>
          )}
        </section>
      )}

      {trace.parts.length > 0 && route.pt === "" && (
        <section aria-label={t.sessions.parts.title}>
          <h3 className="text-[12px] font-semibold">{t.sessions.parts.title}</h3>
          <p className="mb-1 text-[11px] text-muted">{t.sessions.parts.note}</p>
          <ul data-testid="parts" className="max-h-56 overflow-y-auto">
            {trace.parts.map((part) => (
              <li key={part.part}>
                <button type="button" data-testid="part-row" data-part={part.part} onClick={() => go({ pt: part.part, call: 0, pc: "" })} className="flex w-full items-baseline gap-2 px-1 py-0.5 text-left hover:bg-subtle">
                  <b className="shrink-0">{part.label ?? t.sessions.parts.unnamed}</b>
                  <span className="min-w-0 flex-1 truncate text-muted">{part.description ?? ""}</span>
                  <span className="shrink-0 text-[11px] text-muted">{fmt.bytes(part.bytes)}</span>
                </button>
              </li>
            ))}
          </ul>
        </section>
      )}
    </div>
  );
}

function Stat({ id, label, value }: { id: string; label: string; value: string }) {
  return (
    <div data-testid={`stat-${id}`} className="rounded border border-line px-2 py-1">
      <dt className="text-[11px] text-muted">{label}</dt>
      <dd className="text-[15px] font-semibold tabular-nums">{value}</dd>
    </div>
  );
}

function Problems({ problems }: { problems: TraceProblem[] }) {
  const { t, d } = useI18n();
  if (problems.length === 0) return null;
  return (
    <ul data-testid="trace-problems" className="space-y-1">
      {problems.map((problem, i) => (
        <li key={i} data-problem={problem.kind} className="rounded border border-warn bg-warn-bg px-2 py-1 text-warn">
          {say(t.sessions.problem, problem, d)}
        </li>
      ))}
    </ul>
  );
}
