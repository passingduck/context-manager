import { useProject } from "../app/project";
import { Badge, Chip, Empty, Mono, PaneTitle } from "../components/ui";
import type { Hook } from "../gen/Hook";
import { useI18n } from "../i18n";
import { say } from "../i18n/types";
import type { Route } from "../lib/route";
import { HookStateBadge } from "./Detail";
import { AddHookButton, HookSwitch } from "../write/hooks";

type Go = (patch: Partial<Route>) => void;

export function HooksView({ route, go }: { route: Route; go: Go }) {
  const { t } = useI18n();
  const project = useProject();
  const withHooks = project.snapshot.agents.filter((view) => view.hooks.length > 0);
  return (
    <>
      <PaneTitle>{t.hooks.title}</PaneTitle>
      <div className="min-h-0 flex-1 overflow-y-auto pb-4">
        {withHooks.length === 0 && <Empty title={t.hooks.emptyTitle}>{t.hooks.emptyBody}</Empty>}
        {project.snapshot.agents.map((view) => {
          const events = [...new Set(view.hooks.map((hook) => hook.event))];
          return (
            <section key={view.agent} aria-label={project.name(view.agent)} className="border-b border-line pb-2">
              <h3 className="flex items-center gap-2 px-3 pb-1 pt-3 text-[14px] font-semibold">
                <span className="size-2.5 rounded-full" style={{ background: project.color(view.agent) }} aria-hidden />
                {project.name(view.agent)}
                <span className="text-[12px] font-normal text-muted tabular-nums">{view.hooks.length}</span>
                <span className="ml-auto"><AddHookButton agent={view.agent} /></span>
              </h3>
              {view.hooks.length === 0 && <p className="px-3 text-muted">{t.hooks.emptyBody}</p>}
              {events.map((event) => (
                <div key={event} role="group" aria-label={event}>
                  <h4 className="px-3 pb-0.5 pt-2 font-mono text-[11px] font-semibold text-faint">{event}</h4>
                  {view.hooks.filter((hook) => hook.event === event).map((hook, index, inEvent) => (
                    <HookRow key={hook.id} hook={hook} agent={view.agent} selected={route.sel === `hook:${hook.id}`} go={go} previous={inEvent[index - 1]} />
                  ))}
                </div>
              ))}
              {view.hookFiles.length > 0 && (
                <p className="px-3 pt-2 text-[11px] text-faint">
                  {t.hooks.hookFiles}:{" "}
                  {view.hookFiles.map((file) => (
                    <Mono key={file.file} className="mr-2 inline-block">{project.display(file.file)}</Mono>
                  ))}
                </p>
              )}
            </section>
          );
        })}
      </div>
    </>
  );
}

/** Rows that share this key are one run: the scope and the switch kind are written once, on the first row. */
const sourceKey = (hook: Hook) => `${JSON.stringify(hook.source.scope)}\n${hook.toggle.kind}`;

function HookRow({ hook, agent, selected, go, previous }: { hook: Hook; agent: string; selected: boolean; go: Go; previous: Hook | undefined }) {
  const { t, d } = useI18n();
  const project = useProject();
  const showScope = previous === undefined || sourceKey(previous) !== sourceKey(hook);
  const showFile = previous === undefined || previous.source.file !== hook.source.file;
  return (
    <div
      role="button"
      tabIndex={0}
      aria-pressed={selected}
      onClick={() => go({ sel: `hook:${hook.id}`, ag: agent })}
      onKeyDown={(event) => {
        if (event.key === "Enter" || event.key === " ") {
          event.preventDefault();
          go({ sel: `hook:${hook.id}`, ag: agent });
        }
      }}
      className="border-l-2 border-transparent px-3 py-1.5 hover:bg-subtle aria-pressed:border-accent aria-pressed:bg-accent-bg"
    >
      <div className="flex items-center gap-2">
        <Chip title={say(t.matcherEval, hook.matcher.eval, d)}>{hook.matcher.raw ?? t.hooks.anyTool}</Chip>
        <Mono className="min-w-0 flex-1 truncate">{hook.handler.summary}</Mono>
        <HookStateBadge hook={hook} />
        <HookSwitch hook={hook} />
      </div>
      {(showScope || showFile || hook.state.kind !== "active") && (
        <div className="mt-0.5 flex flex-wrap items-center gap-x-2 text-[11px] text-muted">
          {showFile && <Mono className="text-[11px]">{project.display(hook.source.file)}</Mono>}
          {showScope && (
            <>
              <Badge tone="neutral">{say(t.scope, hook.source.scope, d)}</Badge>
              <span>{hook.toggle.kind === "unavailable" ? `${t.toggle.unavailable()}: ${project.rule(hook.toggle.why)}` : t.toggle[hook.toggle.kind]()}</span>
            </>
          )}
          {hook.state.kind !== "active" && <span className="min-w-0 truncate">{say(t.hookState, hook.state, d)}</span>}
        </div>
      )}
    </div>
  );
}
