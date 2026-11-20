import { useEffect, useMemo, useRef, useState, type Dispatch, type KeyboardEvent, type SetStateAction } from "react";
import { useRender, useSimulate } from "../api/queries";
import { useProject } from "../app/project";
import { Badge, Chip, ErrorNote, Mono, PaneTitle } from "../components/ui";
import type { AgentView } from "../gen/AgentView";
import type { DocKind } from "../gen/DocKind";
import type { Load } from "../gen/Load";
import type { Piece } from "../gen/Piece";
import type { Simulation } from "../gen/Simulation";
import { useI18n } from "../i18n";
import type { BundledKind } from "../i18n/en";
import { say } from "../i18n/types";
import * as fmt from "../lib/format";
import { buildPrefix, exact } from "../lib/prefix";
import { activationTone, buildGroups, GROUPS, type GroupKey, isPicked, LABEL_COLOR, pick, type Picked, picked, sends, statusTone, type Node } from "../lib/loads";
import type { Route } from "../lib/route";
import { PrefixMap } from "./PrefixMap";
import { NewMenu } from "../write/create";
import { useDirtyPaths } from "../write/buffers";
import { RowMenu } from "../write/trash";

type Go = (patch: Partial<Route>) => void;

export function ContextView({ route, go }: { route: Route; go: Go }) {
  const { t } = useI18n();
  const project = useProject();
  const sims = useSimulate(project.cwd, route.sim, project.snapshot.fingerprint);
  const byAgent = new Map<string, Simulation>((route.sim ? (sims.data ?? []) : []).map((sim) => [sim.agent, sim]));
  const [focused, setFocused] = useState("");
  const [collapsed, setCollapsed] = useState<Set<string>>(new Set());
  const [expanded, setExpanded] = useState<Set<string>>(new Set());
  const snapshot = project.snapshot;
  const firstWithBlocks = snapshot.agents.find((view) => view.session.length > 0)?.agent;

  function onKeyDown(event: KeyboardEvent<HTMLDivElement>) {
    const rows = Array.from(event.currentTarget.querySelectorAll<HTMLElement>("[data-row]"));
    const index = rows.findIndex((row) => row === document.activeElement);
    const move = (to: number) => {
      const next = rows[Math.max(0, Math.min(rows.length - 1, to))];
      if (next) {
        event.preventDefault();
        next.focus();
      }
    };
    const key = (document.activeElement as HTMLElement | null)?.dataset.key ?? "";
    if (event.key === "ArrowDown") move(index + 1);
    else if (event.key === "ArrowUp") move(index - 1);
    else if (event.key === "Home") move(0);
    else if (event.key === "End") move(rows.length - 1);
    else if (key.startsWith(BUNDLE) && (event.key === "ArrowRight" || event.key === "ArrowLeft")) {
      event.preventDefault();
      const open = event.key === "ArrowRight";
      setExpanded((old) => {
        const next = new Set(old);
        if (open) next.add(key);
        else next.delete(key);
        return next;
      });
    } else if (event.key === "ArrowRight" && key && collapsed.has(key)) {
      event.preventDefault();
      setCollapsed((old) => new Set([...old].filter((k) => k !== key)));
    } else if (event.key === "ArrowLeft" && key) {
      const row = document.activeElement as HTMLElement;
      if (row.getAttribute("aria-expanded") === "true") {
        event.preventDefault();
        setCollapsed((old) => new Set(old).add(key));
      } else {
        const level = Number(row.getAttribute("aria-level"));
        for (let i = index - 1; i >= 0; i--) {
          if (Number(rows[i]!.getAttribute("aria-level")) < level) return move(i);
        }
      }
    }
  }

  const right = (
    <>
      <NewMenu go={go} />
      <div role="group" aria-label={t.context.title} className="flex overflow-hidden rounded border border-line text-[12px]">
        {(["tree", "order"] as const).map((mode) => (
          <button key={mode} type="button" data-testid={`mode-${mode}`} aria-pressed={route.mode === mode} onClick={() => go({ mode })} className="px-2 py-0.5 hover:bg-sunken aria-pressed:bg-accent-bg aria-pressed:font-semibold aria-pressed:text-accent">
            {mode === "tree" ? t.context.modeTree : t.context.modeOrder}
          </button>
        ))}
      </div>
    </>
  );

  return (
    <>
      <PaneTitle right={right}>{t.context.title}</PaneTitle>
      <div
        className="min-h-0 flex-1 overflow-y-auto"
        onKeyDown={onKeyDown}
        role="tree"
        aria-label={t.context.title}
        tabIndex={focused ? -1 : 0}
        onFocus={(event) => {
          if (event.target === event.currentTarget) event.currentTarget.querySelector<HTMLElement>("[data-row]")?.focus();
        }}
      >
        {route.sim && (
          <p className="border-b border-line bg-info-bg px-3 py-1.5 text-info">{t.context.simFor(route.sim)}</p>
        )}
        {snapshot.agents.map((view) => (
          <section key={view.agent} data-testid="agent" data-agent={view.agent} className="border-b border-line pb-2" aria-label={project.name(view.agent)}>
            <AgentHeader view={view} sim={byAgent.get(view.agent)} />
            <PrefixMap view={view} route={route} go={go} intro={view.agent === firstWithBlocks} />
            {route.mode === "tree" ? (
              <AgentTree view={view} route={route} go={go} picked={picked(view, route)} sim={byAgent.get(view.agent)} focused={focused} setFocused={setFocused} collapsed={collapsed} setCollapsed={setCollapsed} expanded={expanded} setExpanded={setExpanded} />
            ) : (
              <OrderBlocks view={view} route={route} go={go} />
            )}
          </section>
        ))}
        <p className="px-3 py-2 text-[11px] text-faint">
          {t.context.scanDirs(snapshot.walk.dirs, snapshot.walk.files)}
          {snapshot.walk.capped ? `. ${t.context.walkCapped}` : ""}
        </p>
      </div>
    </>
  );
}

function AgentHeader({ view, sim }: { view: AgentView; sim: Simulation | undefined }) {
  const { t, d } = useI18n();
  const project = useProject();
  const total = view.budget.byLabel.reduce((n, [, v]) => n + v, 0);
  return (
    <header className="px-3 pb-1 pt-3">
      <div className="flex flex-wrap items-center gap-x-2 gap-y-1">
        <span className="size-2.5 shrink-0 rounded-full" style={{ background: project.color(view.agent) }} aria-hidden />
        <h3 className="text-[14px] font-semibold">{project.name(view.agent)}</h3>
        <Badge tone={view.presence.kind === "installed" ? "ok" : view.presence.kind === "configOnly" ? "warn" : "neutral"} title={view.presence.kind === "installed" ? t.context.installedAt(view.presence.bin) : undefined}>
          {t.presence[view.presence.kind]}
        </Badge>
        <Badge tone={view.trust.level === "trusted" ? "ok" : view.trust.level === "untrusted" ? "bad" : "neutral"} title={view.trust.recordedIn ? t.trustRecorded(d(view.trust.recordedIn)) : undefined}>
          {t.trust[view.trust.level]}
        </Badge>
        {sim && <Badge tone={sim.addedTokens > 0 ? "warn" : "neutral"}>{sim.addedTokens > 0 ? t.context.simAdds(fmt.tokens(sim.addedTokens)) : t.context.simNothing}</Badge>}
        <span className="ml-auto flex items-baseline gap-1.5 whitespace-nowrap text-muted">
          {t.context.always}
          <b data-testid="agent-always" data-tokens={view.budget.sessionStart} className="text-[15px] text-fg tabular-nums">{fmt.tokens(view.budget.sessionStart)}</b>
        </span>
      </div>
      {total > 0 && (
        <>
          <div role="img" aria-label={t.context.labelBar} className="mt-2 flex h-1.5 overflow-hidden rounded-full bg-sunken">
            {view.budget.byLabel.map(([label, value]) => (
              <span key={label} title={`${t.blockLabel[label]} ${fmt.tokens(value)}`} style={{ width: `${(value / total) * 100}%`, background: LABEL_COLOR[label] }} />
            ))}
          </div>
          <ul className="mt-1.5 flex flex-wrap gap-x-3 gap-y-0.5 text-[11px] text-muted">
            {view.budget.byLabel.map(([label, value]) => (
              <li key={label} className="flex items-center gap-1">
                <span className="size-2 rounded-sm" style={{ background: LABEL_COLOR[label] }} aria-hidden />
                {t.blockLabel[label]} <span className="tabular-nums">{fmt.tokens(value)}</span>
              </li>
            ))}
            {view.budget.onAccessMax > 0 && (
              <li className="ml-auto">
                {t.context.onAccessMax} <span className="tabular-nums">+{fmt.tokens(view.budget.onAccessMax)}</span>
              </li>
            )}
          </ul>
        </>
      )}
    </header>
  );
}

interface TreeProps {
  view: AgentView;
  route: Route;
  go: Go;
  picked: Picked | null;
  sim: Simulation | undefined;
  focused: string;
  setFocused: (key: string) => void;
  collapsed: Set<string>;
  setCollapsed: (next: Set<string>) => void;
  /** Keys of the listing bundles that are open. A bundle is closed until it is in here. */
  expanded: Set<string>;
  setExpanded: Dispatch<SetStateAction<Set<string>>>;
}

const BUNDLE = "bundle:";

/** Catalog lines and the bodies behind them are many and alike, so each kind collapses into one row. */
const BUNDLED_KINDS: BundledKind[] = ["listing", "skillBody", "commandBody", "subagentPrompt", "reference"];
const BUNDLE_MIN = 2;

function AgentTree(props: TreeProps) {
  const { view, sim } = props;
  const { t, d } = useI18n();
  const project = useProject();
  const groups = useMemo(() => buildGroups(view), [view]);
  const empty = view.loads.length === 0;

  if (empty) {
    return (
      <div className="mx-3 mb-1 mt-2 rounded border border-dashed border-line p-3">
        <p className="font-semibold">{view.presence.kind === "absent" ? t.context.notInstalled : t.context.noLoads}</p>
        <ul className="mt-2 space-y-1">
          {view.homes.map((home) => (
            <li key={home.path} className="flex items-center gap-2">
              <Mono className="break-all">{d(home.path)}</Mono>
              {!home.exists && <Chip>{t.presence.absent}</Chip>}
            </li>
          ))}
          {view.slots.map((slot) => (
            <li key={slot.id} className="flex flex-wrap items-center gap-2 text-muted">
              <Mono className="break-all">{slot.place.kind === "file" ? d(slot.place.path) : slot.place.kind === "namedFile" ? `${d(slot.place.dir)}/*.${slot.place.ext}` : `${d(slot.place.dir)}/*/${slot.place.file}`}</Mono>
              <span>{project.rule(slot.rule)}</span>
            </li>
          ))}
        </ul>
      </div>
    );
  }

  return (
    <div>
      {GROUPS.map((group) => {
        const nodes = groups[group];
        if (nodes.length === 0) return null;
        return (
          <div key={group} role="group" aria-label={t.context.groups[group]}>
            <h4 className="px-3 pb-0.5 pt-2.5 text-[11px] font-semibold uppercase tracking-wide text-faint" title={t.context.groupHint[group]}>
              {t.context.groups[group]}
            </h4>
            {nodes.filter((node) => !isBundled(node, nodes)).map((node) => (
              <RowTree key={node.load.id} node={node} depth={1} {...props} />
            ))}
            {BUNDLED_KINDS.map((kind) => {
              const members = nodes.filter((node) => isBundled(node, nodes) && node.load.kind === kind);
              return members.length === 0 ? null : <BundleRows key={kind} kind={kind} group={group} members={members} {...props} />;
            })}
          </div>
        );
      })}
      {sim && sim.extra.length > 0 && (
        <div className="mt-1">
          <h4 className="px-3 pb-0.5 pt-2.5 text-[11px] font-semibold uppercase tracking-wide text-warn">{t.context.simExtra}</h4>
          {sim.extra.map((load) => (
            <div key={load.id} className="flex items-center gap-2 border-l-2 border-warn bg-warn-bg px-3 py-1">
              <Mono className="min-w-0 flex-1 truncate">{d(load.file)}</Mono>
              <Badge tone="warn">{t.context.simAdds(fmt.tokens(load.delivered?.tokens ?? 0))}</Badge>
            </div>
          ))}
        </div>
      )}
      {sim && sim.notes.length > 0 && (
        <p className="px-3 pt-1 text-muted">
          {t.context.simNotes}: {sim.notes.map(project.rule).join(" ")}
        </p>
      )}
    </div>
  );
}

const isBundled = (node: Node, siblings: Node[]) =>
  (BUNDLED_KINDS as DocKind[]).includes(node.load.kind) && siblings.filter((n) => n.load.kind === node.load.kind).length >= BUNDLE_MIN;

function BundleRows(props: TreeProps & { kind: BundledKind; group: GroupKey; members: Node[] }) {
  const { kind, group, members, view, expanded, setExpanded, focused, setFocused } = props;
  const { t } = useI18n();
  const key = `${BUNDLE}${view.agent}:${group}:${kind}`;
  const open = expanded.has(key);
  const total = members.reduce((n, node) => n + (node.load.delivered?.tokens ?? 0), 0);
  // A picked place inside the bundle opens it once, so the selected row shows after a reload or a click in the prefix map.
  const holdsPick = members.some((node) => node.load.id === props.picked?.load?.id);
  useEffect(() => {
    if (holdsPick) setExpanded((old) => (old.has(key) ? old : new Set(old).add(key)));
  }, [holdsPick, key, setExpanded]);
  const toggle = () => {
    const next = new Set(expanded);
    if (next.has(key)) next.delete(key);
    else next.add(key);
    setExpanded(next);
  };
  return (
    <>
      <div
        data-row
        data-key={key}
        role="treeitem"
        aria-level={1}
        aria-selected={false}
        aria-expanded={open}
        tabIndex={focused === key ? 0 : -1}
        onFocus={() => setFocused(key)}
        onClick={toggle}
        onKeyDown={(event) => {
          if (event.key === "Enter" || event.key === " ") {
            event.preventDefault();
            toggle();
          }
        }}
        className="flex min-h-7 cursor-default items-center gap-2 border-l-2 border-transparent py-0.5 pl-2 pr-3 hover:bg-subtle"
      >
        <span className="flex w-4 shrink-0 justify-center text-faint" aria-hidden>{open ? "▾" : "▸"}</span>
        <span className="min-w-0 flex-1 truncate font-medium">{t.context.bundle[kind](members.length)}</span>
        <span className="w-11 shrink-0 text-right tabular-nums">{fmt.tokens(total)}</span>
      </div>
      {open && members.map((node) => <RowTree key={node.load.id} {...props} node={node} depth={2} />)}
    </>
  );
}

function RowTree(props: TreeProps & { node: Node; depth: number }) {
  const { node, depth, collapsed } = props;
  const key = `${props.view.agent}:${node.load.id}`;
  const open = !collapsed.has(key);
  return (
    <>
      <LoadRow {...props} nodeKey={key} hasChildren={node.children.length > 0} open={open} />
      {open && node.children.map((child) => <RowTree key={child.load.id} {...props} node={child} depth={depth + 1} />)}
    </>
  );
}

function LoadRow(props: TreeProps & { node: Node; depth: number; nodeKey: string; hasChildren: boolean; open: boolean }) {
  const { node, depth, view, go, sim, nodeKey, hasChildren, open, focused, setFocused, collapsed, setCollapsed } = props;
  const { t, d } = useI18n();
  const project = useProject();
  const load = node.load;
  const entry = project.snapshot.files[load.file];
  const selected = isPicked(props.picked, load);
  const fires = sim?.fired.includes(load.id) ?? false;
  const ext = load.via.kind === "listing" ? view.extensions.find((e) => load.via.kind === "listing" && e.id === load.via.ext) : undefined;
  const label = ext ? ext.name : (entry?.display ?? d(load.file));
  const host = load.via.kind === "import" || load.via.kind === "reference" ? project.load(view.agent, load.via.host) : undefined;
  const hostLabel = host ? (project.snapshot.files[host.file]?.display ?? host.file) : "";
  const tokensValue = load.delivered?.tokens ?? (entry?.state.kind === "present" ? entry.state.tokens : null);
  const tone = statusTone(load.status);
  const isTabStop = focused === nodeKey;
  const unsaved = useDirtyPaths().includes(load.file);

  const toggle = () => {
    const next = new Set(collapsed);
    if (next.has(nodeKey)) next.delete(nodeKey);
    else next.add(nodeKey);
    setCollapsed(next);
  };

  return (
    <div
      data-row
      data-testid="load-row"
      data-file={load.file}
      data-when={load.when.kind}
      data-status={load.status.kind}
      data-via={load.via.kind}
      data-key={nodeKey}
      role="treeitem"
      aria-level={depth}
      aria-selected={selected}
      aria-expanded={hasChildren ? open : undefined}
      tabIndex={isTabStop ? 0 : -1}
      onFocus={() => setFocused(nodeKey)}
      onClick={() => go(pick(view, load))}
      onKeyDown={(event) => {
        if (event.key === "Enter" || event.key === " ") {
          event.preventDefault();
          go(pick(view, load));
        }
      }}
      title={`${say(t.sends, sends(load), d)} ${say(t.activation.detail, load.when, d)}`}
      className={`flex min-h-7 cursor-default items-center gap-2 border-l-2 py-0.5 pr-3 hover:bg-subtle ${selected ? "border-accent bg-accent-bg" : fires ? "border-warn bg-warn-bg" : "border-transparent"}`}
      style={{ paddingLeft: `${8 + (depth - 1) * 18}px` }}
    >
      <span className="flex w-4 shrink-0 justify-center text-faint">
        {hasChildren ? (
          <button type="button" tabIndex={-1} aria-label={open ? t.context.collapse : t.context.expand} onClick={(e) => { e.stopPropagation(); toggle(); }} className="rounded px-0.5 hover:bg-sunken">
            {open ? "▾" : "▸"}
          </button>
        ) : null}
      </span>
      <div className="flex min-w-0 flex-1 items-baseline gap-2">
        <Mono className={`max-w-[62%] shrink-0 truncate ${load.status.kind === "skipped" ? "text-muted" : ""} ${load.status.kind === "skipped" && load.status.skip.kind === "missing" ? "italic" : ""}`}>{label}</Mono>
        {ext && <Chip>{t.extKind[ext.extKind]} {load.kind === "listing" ? t.context.catalogLine : t.context.body}</Chip>}
        {host && (load.via.kind === "import" ? t.context.importedFrom(hostLabel, load.via.line) : load.via.kind === "reference" ? t.context.referencedFrom(hostLabel, load.via.line) : "") !== "" && (
          <span className="min-w-0 truncate text-[11px] text-faint">{load.via.kind === "import" ? t.context.importedFrom(hostLabel, load.via.line) : load.via.kind === "reference" ? t.context.referencedFrom(hostLabel, load.via.line) : ""}</span>
        )}
      </div>
      {unsaved && <Badge tone="warn">{t.write.unsaved}</Badge>}
      {fires && <Badge tone="warn">{t.context.firesOnPath}</Badge>}
      {ext && ext.status.kind !== "active" && ext.status.kind !== "shadowed" && <Badge tone={ext.status.kind === "conditional" ? "info" : "neutral"} title={say(t.extStatus, ext.status, d)}>{say(t.extStatusShort, ext.status, d)}</Badge>}
      {tone && (
        <Badge tone={tone} title={say(t.sends, sends(load), d)}>
          {load.status.kind === "skipped" ? say(t.skipShort, load.status.skip, d) : load.status.kind === "truncated" ? `${say(t.status.badge, load.status, d)} ${load.status.kept}/${load.status.total}` : ""}
        </Badge>
      )}
      <Badge tone={load.status.kind === "skipped" ? "neutral" : activationTone(load.when)} title={say(t.activation.detail, load.when, d)}>
        {say(t.activation.badge, load.when, d)}
      </Badge>
      <span className={`w-11 shrink-0 text-right tabular-nums ${load.delivered ? "" : "text-faint"}`}>{tokensValue === null ? "–" : fmt.tokens(tokensValue)}</span>
      {load.via.kind !== "reference" && <RowMenu path={load.file} />}
    </div>
  );
}

function OrderBlocks({ view, route, go }: { view: AgentView; route: Route; go: Go }) {
  const { t } = useI18n();
  const project = useProject();
  const render = useRender(project.cwd, view.agent, project.snapshot.fingerprint, true);
  const prefix = useMemo(() => buildPrefix(view), [view]);
  const chosen = picked(view, route);
  if (view.session.length === 0) return <p className="px-3 py-2 text-muted">{t.context.noLoads}</p>;
  if (render.isError) return <ErrorNote error={render.error} onRetry={() => void render.refetch()} />;
  return (
    <ol className="space-y-2 px-3 pt-2" aria-label={t.context.blocksFor}>
      {view.session.map((block, index) => {
        const pieces = render.data?.find((rendered) => rendered.label === block.label && rendered.delivery === block.delivery)?.pieces;
        return (
          <li key={block.label} className="rounded border border-line">
            <div className="flex flex-wrap items-center gap-2 border-b border-line bg-subtle px-2 py-1">
              <span className="grid size-5 place-items-center rounded-full bg-sunken text-[11px] font-semibold tabular-nums">{index + 1}</span>
              <b>{t.blockLabel[block.label]}</b>
              <Chip>{t.delivery[block.delivery]}</Chip>
              <Badge tone={block.fidelity === "exact" ? "ok" : "neutral"} title={t.fidelityHint[block.fidelity]}>{t.fidelity[block.fidelity]}</Badge>
              <span className="text-faint">{t.context.blockParts(block.parts.length)}</span>
              <span className="tabular-nums text-faint">{t.prefix.blockRange(exact(prefix.blocks[index]!.start), exact(prefix.blocks[index]!.end))}</span>
              <span className="ml-auto tabular-nums">{fmt.tokens(block.tokens)}</span>
            </div>
            <ul className="flex flex-wrap gap-1 border-b border-line px-2 py-1">
              {block.parts.flatMap((part, i) =>
                part.kind === "glue"
                  ? []
                  : [
                      <li key={i}>
                        <PartLink load={view.loads[part.load]} view={view} tokensValue={part.tokens} chosen={chosen} go={go} />
                      </li>,
                    ],
              )}
            </ul>
            {pieces ? (
              <LiteralText pieces={pieces} marked={(piece) => piece.kind === "load" && view.loads[piece.load] !== undefined && isPicked(chosen, view.loads[piece.load]!)} />
            ) : (
              <p className="px-2 py-1.5 text-muted">{render.isPending ? t.loading : t.context.renderFailed}</p>
            )}
          </li>
        );
      })}
    </ol>
  );
}

/** The text the agent receives, piece by piece, with the picked load's pieces marked. Short by default; one click shows all of it in a box that scrolls. */
function LiteralText({ pieces, marked }: { pieces: Piece[]; marked: (piece: Piece) => boolean }) {
  const { t } = useI18n();
  const [open, setOpen] = useState(false);
  const box = useRef<HTMLPreElement>(null);
  const text = pieces.map((piece) => piece.text).join("");
  const long = text.split("\n").length > 6 || text.length > 600;
  const marks = pieces.map(marked);
  const key = marks.join();
  // Scroll the box, not the page, to the first marked piece.
  useEffect(() => {
    const first = box.current?.querySelector<HTMLElement>("mark");
    if (box.current && first) box.current.scrollTop = Math.max(0, first.offsetTop - 8);
  }, [key, open]);
  return (
    <div>
      <pre ref={box} aria-label={t.context.literalText} data-testid="literal-text" className={`${open ? "max-h-[70vh]" : "max-h-32"} relative overflow-auto whitespace-pre-wrap break-words px-2 py-1.5 font-mono text-[12px] leading-5`}>
        {pieces.map((piece, i) =>
          marks[i] ? (
            <mark key={i} data-testid="literal-mark" className="rounded-sm bg-accent-bg text-fg outline outline-1 outline-accent">
              {piece.text}
            </mark>
          ) : (
            piece.text
          ),
        )}
      </pre>
      {long && (
        <button type="button" aria-expanded={open} onClick={() => setOpen(!open)} className="w-full border-t border-line px-2 py-1 text-left text-[12px] text-accent hover:bg-subtle">
          {open ? t.context.showLess : t.context.showAll}
        </button>
      )}
    </div>
  );
}

function PartLink({ load, view, tokensValue, chosen, go }: { load: Load | undefined; view: AgentView; tokensValue: number; chosen: Picked | null; go: Go }) {
  const project = useProject();
  if (!load) return null;
  return (
    <button type="button" onClick={() => go(pick(view, load))} aria-pressed={isPicked(chosen, load)} className="rounded border border-line px-1.5 py-px hover:bg-sunken aria-pressed:border-accent aria-pressed:bg-accent-bg">
      <Mono>{project.display(load.file)}</Mono> <span className="tabular-nums text-muted">{fmt.tokens(tokensValue)}</span>
    </button>
  );
}
