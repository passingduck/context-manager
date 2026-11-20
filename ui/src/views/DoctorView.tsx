import { useProject } from "../app/project";
import { Badge, Button, Empty, Mono, PaneTitle } from "../components/ui";
import type { Diagnostic } from "../gen/Diagnostic";
import type { Severity } from "../gen/Severity";
import { useI18n } from "../i18n";
import { say } from "../i18n/types";
import type { Route } from "../lib/route";
import { useWrite } from "../write/flow";

type Go = (patch: Partial<Route>) => void;
const SEVERITIES: Severity[] = ["error", "warning", "info"];
const TONE = { error: "bad", warning: "warn", info: "info" } as const;
const MARK = { error: "!", warning: "!", info: "i" } as const;

export function DoctorView({ route, go }: { route: Route; go: Go }) {
  const { t } = useI18n();
  const project = useProject();
  const all = project.snapshot.diagnostics;
  return (
    <>
      <PaneTitle right={<Badge tone={all.length ? "bad" : "ok"}>{t.doctor.problems(all.length)}</Badge>}>{t.doctor.title}</PaneTitle>
      <div className="min-h-0 flex-1 overflow-y-auto pb-4">
        {all.length === 0 && <Empty title={t.doctor.allGood} />}
        {SEVERITIES.map((severity) => {
          const rows = all.filter((diagnostic) => diagnostic.severity === severity);
          if (rows.length === 0) return null;
          return (
            <section key={severity} aria-label={t.severity[severity]}>
              <h3 className="flex items-center gap-2 px-3 pb-1 pt-3 text-[12px] font-semibold uppercase tracking-wide text-faint">
                {t.severity[severity]}
                <Badge tone={TONE[severity]}>{rows.length}</Badge>
              </h3>
              <ul>
                {rows.map((diagnostic, index) => (
                  <Row key={index} diagnostic={diagnostic} route={route} go={go} />
                ))}
              </ul>
            </section>
          );
        })}
      </div>
    </>
  );
}

interface Target {
  label: string;
  patch: Partial<Route>;
}

function useTarget(diagnostic: Diagnostic): Target {
  const project = useProject();
  const { t } = useI18n();
  const at = diagnostic.at;
  switch (at.kind) {
    case "file":
      return { label: `${project.display(at.path)}${at.line ? `:${at.line}` : ""}`, patch: { view: "context", sel: `file:${at.path}`, ag: diagnostic.agent ?? "", ln: at.line ?? 0 } };
    case "load": {
      const load = project.load(at.agent, at.load);
      const file = load?.file ?? "";
      return { label: project.display(file), patch: { view: "context", sel: `file:${file}`, ag: at.agent, ln: 0 } };
    }
    case "hook": {
      const hook = project.snapshot.agents.flatMap((v) => v.hooks).find((h) => h.id === at.hook);
      return { label: hook ? `${hook.event} ${hook.handler.summary}` : at.hook, patch: { view: "hooks", sel: `hook:${at.hook}`, ag: diagnostic.agent ?? "", ln: 0 } };
    }
    case "extension": {
      const ext = project.snapshot.agents.flatMap((v) => v.extensions).find((e) => e.id === at.ext);
      return { label: ext ? `${t.extKind[ext.extKind]} ${ext.name}` : at.ext, patch: { view: "context", sel: ext ? `file:${ext.file}` : "", ag: diagnostic.agent ?? "", ln: 0 } };
    }
    case "agent":
      return { label: diagnostic.agent ? project.name(diagnostic.agent) : t.doctor.agentless, patch: { view: "context", sel: "", ag: diagnostic.agent ?? "", ln: 0 } };
  }
}

function Row({ diagnostic, route, go }: { diagnostic: Diagnostic; route: Route; go: Go }) {
  const { t, d } = useI18n();
  const project = useProject();
  const target = useTarget(diagnostic);
  const write = useWrite();
  const selected = target.patch.sel !== "" && route.sel === target.patch.sel;
  const tone = TONE[diagnostic.severity];
  const badgeTone = { bad: "text-bad bg-bad-bg", warn: "text-warn bg-warn-bg", info: "text-info bg-info-bg" }[tone];
  return (
    <li
      data-testid="diagnostic"
      data-problem={diagnostic.problem.kind}
      aria-selected={selected}
      className={`flex gap-2 border-l-2 px-3 py-1.5 hover:bg-subtle ${selected ? "border-accent bg-accent-bg" : "border-transparent"}`}
      onClick={() => go({ sel: target.patch.sel ?? "", ag: target.patch.ag ?? "", ln: target.patch.ln ?? 0 })}
    >
      <span className={`mt-0.5 grid size-4 shrink-0 place-items-center rounded-sm text-[11px] font-bold ${badgeTone}`} aria-label={t.severityOne[diagnostic.severity]}>
        {MARK[diagnostic.severity]}
      </span>
      <div className="min-w-0 flex-1">
        <p>{say(t.problem, diagnostic.problem, d)}</p>
        <div className="mt-0.5 flex flex-wrap items-center gap-2 text-[11px]">
          {diagnostic.agent && (
            <span className="inline-flex items-center gap-1 text-muted">
              <span className="size-2 rounded-full" style={{ background: project.color(diagnostic.agent) }} aria-hidden />
              {project.name(diagnostic.agent)}
            </span>
          )}
          <span className="text-muted">{t.doctor.subject}:</span>
          <button
            type="button"
            className="min-w-0 truncate text-left text-accent underline"
            title={t.doctor.openSubject}
            onClick={(event) => {
              event.stopPropagation();
              go(target.patch);
            }}
          >
            <Mono className="text-[11px]">{target.label}</Mono>
          </button>
          {diagnostic.fix && (
            <Button
              disabled={write.blocked !== null}
              title={write.blocked ?? t.doctor.fixHint}
              onClick={(event) => {
                event.stopPropagation();
                if (diagnostic.fix) write.propose({ edit: diagnostic.fix, title: t.doctor.fixTitle });
              }}
            >
              {t.doctor.fix}
            </Button>
          )}
        </div>
      </div>
    </li>
  );
}
