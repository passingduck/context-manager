import { useMemo, type ReactNode } from "react";
import { useChange, useFile } from "../api/queries";
import { useProject } from "../app/project";
import { CodeEditor, DiffView } from "../components/CodeEditor";
import { Badge, Button, Chip, ErrorNote, Loading, Mono } from "../components/ui";
import type { FileEntry } from "../gen/FileEntry";
import type { Hook } from "../gen/Hook";
import type { Load } from "../gen/Load";
import { useI18n } from "../i18n";
import { say } from "../i18n/types";
import * as fmt from "../lib/format";
import { activationTone, statusTone, truncationLine } from "../lib/loads";
import type { Route } from "../lib/route";

type Go = (patch: Partial<Route>) => void;

export function Detail({ route, go }: { route: Route; go: Go }) {
  const { t } = useI18n();
  const [kind, ...rest] = route.sel.split(":");
  const id = rest.join(":");
  if (kind === "file" && id) return <FileDetail key={id} path={id} route={route} />;
  if (kind === "hook" && id) return <HookDetail id={id} go={go} />;
  if (kind === "change" && id) return <ChangeDetailView id={id} />;
  return (
    <div className="grid h-full place-items-center p-6 text-center text-muted">
      <p className="max-w-xs">{t.detail.empty}</p>
    </div>
  );
}

function Field({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div className="grid grid-cols-[5.5rem_1fr] gap-2 py-0.5">
      <dt className="text-muted">{label}</dt>
      <dd className="min-w-0 break-words">{children}</dd>
    </div>
  );
}

function Toolbar({ left }: { left: ReactNode }) {
  const { t } = useI18n();
  return (
    <div className="flex items-center gap-2 border-b border-line bg-subtle px-3 py-1.5">
      <div className="flex min-w-0 flex-1 items-center gap-2">{left}</div>
      <Button disabled title={t.detail.editsLater}>{t.detail.revert}</Button>
      <Button disabled title={t.detail.editsLater}>{t.detail.trash}</Button>
      <Button disabled title={t.detail.editsLater} className="border-accent bg-accent text-bg">{t.detail.save}</Button>
    </div>
  );
}

function FileDetail({ path, route }: { path: string; route: Route }) {
  const { t, d } = useI18n();
  const project = useProject();
  const entry: FileEntry | undefined = project.snapshot.files[path];
  const uses = useMemo(() => {
    const all = project.snapshot.agents.flatMap((view) => view.loads.filter((l) => l.file === path).map((load) => ({ agent: view.agent, load })));
    return all.sort((a, b) => Number(b.agent === route.ag) - Number(a.agent === route.ag));
  }, [project.snapshot, path, route.ag]);
  const rev = entry?.state.kind === "present" ? entry.state.rev : "none";
  const file = useFile(project.cwd, entry?.state.kind === "present" ? path : null, rev);
  const lead = uses[0];
  const cutLine = lead && file.data?.text ? truncationLine(file.data.text, lead.load.status) : null;

  if (!entry) {
    return <div className="p-4 text-muted"><Mono>{path}</Mono></div>;
  }
  const state = entry.state;
  return (
    <div className="flex h-full min-h-0 flex-col">
      <div className="max-h-[58%] shrink-0 overflow-y-auto">
      <header className="border-b border-line px-4 py-3">
        <div className="flex flex-wrap items-center gap-2">
          <h2 className="break-all font-mono text-[14px] font-semibold">{entry.display}</h2>
          {lead && <Badge tone={activationTone(lead.load.when)}>{say(t.activation.badge, lead.load.when, d)}</Badge>}
        </div>
        <dl className="mt-2">
          <Field label={t.detail.path}><Mono className="break-all text-muted">{path}</Mono></Field>
          <Field label={t.detail.format}>{t.format[entry.format]}</Field>
          {state.kind === "present" && (
            <Field label={t.detail.size}>
              {fmt.bytes(state.bytes)}, {t.detail.lines(state.lines)}, {t.detail.tokenCount(fmt.tokens(state.tokens))}
            </Field>
          )}
          {entry.agents.length > 0 && (
            <Field label={t.detail.agents}>
              <span className="flex flex-wrap gap-x-3">
                {entry.agents.map((agent) => (
                  <span key={agent} className="inline-flex items-center gap-1">
                    <span className="size-2 rounded-full" style={{ background: project.color(agent) }} aria-hidden />
                    {project.name(agent)}
                  </span>
                ))}
              </span>
            </Field>
          )}
        </dl>
        {entry.realPath && <p className="mt-1 text-muted">{t.detail.symlink(d(entry.realPath))}</p>}
        {entry.policy.kind === "readOnly" && <p className="mt-1 text-muted">{t.detail.readOnlyFile} {say(t.readOnlyWhy, entry.policy.why, d)}</p>}
      </header>

      <section className="px-4 py-3" aria-label={t.detail.why}>
        <h3 className="mb-1.5 text-[12px] font-semibold uppercase tracking-wide text-faint">{t.detail.why}</h3>
        {uses.length === 0 && <p className="rounded border border-line bg-subtle p-2 text-muted">{t.detail.notLoaded}</p>}
        <div className="space-y-2">
          {uses.map(({ agent, load }) => (
            <WhyCard key={`${agent}:${load.id}`} agent={agent} load={load} text={file.data?.text ?? null} />
          ))}
        </div>
      </section>
      </div>

      <section className="flex min-h-[12rem] flex-1 flex-col border-t border-line" aria-label={t.detail.text}>
        <Toolbar
          left={
            <>
              <Chip>{t.format[entry.format]}</Chip>
              {state.kind === "present" && <span className="text-muted">{t.detail.lines(state.lines)}</span>}
              {cutLine && <Badge tone="bad">{t.detail.cutLine(cutLine)}</Badge>}
            </>
          }
        />
        <div className="min-h-0 flex-1">
          {state.kind === "absent" && <p className="p-4 text-muted">{t.detail.absent}</p>}
          {state.kind === "unreadable" && <p className="p-4 text-muted">{t.detail.unreadable(state.error)}</p>}
          {state.kind === "tooBig" && <p className="p-4 text-muted">{t.detail.tooBig(fmt.bytes(state.bytes))}</p>}
          {state.kind === "present" && file.isPending && <Loading />}
          {state.kind === "present" && file.isError && <ErrorNote error={file.error} />}
          {state.kind === "present" && file.data?.text != null && (
            <CodeEditor key={`${path}:${rev}`} text={file.data.text} format={entry.format} cutLine={cutLine} cutTitle={t.detail.cutMarker} revealLine={route.ln} />
          )}
        </div>
      </section>
    </div>
  );
}

function WhyCard({ agent, load, text }: { agent: string; load: Load; text: string | null }) {
  const { t, d } = useI18n();
  const project = useProject();
  const view = project.view(agent);
  const tone = statusTone(load.status);
  const via = load.via;
  const ext = via.kind === "listing" ? view?.extensions.find((e) => via.kind === "listing" && e.id === via.ext) : undefined;
  const host = via.kind === "import" || via.kind === "reference" ? project.load(agent, via.host) : undefined;
  const rules: string[] = [];
  if (via.kind === "discovery") rules.push(via.rule);
  if (load.status.kind === "skipped" && load.status.skip.kind === "notRead") rules.push(load.status.skip.why);
  if (ext?.status.kind === "hidden" || ext?.status.kind === "rejected") rules.push(ext.status.why);
  const cut = load.status.kind === "truncated" ? load.status : null;
  const line = cut && text ? truncationLine(text, load.status) : null;

  return (
    <article className="rounded border border-line bg-subtle p-2.5">
      <div className="mb-1 flex flex-wrap items-center gap-2">
        <span className="size-2 rounded-full" style={{ background: project.color(agent) }} aria-hidden />
        <b>{project.name(agent)}</b>
        <Badge tone={activationTone(load.when)}>{say(t.activation.badge, load.when, d)}</Badge>
        {tone && <Badge tone={tone}>{load.status.kind === "skipped" ? say(t.skipShort, load.status.skip, d) : say(t.status.badge, load.status, d)}</Badge>}
        <Chip>{t.docKind[load.kind]}</Chip>
        <Chip>{say(t.scope, load.scope, d)}</Chip>
        <span className="ml-auto tabular-nums text-muted">{load.delivered ? t.detail.tokenCount(fmt.tokens(load.delivered.tokens)) : ""}</span>
      </div>
      <dl>
        <Field label={t.detail.ruleLabel}>
          <div className="space-y-0.5">
            {rules.map((id) => <p key={id}>{project.rule(id)}</p>)}
            <p>{host && via.kind === "import" ? t.detail.importedAt(project.display(host.file), via.line, via.depth) : host && via.kind === "reference" ? t.detail.referencedAt(project.display(host.file), via.line, via.depth) : say(t.via, via, d)}</p>
            {ext && <p>{t.extKind[ext.extKind]} {ext.name}: {say(t.extStatus, ext.status, d)}</p>}
          </div>
        </Field>
        <Field label={t.detail.whenLabel}>{say(t.activation.detail, load.when, d)}</Field>
        <Field label={t.detail.statusLabel}>
          <div className="space-y-0.5">
            <p>{say(t.status.detail, load.status, d)}</p>
            {load.status.kind === "skipped" && <p>{say(t.skip, load.status.skip, d)}</p>}
            {cut && <p>{t.detail.cutAt(cut.kept, cut.total, t.unit[cut.limit.unit], t.limit(cut.limit))}{line ? ` ${t.detail.cutLine(line)}` : ""}</p>}
          </div>
        </Field>
      </dl>
    </article>
  );
}

function HookDetail({ id, go }: { id: string; go: Go }) {
  const { t, d } = useI18n();
  const project = useProject();
  const found = project.snapshot.agents.flatMap((view) => view.hooks.map((hook) => ({ agent: view.agent, hook }))).find((item) => item.hook.id === id);
  if (!found) return <p className="p-4 text-muted">{id}</p>;
  const { agent, hook } = found;
  return (
    <div className="flex h-full min-h-0 flex-col overflow-y-auto">
      <header className="shrink-0 border-b border-line px-4 py-3">
        <div className="flex flex-wrap items-center gap-2">
          <span className="size-2 rounded-full" style={{ background: project.color(agent) }} aria-hidden />
          <h2 className="text-[14px] font-semibold">{project.name(agent)} {hook.event}</h2>
          <HookStateBadge hook={hook} />
        </div>
        <dl className="mt-2">
          <Field label={t.hooks.matcher}>{hook.matcher.raw ? <Mono>{hook.matcher.raw}</Mono> : t.hooks.anyTool} ({say(t.matcherEval, hook.matcher.eval, d)})</Field>
          <Field label={t.hooks.handler}><Chip>{hook.handler.handlerKind}</Chip> <Mono className="break-all">{hook.handler.summary}</Mono></Field>
          <Field label={t.hooks.state}>{say(t.hookState, hook.state, d)}</Field>
          <Field label={t.hooks.source}>
            <button type="button" className="text-left text-accent underline" onClick={() => go({ sel: `file:${hook.source.file}`, ag: agent, view: "context" })}>
              <Mono className="break-all">{project.display(hook.source.file)}</Mono>
            </button>
          </Field>
          <Field label={t.hooks.location}>{say(t.locator, hook.source.at, d)}</Field>
          <Field label={t.hooks.scope}>{say(t.scope, hook.source.scope, d)}</Field>
          <Field label={t.hooks.registered}>{say(t.registration, hook.source.registered, d)}</Field>
          <Field label={t.hooks.switch}>{toggleText(hook, project.rule, t)}</Field>
        </dl>
      </header>
      <section className="flex min-h-[14rem] flex-1 flex-col" aria-label={t.hooks.raw}>
        <Toolbar left={<><Chip>JSON</Chip><span className="text-muted">{t.hooks.raw}</span></>} />
        <div className="min-h-0 flex-1">
          <CodeEditor key={hook.id} text={JSON.stringify(hook.handler.raw, null, 2)} format="json" />
        </div>
      </section>
    </div>
  );
}

export function toggleText(hook: Hook, rule: (id: string) => string, t: ReturnType<typeof useI18n>["t"]): string {
  switch (hook.toggle.kind) {
    case "native":
      return t.hooks.switchNative;
    case "park":
      return t.hooks.switchPark;
    case "unavailable":
      return `${t.hooks.switchWhyNot}. ${rule(hook.toggle.why)}`;
  }
}

export function HookStateBadge({ hook }: { hook: Hook }) {
  const { t, d } = useI18n();
  const tone = hook.state.kind === "active" ? "ok" : hook.state.kind === "disabled" ? "neutral" : "warn";
  return <Badge tone={tone} title={say(t.hookState, hook.state, d)}>{say(t.hookStateBadge, hook.state, d)}</Badge>;
}

function ChangeDetailView({ id }: { id: string }) {
  const { t, lang } = useI18n();
  const project = useProject();
  const change = useChange(id);
  if (change.isPending) return <Loading />;
  if (change.isError) return <ErrorNote error={change.error} onRetry={() => void change.refetch()} />;
  const { record, files } = change.data;
  return (
    <div className="h-full overflow-y-auto">
      <header className="border-b border-line px-4 py-3">
        <div className="flex flex-wrap items-center gap-2">
          <h2 className="text-[14px] font-semibold">{t.changeEdit[record.summary.edit.kind]}</h2>
          <Badge tone={record.state === "sealed" ? "ok" : record.state === "pending" ? "info" : "warn"}>{t.changeState[record.state]}</Badge>
          {record.summary.agent && <Chip>{project.name(record.summary.agent)}</Chip>}
          <span className="text-muted">{fmt.when(record.at, lang)}</span>
          <Button disabled title={t.history.revertLater} className="ml-auto">{t.detail.revert}</Button>
        </div>
      </header>
      <div className="space-y-4 p-4">
        {files.map((file) => (
          <section key={file.path}>
            <h3 className="mb-1 flex items-center gap-2">
              <Mono className="break-all font-semibold">{project.display(file.path)}</Mono>
              {file.before === null && <Badge tone="ok">{t.history.created}</Badge>}
              {file.after === null && <Badge tone="bad">{t.history.deleted}</Badge>}
            </h3>
            <div className="mb-1 grid grid-cols-2 gap-4 text-[11px] text-faint"><span>{t.history.before}</span><span>{t.history.after}</span></div>
            <DiffView before={file.before ?? ""} after={file.after ?? ""} format={project.snapshot.files[file.path]?.format ?? "markdown"} />
          </section>
        ))}
      </div>
    </div>
  );
}
