// Hook controls: the on/off switch, the add and edit form built from `HookSchema`, and the test run.
import { useQuery } from "@tanstack/react-query";
import { useEffect, useMemo, useState } from "react";
import { call } from "../api/client";
import { useProject } from "../app/project";
import { Badge, Button, FormRow, inputClass, Modal, Mono, PrimaryButton } from "../components/ui";
import type { AbsPath } from "../gen/AbsPath";
import type { AgentId } from "../gen/AgentId";
import type { HandlerField } from "../gen/HandlerField";
import type { Hook } from "../gen/Hook";
import type { HookDraft } from "../gen/HookDraft";
import type { HookRun } from "../gen/HookRun";
import type { JsonValue } from "../gen/serde_json/JsonValue";
import { useI18n } from "../i18n";
import { say } from "../i18n/types";
import { useWrite } from "./flow";

/** The on/off switch of a hook. When it is unavailable the reason sits next to it. */
export function HookSwitch({ hook, showReason = false }: { hook: Hook; showReason?: boolean }) {
  const { t } = useI18n();
  const project = useProject();
  const { propose, blocked } = useWrite();
  const off = hook.state.kind === "disabled";
  const unavailable = hook.toggle.kind === "unavailable" ? project.rule(hook.toggle.why) : null;
  const reason = unavailable ?? blocked;
  return (
    <span className="inline-flex items-center gap-2">
      <Button
        role="switch"
        aria-checked={!off}
        disabled={reason !== null}
        title={reason ?? (off ? t.hooks.turnOn : t.hooks.turnOff)}
        onClick={(event) => {
          event.stopPropagation();
          propose({
            edit: { kind: "setHookEnabled", hook: hook.id, enabled: off },
            title: off ? t.write.hookOnTitle(hook.event) : t.write.hookOffTitle(hook.event),
            firstUse: hook.toggle.kind === "park" ? { key: "park", title: t.write.parkTitle, body: t.write.parkBody } : undefined,
          });
        }}
      >
        {off ? t.hooks.offLabel : t.hooks.on}
      </Button>
      {showReason && unavailable && <span className="text-[11px] text-muted">{unavailable}</span>}
    </span>
  );
}

/** Edit, delete and the switch, for the toolbar of the hook detail. */
export function HookActions({ agent, hook, onTest }: { agent: AgentId; hook: Hook; onTest: () => void }) {
  const { t } = useI18n();
  const { propose, blocked } = useWrite();
  const project = useProject();
  const [editing, setEditing] = useState(false);
  const locked = hook.toggle.kind === "unavailable" ? project.rule(hook.toggle.why) : blocked;
  return (
    <>
      <Button disabled={blocked !== null} title={blocked ?? undefined} onClick={onTest}>{t.hooks.test}</Button>
      <HookSwitch hook={hook} />
      <Button disabled={locked !== null} title={locked ?? undefined} onClick={() => setEditing(true)}>
        {t.write.edit}
      </Button>
      <Button
        disabled={locked !== null}
        title={locked ?? undefined}
        onClick={() => propose({ edit: { kind: "removeHook", hook: hook.id }, title: t.write.removeHookTitle(hook.event) })}
      >
        {t.write.delete}
      </Button>
      {editing && <HookForm agent={agent} hook={hook} onClose={() => setEditing(false)} />}
    </>
  );
}

export function AddHookButton({ agent }: { agent: AgentId }) {
  const { t } = useI18n();
  const project = useProject();
  const { blocked } = useWrite();
  const [open, setOpen] = useState(false);
  const files = project.view(agent)?.hookFiles.filter((file) => file.policy.kind === "editable") ?? [];
  const reason = blocked ?? (files.length === 0 ? t.write.noHookFile : null);
  return (
    <>
      <Button disabled={reason !== null} title={reason ?? undefined} onClick={() => setOpen(true)}>
        {t.write.addHook}
      </Button>
      {open && <HookForm agent={agent} hook={null} onClose={() => setOpen(false)} />}
    </>
  );
}

type Handler = Record<string, JsonValue | undefined>;

const asHandler = (value: JsonValue | undefined): Handler => (typeof value === "object" && value !== null && !Array.isArray(value) ? (value as Handler) : {});

function setKey(handler: Handler, key: string, value: JsonValue | undefined): Handler {
  const next = { ...handler };
  if (value === undefined) delete next[key];
  else next[key] = value;
  return next;
}

function FieldInput({ field, value, onChange }: { field: HandlerField; value: JsonValue | undefined; onChange: (value: JsonValue | undefined) => void }) {
  const { t } = useI18n();
  const [invalid, setInvalid] = useState(false);
  switch (field.ty) {
    case "string":
      return <input className={inputClass} value={typeof value === "string" ? value : ""} onChange={(e) => onChange(e.target.value === "" ? undefined : e.target.value)} />;
    case "number":
      return <input className={inputClass} type="number" min={0} value={typeof value === "number" ? value : ""} onChange={(e) => onChange(e.target.value === "" ? undefined : Number(e.target.value))} />;
    case "bool":
      return <input type="checkbox" className="size-4 accent-[var(--accent)]" checked={value === true} onChange={(e) => onChange(e.target.checked ? true : undefined)} />;
    case "stringList":
      return (
        <textarea
          className={`${inputClass} font-mono`}
          rows={3}
          value={Array.isArray(value) ? value.join("\n") : ""}
          onChange={(e) => {
            const lines = e.target.value.split("\n").filter((line) => line.trim() !== "");
            onChange(lines.length === 0 ? undefined : lines);
          }}
        />
      );
    case "stringMap":
      return (
        <textarea
          className={`${inputClass} font-mono`}
          rows={3}
          placeholder="KEY=value"
          value={typeof value === "object" && value !== null && !Array.isArray(value) ? Object.entries(value).map(([k, v]) => `${k}=${String(v)}`).join("\n") : ""}
          onChange={(e) => {
            const pairs = e.target.value.split("\n").flatMap((line) => (line.includes("=") ? [[line.slice(0, line.indexOf("=")), line.slice(line.indexOf("=") + 1)] as const] : []));
            onChange(pairs.length === 0 ? undefined : Object.fromEntries(pairs));
          }}
        />
      );
    case "json":
      return (
        <>
          <textarea
            className={`${inputClass} font-mono`}
            rows={3}
            defaultValue={value === undefined ? "" : JSON.stringify(value, null, 2)}
            onChange={(e) => {
              if (e.target.value.trim() === "") {
                setInvalid(false);
                onChange(undefined);
                return;
              }
              try {
                onChange(JSON.parse(e.target.value) as JsonValue);
                setInvalid(false);
              } catch {
                setInvalid(true);
              }
            }}
          />
          {invalid && <span className="text-[11px] text-bad">{t.write.jsonInvalid}</span>}
        </>
      );
  }
}

export function HookForm({ agent, hook, onClose }: { agent: AgentId; hook: Hook | null; onClose: () => void }) {
  const { t } = useI18n();
  const project = useProject();
  const { propose } = useWrite();
  const info = project.infos.find((item) => item.id === agent);
  const schema = info?.hookSchema;
  const files = (project.view(agent)?.hookFiles ?? []).filter((file) => file.policy.kind === "editable");
  const [event, setEvent] = useState(hook?.event ?? schema?.events[0]?.name ?? "");
  const [matcher, setMatcher] = useState(hook?.matcher.raw ?? "");
  const [handler, setHandler] = useState<Handler>(asHandler(hook?.handler.raw ?? { type: schema?.handlerKinds[0]?.kind ?? "command" }));
  const [file, setFile] = useState<AbsPath>(files[0]?.file ?? "");
  const [tab, setTab] = useState<"form" | "raw">("form");
  const [raw, setRaw] = useState(() => JSON.stringify(handler, null, 2));
  const [rawError, setRawError] = useState<string | null>(null);

  const kind = typeof handler.type === "string" ? handler.type : "";
  const eventSpec = schema?.events.find((spec) => spec.name === event);
  const kindSpec = schema?.handlerKinds.find((spec) => spec.kind === kind);
  const matcherKind = eventSpec?.matcher ?? "none";
  const eventNames = useMemo(() => [...new Set([...(schema?.events.map((spec) => spec.name) ?? []), ...(hook ? [hook.event] : [])])], [schema, hook]);
  const kinds = useMemo(() => [...new Set([...(schema?.handlerKinds.map((spec) => spec.kind) ?? []), ...(kind ? [kind] : [])])], [schema, kind]);

  const update = (next: Handler) => {
    setHandler(next);
    setRaw(JSON.stringify(next, null, 2));
  };
  const changeKind = (next: string) => {
    const old = new Set(kindSpec?.fields.map((f) => f.key) ?? []);
    const keep = new Set(schema?.handlerKinds.find((spec) => spec.kind === next)?.fields.map((f) => f.key) ?? []);
    const cleaned = Object.fromEntries(Object.entries(handler).filter(([key]) => key === "type" || !old.has(key) || keep.has(key)));
    update({ ...cleaned, type: next });
  };

  const missing = (kindSpec?.fields ?? []).filter((field) => {
    const value = handler[field.key];
    return field.required && (value === undefined || value === "" || (Array.isArray(value) && value.length === 0));
  });
  const problem = rawError ? t.write.rawInvalid(rawError) : !event ? t.write.needEvent : !kind ? t.write.needKind : !hook && !file ? t.write.noHookFile : missing[0] ? t.write.needField(missing[0].key) : null;

  const submit = () => {
    const draft: HookDraft = { event, matcher: matcherKind === "none" || matcher.trim() === "" ? null : matcher.trim(), handler: handler as JsonValue };
    propose({
      edit: hook ? { kind: "updateHook", hook: hook.id, draft } : { kind: "addHook", agent, file, draft },
      title: hook ? t.write.updateHookTitle(event) : t.write.addHookTitle(project.name(agent), event),
      onApplied: onClose,
    });
  };

  return (
    <Modal
      title={hook ? t.write.editHook : t.write.addHook}
      label={t.write.hookForm}
      onClose={onClose}
      footer={
        <>
          {problem && <span className="mr-auto text-bad">{problem}</span>}
          <Button onClick={onClose}>{t.write.cancel}</Button>
          <PrimaryButton disabled={problem !== null} onClick={submit}>
            {t.write.review}
          </PrimaryButton>
        </>
      }
    >
      <div role="tablist" aria-label={t.write.hookForm} className="mb-3 flex gap-1 border-b border-line">
        {(["form", "raw"] as const).map((name) => (
          <button
            key={name}
            type="button"
            role="tab"
            aria-selected={tab === name}
            disabled={name === "form" && rawError !== null}
            onClick={() => setTab(name)}
            className="-mb-px border-b-2 border-transparent px-3 py-1.5 aria-selected:border-accent aria-selected:font-semibold disabled:opacity-50"
          >
            {name === "form" ? t.write.tabForm : t.write.tabRaw}
          </button>
        ))}
      </div>
      <div className="space-y-3">
        {!hook && (
          <FormRow label={t.write.hookFile} hint={t.write.hookFileHint}>
            <select className={inputClass} value={file} onChange={(e) => setFile(e.target.value)}>
              {files.map((entry) => (
                <option key={entry.file} value={entry.file}>
                  {project.display(entry.file)} ({say(t.scope, entry.scope, project.display)}){entry.exists ? "" : ` ${t.write.willCreate}`}
                </option>
              ))}
            </select>
          </FormRow>
        )}
        <FormRow label={t.write.event}>
          <select className={inputClass} value={event} onChange={(e) => setEvent(e.target.value)}>
            {eventNames.map((name) => (
              <option key={name}>{name}</option>
            ))}
          </select>
        </FormRow>
        {tab === "form" ? (
          <>
            {matcherKind !== "none" && (
              <FormRow label={t.hooks.matcher} hint={t.write.matcherHint[matcherKind]}>
                <input className={`${inputClass} font-mono`} value={matcher} onChange={(e) => setMatcher(e.target.value)} placeholder={t.hooks.anyTool} />
              </FormRow>
            )}
            <FormRow label={t.write.handlerKind}>
              <select className={inputClass} value={kind} onChange={(e) => changeKind(e.target.value)}>
                {kinds.map((name) => (
                  <option key={name}>{name}</option>
                ))}
              </select>
            </FormRow>
            {kindSpec?.fields.map((field) => (
              <FormRow key={field.key} label={`${field.key}${field.required ? " *" : ""}`}>
                <FieldInput field={field} value={handler[field.key]} onChange={(value) => update(setKey(handler, field.key, value))} />
              </FormRow>
            ))}
            {!kindSpec && kind && <p className="text-muted">{t.write.unknownKind(kind)}</p>}
            <p className="text-[11px] text-muted">{t.write.unknownKeys}</p>
          </>
        ) : (
          <FormRow label={t.write.rawHandler} hint={t.write.rawHint}>
            <textarea
              className={`${inputClass} font-mono`}
              rows={12}
              spellCheck={false}
              value={raw}
              onChange={(e) => {
                setRaw(e.target.value);
                try {
                  const parsed = JSON.parse(e.target.value) as JsonValue;
                  if (typeof parsed !== "object" || parsed === null || Array.isArray(parsed)) throw new Error(t.write.rawNotObject);
                  setHandler(parsed as Handler);
                  setRawError(null);
                } catch (error) {
                  setRawError(error instanceof Error ? error.message : String(error));
                }
              }}
            />
          </FormRow>
        )}
      </div>
    </Modal>
  );
}

/** Runs the hook's command with an input the operator can edit, and shows everything the run produced. */
export function HookTest({ agent, hook }: { agent: AgentId; hook: Hook }) {
  const { t } = useI18n();
  const project = useProject();
  const { blocked } = useWrite();
  const sample = useQuery({ queryKey: ["hook_sample", project.cwd, agent, hook.event], queryFn: () => call("hook_sample", { cwd: project.cwd, agent, event: hook.event }) });
  const [input, setInput] = useState("");
  const [seeded, setSeeded] = useState(false);
  const [state, setState] = useState<{ kind: "idle" } | { kind: "running" } | { kind: "done"; run: HookRun } | { kind: "error"; error: unknown }>({ kind: "idle" });
  useEffect(() => {
    if (sample.data !== undefined && !seeded) {
      setInput(JSON.stringify(sample.data, null, 2));
      setSeeded(true);
    }
  }, [sample.data, seeded]);

  let parsed: JsonValue | undefined;
  let parseError: string | null = null;
  try {
    parsed = JSON.parse(input) as JsonValue;
  } catch (error) {
    parseError = error instanceof Error ? error.message : String(error);
  }

  const run = async () => {
    if (parsed === undefined) return;
    setState({ kind: "running" });
    try {
      setState({ kind: "done", run: await call("run_hook", { cwd: project.cwd, hook: hook.id, input: parsed }) });
    } catch (error) {
      setState({ kind: "error", error });
    }
  };

  return (
    <section aria-label={t.hooks.test} id="hook-test" className="border-t border-line px-4 py-3">
      <h3 className="mb-1 text-[12px] font-semibold uppercase tracking-wide text-faint">{t.hooks.test}</h3>
      <p className="mb-2 rounded border border-warn bg-warn-bg p-2 text-warn">{t.hooks.testWarning}</p>
      <FormRow label={t.hooks.testInput} hint={sample.isPending ? t.loading : t.hooks.testInputHint}>
        <textarea className={`${inputClass} font-mono`} rows={6} spellCheck={false} value={input} onChange={(e) => setInput(e.target.value)} />
      </FormRow>
      {parseError && <p className="mt-1 text-bad">{t.write.rawInvalid(parseError)}</p>}
      <div className="mt-2">
        <PrimaryButton disabled={blocked !== null || parsed === undefined || state.kind === "running"} title={blocked ?? undefined} onClick={() => void run()}>
          {state.kind === "running" ? t.hooks.running : t.hooks.run}
        </PrimaryButton>
      </div>
      {state.kind === "error" && <p role="alert" className="mt-2 rounded border border-bad bg-bad-bg p-2 text-bad">{String(typeof state.error === "object" && state.error !== null && "detail" in state.error ? (state.error as { detail: string }).detail : JSON.stringify(state.error))}</p>}
      {state.kind === "done" && <RunResult run={state.run} />}
    </section>
  );
}

function RunResult({ run }: { run: HookRun }) {
  const { t } = useI18n();
  const ok = run.exitCode === 0 && !run.timedOut;
  return (
    <div className="mt-3 space-y-2" aria-label={t.hooks.result}>
      <div className="flex flex-wrap items-center gap-2">
        <Badge tone={ok ? "ok" : "bad"}>{run.exitCode === null ? t.hooks.noExit : t.hooks.exit(run.exitCode)}</Badge>
        {run.timedOut && <Badge tone="bad">{t.hooks.timedOut}</Badge>}
        <span className="text-muted tabular-nums">{t.hooks.duration(run.ms)}</span>
      </div>
      <dl className="grid grid-cols-[5rem_1fr] gap-x-2 gap-y-1">
        <dt className="text-muted">{t.hooks.command}</dt>
        <dd><Mono className="break-all">{run.spec.argv.join(" ")}</Mono></dd>
        <dt className="text-muted">{t.hooks.runCwd}</dt>
        <dd><Mono className="break-all">{run.spec.cwd}</Mono></dd>
        {run.spec.env.length > 0 && (
          <>
            <dt className="text-muted">{t.hooks.env}</dt>
            <dd><Mono className="break-all">{run.spec.env.map(([k, v]) => `${k}=${v}`).join(" ")}</Mono></dd>
          </>
        )}
      </dl>
      <Output label={t.hooks.stdout} text={run.stdout} />
      <Output label={t.hooks.stderr} text={run.stderr} />
    </div>
  );
}

function Output({ label, text }: { label: string; text: string }) {
  const { t } = useI18n();
  return (
    <div>
      <p className="text-[12px] font-semibold">{label}</p>
      <pre className="max-h-40 overflow-auto whitespace-pre-wrap break-words rounded border border-line bg-subtle p-2 font-mono text-[12px]">{text === "" ? t.hooks.empty : text}</pre>
    </div>
  );
}
