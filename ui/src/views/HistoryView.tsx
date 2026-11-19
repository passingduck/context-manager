import { useEffect } from "react";
import { useHistory } from "../api/queries";
import { useProject } from "../app/project";
import { Badge, Chip, Empty, ErrorNote, Loading, Mono, PaneTitle } from "../components/ui";
import type { ChangeRecord } from "../gen/ChangeRecord";
import { useI18n } from "../i18n";
import * as fmt from "../lib/format";
import type { Route } from "../lib/route";
import { Attention } from "./Detail";

type Go = (patch: Partial<Route>, replace?: boolean) => void;

export function HistoryView({ route, go }: { route: Route; go: Go }) {
  const { t } = useI18n();
  const project = useProject();
  const history = useHistory(project.cwd);
  const rows = [...(history.data ?? [])].sort((a, b) => Number(b.state !== "sealed") - Number(a.state !== "sealed") || b.id.localeCompare(a.id));
  const firstId = rows[0]?.id;
  // With nothing selected, show the newest pending change, or the newest change.
  useEffect(() => {
    if (!route.sel && firstId) go({ sel: `change:${firstId}` }, true);
  }, [route.sel, firstId, go]);
  return (
    <>
      <PaneTitle>{t.history.title}</PaneTitle>
      <div className="min-h-0 flex-1 overflow-y-auto">
        {history.isPending && <Loading />}
        {history.isError && <ErrorNote error={history.error} onRetry={() => void history.refetch()} />}
        {history.isSuccess && rows.length === 0 && <Empty title={t.history.empty} />}
        <ul>
          {rows.map((record) => (
            <Row key={record.id} record={record} selected={route.sel === `change:${record.id}`} go={go} />
          ))}
        </ul>
      </div>
    </>
  );
}

function Row({ record, selected, go }: { record: ChangeRecord; selected: boolean; go: Go }) {
  const { t, lang } = useI18n();
  const project = useProject();
  const first = record.summary.targets[0];
  const more = record.summary.targets.length - 1;
  return (
    <li>
      <button
        type="button"
        aria-pressed={selected}
        onClick={() => go({ sel: `change:${record.id}`, ag: record.summary.agent ?? "" })}
        className="flex w-full items-start gap-2 border-l-2 border-transparent px-3 py-2 text-left hover:bg-subtle aria-pressed:border-accent aria-pressed:bg-accent-bg"
      >
        <Badge tone={record.state === "sealed" ? "ok" : record.state === "pending" ? "info" : "warn"}>{t.changeState[record.state]}</Badge>
        <span className="min-w-0 flex-1">
          <span className="flex items-center gap-2">
            <b>{t.changeEdit[record.summary.edit.kind]}</b>
            {record.summary.agent && <Chip>{project.name(record.summary.agent)}</Chip>}
          </span>
          {first && (
            <Mono className="block truncate text-muted">
              {project.display(first)}
              {more > 0 ? ` +${more}` : ""}
            </Mono>
          )}
        </span>
        <span className="shrink-0 text-[11px] text-muted">{fmt.when(record.at, lang)}</span>
      </button>
      {(record.state === "pending" || record.state === "needsAttention") && <Attention id={record.id} state={record.state} />}
    </li>
  );
}
