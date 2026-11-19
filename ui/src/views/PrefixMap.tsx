import { useRef, useState, type CSSProperties, type KeyboardEvent } from "react";
import { useProject } from "../app/project";
import { Mono } from "../components/ui";
import type { AgentView } from "../gen/AgentView";
import type { Load } from "../gen/Load";
import { useI18n } from "../i18n";
import * as fmt from "../lib/format";
import { LABEL_COLOR } from "../lib/loads";
import { buildPrefix, buildRows, exact, stubsBefore, ticks, unsizedAfter, type Prefix, type Seg } from "../lib/prefix";
import type { Route } from "../lib/route";

type Go = (patch: Partial<Route>) => void;

const CAPTION_H = 34;
const STRIP_H = 34;
const STUB_W = 88;

const pct = (n: number, total: number) => `${(n / total) * 100}%`;

/** Token positions as CSS lengths. Each stub takes a fixed width, and the tokens share the rest of the bar. */
function scale(prefix: Prefix) {
  const room = `(100% - ${prefix.stubs.length * STUB_W}px)`;
  return {
    /** The left edge of a token position. A stub at that position lies before it on side "after", after it on side "before". */
    at: (tokens: number, side: "before" | "after") => `calc(${room} * ${tokens / prefix.total} + ${stubsBefore(prefix, tokens, side) * STUB_W}px)`,
    width: (tokens: number) => `calc(${room} * ${tokens / prefix.total})`,
  };
}
const hatch = "repeating-linear-gradient(135deg, color-mix(in srgb, var(--bg) 38%, transparent) 0 2px, transparent 2px 7px)";

/** Name of a file as the tree shows it: the extension name for a catalog line, else the display path. */
function useLoadName(view: AgentView) {
  const project = useProject();
  const { d } = useI18n();
  return (load: Load) => {
    const ext = load.via.kind === "listing" ? view.extensions.find((e) => load.via.kind === "listing" && e.id === load.via.ext) : undefined;
    return ext ? ext.name : (project.snapshot.files[load.file]?.display ?? d(load.file));
  };
}

export function PrefixMap({ view, route, go, intro }: { view: AgentView; route: Route; go: Go; intro: boolean }) {
  const { t } = useI18n();
  const project = useProject();
  const prefix = buildPrefix(view);
  if (prefix.total === 0) {
    return (
      <div className="mx-3 mb-1 mt-2 rounded border border-dashed border-line px-3 py-2 text-muted" role="group" aria-label={t.prefix.groupLabel(project.name(view.agent))}>
        <b className="text-fg">{t.prefix.title}</b>
        <p>{t.prefix.empty}</p>
      </div>
    );
  }
  return <Strip view={view} prefix={prefix} route={route} go={go} intro={intro} />;
}

function Strip({ view, prefix, route, go, intro }: { view: AgentView; prefix: Prefix; route: Route; go: Go; intro: boolean }) {
  const { t } = useI18n();
  const project = useProject();
  const name = useLoadName(view);
  const [hover, setHover] = useState<number | null>(null);
  const [focus, setFocus] = useState<number | null>(null);
  const [rowsOpen, setRowsOpen] = useState(false);
  const stripRef = useRef<HTMLDivElement>(null);
  const { segs, total } = prefix;
  const place = scale(prefix);
  const files = segs.filter((seg) => seg.load);
  const selectedFile = route.sel.startsWith("file:") ? route.sel.slice(5) : "";
  const isSelected = (seg: Seg) => seg.load !== null && seg.load.file === selectedFile && (route.ag === "" || route.ag === view.agent);
  const active = hover ?? focus;
  const activeSeg = active === null ? null : segs[active];
  const tabStop = focus ?? files.find(isSelected)?.index ?? files[0]?.index;
  const select = (seg: Seg) => seg.load && go({ sel: `file:${seg.load.file}`, ag: view.agent, ln: 0 });

  function onKeyDown(event: KeyboardEvent<HTMLDivElement>) {
    event.stopPropagation();
    const buttons = Array.from(stripRef.current?.querySelectorAll<HTMLElement>("[data-seg]") ?? []);
    const at = buttons.findIndex((b) => b === document.activeElement);
    const to = event.key === "ArrowRight" ? at + 1 : event.key === "ArrowLeft" ? at - 1 : event.key === "Home" ? 0 : event.key === "End" ? buttons.length - 1 : null;
    if (to === null) return;
    event.preventDefault();
    buttons[Math.max(0, Math.min(buttons.length - 1, to))]?.focus();
  }

  const tooltip = activeSeg?.load ? (
    <Tooltip seg={activeSeg} view={view} total={total} prefix={prefix} />
  ) : null;

  return (
    <div data-testid="prefix" className="px-3 pb-1 pt-2" role="group" aria-label={t.prefix.groupLabel(project.name(view.agent))}>
      <p className="mb-1.5 max-w-3xl text-[12px] text-muted">
        <b className="text-fg">{t.prefix.title}.</b> {intro && t.prefix.intro}
      </p>
      <div className="relative">
        <div className="relative" style={{ height: CAPTION_H + STRIP_H }}>
          {prefix.stubs.map((stub, order) => (
            <div key={`${stub.block}-${stub.kind}`} data-stub={stub.kind} className="absolute top-0" style={{ left: `calc(${place.at(stub.at, "before")} + ${Math.max(0, order - stubsBefore(prefix, stub.at, "before")) * STUB_W}px)`, width: STUB_W, height: CAPTION_H + STRIP_H }} title={t.prefix.stub[stub.kind].title}>
              <p className="break-keep pr-1 text-[10px] leading-3 text-faint" style={{ height: CAPTION_H }}>{t.prefix.stub[stub.kind].caption}</p>
              <div aria-hidden className="absolute inset-x-0 bottom-0 border-y-2 border-dashed border-faint bg-sunken" style={{ height: STRIP_H, clipPath: "polygon(0 0, 100% 0, 92% 25%, 100% 50%, 92% 75%, 100% 100%, 0 100%)" }} />
            </div>
          ))}
          {prefix.blocks.map((pb) => (
            <div
              key={pb.index}
              className="pointer-events-none absolute inset-y-0 min-w-0 overflow-hidden border-l-2 border-fg-faint pl-1"
              style={{ left: place.at(pb.start, "after"), width: place.width(pb.end - pb.start), height: CAPTION_H }}
              title={`${t.blockLabel[pb.block.label]}, ${t.delivery[pb.block.delivery]}, ${t.fidelity[pb.block.fidelity]}, ${t.prefix.blockRange(exact(pb.start), exact(pb.end))}`}
            >
              <p className="truncate text-[11px] font-semibold leading-4">{t.blockLabel[pb.block.label]}</p>
              <p className="truncate text-[10px] leading-3 text-muted">
                {t.delivery[pb.block.delivery]}, {t.fidelity[pb.block.fidelity]}
              </p>
            </div>
          ))}
          <div
            ref={stripRef}
            role="group"
            aria-label={t.prefix.title}
            onKeyDown={onKeyDown}
            className="absolute inset-x-0 bg-sunken"
            style={{ top: CAPTION_H, height: STRIP_H }}
          >
            {segs.map((seg) => {
              const load = seg.load;
              const block = prefix.blocks[seg.block]!.block;
              const color = load ? LABEL_COLOR[block.label] : "var(--fg-faint)";
              const alt = load ? files.filter((f) => f.block === seg.block && f.index < seg.index).length % 2 === 1 : false;
              const style: CSSProperties = {
                left: place.at(seg.start, "after"),
                width: place.width(seg.tokens),
                minWidth: load ? 6 : 2,
              };
              const fill = load ? `color-mix(in srgb, ${color} ${alt ? 68 : 100}%, var(--bg))` : "var(--line)";
              const inner: CSSProperties = {
                background: block.fidelity === "approximate" && load ? `${hatch}, ${fill}` : fill,
                height: load ? "100%" : "40%",
                marginRight: 2,
              };
              if (!load) {
                return (
                  <span key={seg.index} aria-hidden data-testid="prefix-glue" data-start={seg.start} data-end={seg.end} title={t.prefix.glueTitle(seg.tokens)} className="absolute inset-y-0 flex items-center" style={style}>
                    <span className="block w-full" style={inner} />
                  </span>
                );
              }
              const selected = isSelected(seg);
              return (
                <button
                  key={seg.index}
                  type="button"
                  data-seg
                  data-testid="prefix-seg"
                  data-file={load.file}
                  data-start={seg.start}
                  data-end={seg.end}
                  aria-pressed={selected}
                  aria-label={t.prefix.segLabel(name(load), exact(seg.tokens), exact(seg.start), exact(seg.end))}
                  tabIndex={tabStop === seg.index ? 0 : -1}
                  onClick={() => select(seg)}
                  onPointerEnter={() => setHover(seg.index)}
                  onPointerLeave={() => setHover(null)}
                  onFocus={() => setFocus(seg.index)}
                  onBlur={() => setFocus(null)}
                  className="absolute inset-y-0 block cursor-default p-0"
                  style={{ ...style, zIndex: selected ? 2 : 1 }}
                >
                  <span className="block w-full" style={{ ...inner, boxShadow: selected ? "0 0 0 2px var(--bg), 0 0 0 4px var(--fg)" : undefined }} />
                </button>
              );
            })}
            {activeSeg && activeSeg.end < total && (
              <>
                <div aria-hidden data-tail className="pointer-events-none absolute inset-y-0 z-[3]" style={{ left: place.at(activeSeg.end, "before"), right: 0, background: `${hatch}, color-mix(in srgb, var(--bg) 35%, transparent)`, backdropFilter: "grayscale(1)" }} />
                <div aria-hidden className="pointer-events-none absolute z-[3] h-1.5 border-b-2 border-l-2 border-r-2 border-warn" style={{ left: place.at(activeSeg.end, "before"), right: 0, bottom: -8 }} />
              </>
            )}
          </div>
        </div>

        <div className="relative h-6">
          <Axis total={total} at={(v) => place.at(v, v === 0 ? "after" : "before")} />
        </div>

        {tooltip && (
          <div
            role="tooltip"
            className="pointer-events-none absolute z-10 w-[22rem] rounded border border-line bg-bg p-2 text-[12px] shadow-lg"
            style={{ top: "100%", left: `clamp(0px, calc(${place.at((activeSeg!.start + activeSeg!.end) / 2, "after")} - 11rem), calc(100% - 22rem))` }}
          >
            {tooltip}
          </div>
        )}
      </div>

      <ul className="mt-1 flex flex-wrap gap-x-4 gap-y-0.5 text-[11px] text-muted" aria-label={t.prefix.title}>
        <li className="flex items-center gap-1">
          <span className="inline-block h-2.5 w-5" style={{ background: "var(--fg-faint)" }} aria-hidden />
          {t.prefix.legendExact}
        </li>
        <li className="flex items-center gap-1">
          <span className="inline-block h-2.5 w-5" style={{ background: `${hatch}, var(--fg-faint)` }} aria-hidden />
          {t.prefix.legendApprox}
        </li>
        <li className="flex items-center gap-1">
          <span className="inline-block h-1 w-5" style={{ background: "var(--line)" }} aria-hidden />
          {t.prefix.legendGlue}
        </li>
        <li className="flex items-center gap-1">
          <span className="inline-block h-2.5 w-4 border-y-2 border-dashed border-faint bg-sunken" aria-hidden />
          {t.prefix.legendStub}
        </li>
        <li className="flex items-center gap-1">
          <span className="inline-block h-2.5 w-5 bg-fg-faint" style={{ boxShadow: "0 0 0 1px var(--bg), 0 0 0 2px var(--fg)" }} aria-hidden />
          {t.prefix.legendSelected}
        </li>
      </ul>

      <button type="button" aria-expanded={rowsOpen} onClick={() => setRowsOpen(!rowsOpen)} className="mt-1.5 text-[12px] text-accent hover:underline">
        {rowsOpen ? "▾ " : "▸ "}
        {rowsOpen ? t.prefix.rowsHide : t.prefix.rowsShow}
      </button>
      {rowsOpen && <Rows view={view} prefix={prefix} route={route} go={go} name={name} isSelected={isSelected} select={select} />}
    </div>
  );
}

function Tooltip({ seg, view, total, prefix }: { seg: Seg; view: AgentView; total: number; prefix: Prefix }) {
  const { t } = useI18n();
  const project = useProject();
  const name = useLoadName(view);
  const load = seg.load!;
  const block = prefix.blocks[seg.block]!.block;
  const after = total - seg.end;
  const unsized = unsizedAfter(prefix, seg.end);
  return (
    <>
      <p className="mb-1 break-all font-mono font-semibold">{load.kind === "listing" ? `${name(load)} (${project.display(load.file)})` : project.display(load.file)}</p>
      <dl className="grid grid-cols-[5.5rem_1fr] gap-x-2 gap-y-0.5">
        <dt className="text-muted">{t.prefix.kind}</dt>
        <dd>{t.docKind[load.kind]}</dd>
        <dt className="text-muted">{t.prefix.tokens}</dt>
        <dd className="tabular-nums">{exact(seg.tokens)}</dd>
        <dt className="text-muted">{t.prefix.position}</dt>
        <dd className="tabular-nums">{t.prefix.positionText(exact(seg.start), exact(seg.end), exact(total))}</dd>
        <dt className="text-muted">{t.prefix.lands}</dt>
        <dd>
          {t.blockLabel[block.label]}, {t.delivery[block.delivery]}, {t.fidelity[block.fidelity]}
        </dd>
        <dt className="text-muted">{t.prefix.after}</dt>
        <dd className="tabular-nums">{unsized.length > 0 ? t.prefix.afterAtLeast(exact(after)) : t.prefix.afterText(exact(after))}</dd>
      </dl>
      <p className="mt-1.5 border-t border-line pt-1.5 text-muted">
        {unsized.length > 0 ? t.prefix.tailAtLeast(exact(after), unsized.map((kind) => t.prefix.unsizedName[kind]).join(", ")) : after > 0 ? t.prefix.tail(exact(after)) : t.prefix.tailNone}
      </p>
    </>
  );
}

function Axis({ total, narrow = false, at }: { total: number; narrow?: boolean; at?: (tokens: number) => string }) {
  const marks = ticks(total).filter((v) => v === 0 || v / total < (narrow ? 0.8 : 0.93));
  return (
    <div aria-hidden className="relative h-full text-[10px] tabular-nums text-faint">
      <div className="absolute inset-x-0 top-0 h-px bg-line" />
      {marks.map((v) => (
        <span key={v} className="absolute top-0" style={{ left: at ? at(v) : pct(v, total) }}>
          <span className="block h-1.5 w-px bg-fg-faint" />
          <span className="block" style={{ transform: v === 0 ? undefined : "translateX(-50%)" }}>{fmt.tokens(v)}</span>
        </span>
      ))}
      <span className="absolute right-0 top-0">
        <span className="ml-auto block h-1.5 w-px bg-fg-faint" />
        <span className="block text-right">{exact(total)}</span>
      </span>
    </div>
  );
}

function Rows(props: {
  view: AgentView;
  prefix: Prefix;
  route: Route;
  go: Go;
  name: (load: Load) => string;
  isSelected: (seg: Seg) => boolean;
  select: (seg: Seg) => void;
}) {
  const { prefix, name, isSelected, select } = props;
  const { t } = useI18n();
  const project = useProject();
  const { total } = prefix;
  const rows = buildRows(prefix);
  return (
    <div className="mt-1" role="group" aria-label={t.prefix.rowsLabel}>
      <div className="grid grid-cols-[minmax(0,14rem)_1fr_3.5rem] items-end gap-x-2 border-l-2 border-transparent px-1">
        <span />
        <div className="relative h-6">
          <Axis total={total} narrow />
        </div>
        <span />
      </div>
      <ul>
        {rows.map((row) => {
          const first = row.segs[0]!;
          const block = prefix.blocks[row.block]!.block;
          const bundle = row.segs.length > 1;
          const label = bundle ? t.context.bundle.listing(row.segs.length) : first.load!.kind === "listing" ? name(first.load!) : project.display(first.load!.file);
          const selected = row.segs.some(isSelected);
          const body = (
            <>
              <span className={`min-w-0 truncate text-left ${bundle ? "font-medium" : ""}`}>{bundle ? label : <Mono>{label}</Mono>}</span>
              <span className="relative h-3.5 rounded-sm bg-sunken">
                <span
                  className="absolute inset-y-0 rounded-sm"
                  style={{
                    left: pct(row.start, total),
                    width: pct(row.end - row.start, total),
                    minWidth: 4,
                    background: block.fidelity === "approximate" ? `${hatch}, ${LABEL_COLOR[block.label]}` : LABEL_COLOR[block.label],
                  }}
                />
              </span>
              <span className="text-right tabular-nums text-muted">{fmt.tokens(row.end - row.start)}</span>
            </>
          );
          const cls = `grid w-full grid-cols-[minmax(0,14rem)_1fr_3.5rem] items-center gap-x-2 border-l-2 px-1 py-0.5 ${selected ? "border-accent bg-accent-bg" : "border-transparent"}`;
          return (
            <li key={row.key} title={`${label}: ${t.prefix.positionText(exact(row.start), exact(row.end), exact(total))}`}>
              {bundle ? (
                <div className={cls}>{body}</div>
              ) : (
                <button type="button" aria-pressed={selected} onClick={() => select(first)} className={`${cls} cursor-default hover:bg-subtle`}>
                  {body}
                </button>
              )}
            </li>
          );
        })}
      </ul>
    </div>
  );
}
