import { Badge } from "../components/ui";
import type { Snapshot } from "../gen/Snapshot";
import { useI18n } from "../i18n";
import * as fmt from "../lib/format";
import { VIEWS, type Route, type View } from "../lib/route";

const ICONS: Record<View, string> = {
  context: "M3 4h10M3 8h10M3 12h6",
  memory: "M8 2a6 6 0 1 0 0 12A6 6 0 0 0 8 2Zm0 3v3l2 1.5",
  hooks: "M9 1 3 9h4l-1 6 6-8H8l1-6Z",
  doctor: "M8 3v10M3 8h10",
  history: "M3 8a5 5 0 1 0 1.5-3.5M3 2v3h3M8 5v3l2 1",
};

export function Rail({ route, go, snapshot, readOnly, pending }: { route: Route; go: (patch: Partial<Route>) => void; snapshot: Snapshot; readOnly: boolean; pending: number }) {
  const { t } = useI18n();
  const always = snapshot.agents.reduce((n, view) => n + view.budget.sessionStart, 0);
  const counts: Record<View, number | null> = {
    context: null,
    memory: snapshot.agents.reduce((n, v) => n + v.memory.length, 0),
    hooks: snapshot.agents.reduce((n, v) => n + v.hooks.length, 0),
    doctor: snapshot.diagnostics.length,
    history: pending > 0 ? pending : null,
  };
  return (
    <nav aria-label={t.rail.sections} className="flex min-h-0 flex-col border-r border-line bg-subtle px-2 py-3">
      <ul className="space-y-0.5">
        {VIEWS.map((view) => {
          const current = route.view === view;
          const count = counts[view];
          return (
            <li key={view}>
              <button
                type="button"
                aria-current={current ? "page" : undefined}
                onClick={() => go({ view, sel: "", ag: "", ln: 0 })}
                className="flex w-full items-center gap-2 rounded px-2 py-1.5 text-left hover:bg-sunken aria-[current=page]:bg-accent-bg aria-[current=page]:font-semibold aria-[current=page]:text-accent"
              >
                <svg viewBox="0 0 16 16" className="size-4 shrink-0" fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" strokeLinejoin="round" aria-hidden>
                  <path d={ICONS[view]} />
                </svg>
                <span className="flex-1">{t.rail[view]}</span>
                {count !== null && (view === "doctor" ? <Badge tone={count ? "bad" : "ok"}>{count}</Badge> : view === "history" ? <Badge tone="info" title={t.rail.pending(count)}>{count}</Badge> : <span className="text-[11px] text-muted tabular-nums">{count}</span>)}
              </button>
            </li>
          );
        })}
      </ul>
      <div className="mt-auto space-y-1 px-2 text-[11px] text-muted">
        <p>{t.rail.totalAlways(fmt.tokens(always))}</p>
        {readOnly && <p>{t.rail.readOnlyHost}</p>}
      </div>
    </nav>
  );
}
