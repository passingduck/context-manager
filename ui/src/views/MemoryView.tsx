import { useProject } from "../app/project";
import { Badge, Button, Chip, Empty, Mono, PaneTitle } from "../components/ui";
import type { MemoryStore } from "../gen/MemoryStore";
import { useI18n } from "../i18n";
import { say } from "../i18n/types";
import type { Route } from "../lib/route";
import { AddMemory } from "../write/create";
import { useWrite } from "../write/flow";

type Go = (patch: Partial<Route>) => void;

export function MemoryView({ route, go }: { route: Route; go: Go }) {
  const { t } = useI18n();
  const project = useProject();
  const withStores = project.snapshot.agents.filter((view) => view.memory.length > 0);
  return (
    <>
      <PaneTitle>{t.memory.title}</PaneTitle>
      <div className="min-h-0 flex-1 overflow-y-auto pb-4">
        {withStores.length === 0 && <Empty title={t.memory.emptyTitle}>{t.memory.emptyBody}</Empty>}
        {withStores.map((view) => (
          <section key={view.agent} aria-label={project.name(view.agent)}>
            <h3 className="flex items-center gap-2 px-3 pb-1 pt-3 text-[14px] font-semibold">
              <span className="size-2.5 rounded-full" style={{ background: project.color(view.agent) }} aria-hidden />
              {project.name(view.agent)}
            </h3>
            {view.memory.map((store) => (
              <Store key={store.id} store={store} agent={view.agent} route={route} go={go} />
            ))}
          </section>
        ))}
      </div>
    </>
  );
}

function Store({ store, agent, route, go }: { store: MemoryStore; agent: string; route: Route; go: Go }) {
  const { t, d } = useI18n();
  const project = useProject();
  const write = useWrite();
  return (
    <article className="mx-3 mt-2 rounded border border-line">
      <header className="space-y-1 border-b border-line bg-subtle px-3 py-2">
        <div className="flex flex-wrap items-center gap-2">
          <b className="font-mono text-[12px]">{store.id}</b>
          <Badge tone={store.enabled.kind === "on" ? "ok" : "warn"}>{store.enabled.kind === "on" ? t.memory.enabled : t.memory.off}</Badge>
          <span className="ml-auto"><AddMemory agent={agent} store={store} /></span>
        </div>
        <p className="text-muted">{project.rule(store.label)}</p>
        {store.enabled.kind === "off" && <p>{t.memory.disabled(store.enabled.by)}</p>}
        <p className="text-muted">
          {t.memory.root}: <Mono className="break-all">{d(store.root)}</Mono>
        </p>
      </header>
      <div className="px-3 py-1.5">
        <p className="text-[11px] font-semibold uppercase tracking-wide text-faint">{t.memory.index}</p>
        {store.index ? (
          <button type="button" onClick={() => go({ sel: `file:${store.index}`, ag: agent, ln: 0 })} className="text-left text-accent underline">
            <Mono className="break-all">{project.display(store.index)}</Mono>
          </button>
        ) : (
          <p className="text-muted">{t.memory.noIndex}</p>
        )}
      </div>
      <div className="border-t border-line">
        <p className="px-3 pb-0.5 pt-1.5 text-[11px] font-semibold uppercase tracking-wide text-faint">{t.memory.entries}</p>
        {store.entries.length === 0 && <p className="px-3 pb-2 text-muted">{t.memory.noEntries}</p>}
        <ul>
          {store.entries.map((entry) => {
            const selected = route.sel === `file:${entry.file}`;
            const state = project.snapshot.files[entry.file];
            const removable = (entry.role === "topic" || entry.role === "note") && state?.policy.kind === "editable" && state.state.kind === "present";
            return (
              <li key={entry.file} className="flex items-center hover:bg-subtle">
                <button
                  type="button"
                  aria-pressed={selected}
                  onClick={() => go({ sel: `file:${entry.file}`, ag: agent, ln: 0 })}
                  className="flex min-w-0 flex-1 items-center gap-2 border-l-2 border-transparent px-3 py-1 text-left aria-pressed:border-accent aria-pressed:bg-accent-bg"
                >
                  <Chip>{t.memoryRole[entry.role]}</Chip>
                  <span className="min-w-0 flex-1">
                    <Mono className="block truncate" >{entry.file.startsWith(`${store.root}/`) ? entry.file.slice(store.root.length + 1) : project.display(entry.file)}</Mono>
                    {entry.description && <span className="block truncate text-[11px] text-muted">{entry.description}</span>}
                  </span>
                  {entry.indexed === true && <Badge tone="ok">{t.memory.indexed}</Badge>}
                  {entry.indexed === false && <Badge tone="warn">{t.memory.notIndexed}</Badge>}
                </button>
                {removable && state?.state.kind === "present" && (
                  <Button
                    className="mr-3 shrink-0"
                    disabled={write.blocked !== null}
                    title={write.blocked ?? undefined}
                    onClick={() => {
                      if (state.state.kind !== "present") return;
                      write.propose({ edit: { kind: "trash", path: entry.file, base: state.state.rev }, title: t.write.deleteMemoryTitle(entry.title) });
                    }}
                  >
                    {t.write.delete}
                  </Button>
                )}
              </li>
            );
          })}
        </ul>
      </div>
      <footer className="border-t border-line bg-subtle px-3 py-2">
        <p className="text-[11px] font-semibold uppercase tracking-wide text-faint">{t.memory.writeTarget}</p>
        <p>{say(t.memoryWrite, store.write, d)}</p>
        {store.write.kind === "readOnly" && <p className="text-muted">{project.rule(store.write.why)}</p>}
      </footer>
    </article>
  );
}
