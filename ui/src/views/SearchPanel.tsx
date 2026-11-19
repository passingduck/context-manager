import { useState } from "react";
import { useSearch } from "../api/queries";
import { useProject } from "../app/project";
import { ErrorNote, Loading, Mono, PaneTitle, Badge, Empty } from "../components/ui";
import type { SearchHit } from "../gen/SearchHit";
import { useI18n } from "../i18n";
import type { Route } from "../lib/route";

type Go = (patch: Partial<Route>) => void;

export function SearchPanel({ route, go }: { route: Route; go: Go }) {
  const { t } = useI18n();
  const project = useProject();
  const [regex, setRegex] = useState(false);
  const [caseSensitive, setCase] = useState(false);
  const search = useSearch(project.cwd, route.q, regex, caseSensitive);
  const hits = search.data?.hits ?? [];
  const byFile = new Map<string, SearchHit[]>();
  for (const hit of hits) byFile.set(hit.file, [...(byFile.get(hit.file) ?? []), hit]);

  return (
    <>
      <PaneTitle
        right={
          <>
            <label className="flex items-center gap-1 text-[12px]">
              <input type="checkbox" checked={caseSensitive} onChange={(e) => setCase(e.target.checked)} /> {t.search.caseSensitive}
            </label>
            <label className="flex items-center gap-1 text-[12px]">
              <input type="checkbox" checked={regex} onChange={(e) => setRegex(e.target.checked)} /> {t.search.regex}
            </label>
          </>
        }
      >
        {t.search.title}
        {search.data && <span className="ml-2 font-normal text-muted">{t.search.hits(hits.length, search.data.capped)}</span>}
      </PaneTitle>
      <div className="min-h-0 flex-1 overflow-y-auto pb-4">
        {search.isPending && search.fetchStatus !== "idle" && <Loading />}
        {search.isError && <ErrorNote error={search.error} onRetry={() => void search.refetch()} />}
        {search.isSuccess && hits.length === 0 && <Empty title={t.search.none} />}
        {[...byFile].map(([file, fileHits]) => (
          <section key={file} aria-label={project.display(file)}>
            <h3 className="flex items-center gap-2 bg-subtle px-3 py-1">
              <Mono className="font-semibold">{project.display(file)}</Mono>
              <Badge tone="neutral">{fileHits.length}</Badge>
            </h3>
            <ul>
              {fileHits.map((hit) => (
                <li key={`${hit.line}:${hit.column}`}>
                  <button
                    type="button"
                    onClick={() => go({ sel: `file:${file}`, ag: "", ln: hit.line })}
                    aria-pressed={route.sel === `file:${file}` && route.ln === hit.line}
                    className="flex w-full gap-3 border-l-2 border-transparent px-3 py-0.5 text-left hover:bg-subtle aria-pressed:border-accent aria-pressed:bg-accent-bg"
                  >
                    <span className="w-16 shrink-0 text-right text-faint tabular-nums" title={t.search.line(hit.line)}>{hit.line}</span>
                    <Preview hit={hit} query={route.q} regex={regex} />
                  </button>
                </li>
              ))}
            </ul>
          </section>
        ))}
      </div>
    </>
  );
}

function Preview({ hit, query, regex }: { hit: SearchHit; query: string; regex: boolean }) {
  const length = regex ? 0 : query.length;
  // Keep the match in view: drop the start of a long line.
  const from = Math.max(0, hit.column - 1 - 24);
  const start = hit.column - 1 - from;
  const text = hit.text.slice(from);
  return (
    <Mono className="min-w-0 flex-1 truncate">
      {from > 0 ? "…" : ""}
      {text.slice(0, start)}
      <mark className="rounded-sm bg-warn-bg text-warn">{text.slice(start, start + length)}</mark>
      {text.slice(start + length)}
    </Mono>
  );
}
