import type { AbsPath } from "../gen/AbsPath";
import type { Activation } from "../gen/Activation";
import type { AgentView } from "../gen/AgentView";
import type { BlockLabel } from "../gen/BlockLabel";
import type { Load } from "../gen/Load";
import type { LoadStatus } from "../gen/LoadStatus";
import type { Skip } from "../gen/Skip";
import type { Tone } from "../components/ui";
import type { Route } from "./route";

export type GroupKey = "sessionStart" | "onAccess" | "onInvoke" | "modelMayRead";
export const GROUPS: GroupKey[] = ["sessionStart", "onAccess", "onInvoke", "modelMayRead"];

export interface Node {
  load: Load;
  children: Node[];
}

/** Roots grouped by when they load. Imports and references nest under their host. */
export function buildGroups(view: AgentView): Record<GroupKey, Node[]> {
  const nodes = new Map<number, Node>(view.loads.map((load) => [load.id, { load, children: [] }]));
  const groups: Record<GroupKey, Node[]> = { sessionStart: [], onAccess: [], onInvoke: [], modelMayRead: [] };
  for (const load of view.loads) {
    const node = nodes.get(load.id)!;
    const host = load.via.kind === "import" || load.via.kind === "reference" ? nodes.get(load.via.host) : undefined;
    if (host) host.children.push(node);
    else groups[groupOf(load.when)].push(node);
  }
  return groups;
}

function groupOf(when: Activation): GroupKey {
  return when.kind === "withHost" ? "sessionStart" : when.kind;
}

export function activationTone(when: Activation): Tone {
  switch (when.kind) {
    case "sessionStart":
    case "withHost":
      return "ok";
    case "onAccess":
      return "warn";
    case "onInvoke":
      return "info";
    case "modelMayRead":
      return "neutral";
  }
}

const quietSkips: Skip["kind"][] = ["notRead", "alreadyLoaded", "empty", "featureOff", "excluded", "shadowed"];

export function statusTone(status: LoadStatus): Tone | null {
  switch (status.kind) {
    case "loaded":
      return null;
    case "truncated":
      return "warn";
    case "skipped":
      if (status.skip.kind === "missing" || status.skip.kind === "invalid") return "bad";
      return quietSkips.includes(status.skip.kind) ? "neutral" : "warn";
  }
}

/** The line after which the agent no longer receives the text, or null when nothing is cut. */
export function truncationLine(text: string, status: LoadStatus): number | null {
  if (status.kind !== "truncated") return null;
  const { kept, total } = status;
  const limitUnit = status.limit.unit;
  const count = (head: string) => head.split("\n").length - (head.endsWith("\n") ? 1 : 0);
  switch (limitUnit) {
    case "lines":
      return kept;
    case "chars":
      return count(text.slice(0, kept));
    case "bytes": {
      const head = new TextDecoder().decode(new TextEncoder().encode(text).slice(0, kept));
      return count(head);
    }
    default:
      return count(text.slice(0, Math.round((text.length * kept) / Math.max(total, 1))));
  }
}

/**
 * A name for one place a file loads that a rescan keeps: what loads, when, and how it got there,
 * down to the file and line of the load that brought it in. A `LoadId` is an index into one
 * snapshot, so it cannot go in the address.
 */
export function loadKey(view: AgentView, load: Load): string {
  const via = load.via;
  let how: string;
  if (via.kind === "discovery") how = via.rule;
  else if (via.kind === "listing") how = via.ext;
  else {
    const host = view.loads[via.host];
    how = `${via.kind}:${via.line}<${host ? `${host.file} ${loadKey(view, host)}` : ""}>`;
  }
  return `${load.kind} ${load.when.kind} ${how}`;
}

/** What the route selects among one agent's loads. `load` is null when the route names the file only, or a place this scan no longer has. */
export interface Picked {
  file: AbsPath;
  load: Load | null;
}

export function picked(view: AgentView, route: Route): Picked | null {
  if (!route.sel.startsWith("file:") || (route.ag !== "" && route.ag !== view.agent)) return null;
  const file = route.sel.slice("file:".length);
  const load = route.at ? (view.loads.find((l) => l.file === file && loadKey(view, l) === route.at) ?? null) : null;
  return { file, load };
}

/** With one place picked, only that load. With the file only, every load of the file. */
export const isPicked = (picked: Picked | null, load: Load): boolean =>
  picked !== null && (picked.load ? picked.load.id === load.id : picked.file === load.file);

/** The route patch that picks one place. */
export const pick = (view: AgentView, load: Load): Partial<Route> => ({ sel: `file:${load.file}`, ag: view.agent, at: loadKey(view, load), ln: 0 });

/** What the model receives from one load, as the facts of the load tell it. */
export type Sends =
  | { kind: "line" }
  | { kind: "body" }
  | { kind: "whole" }
  | { kind: "mayRead" }
  | ({ kind: "part" } & Omit<Extract<LoadStatus, { kind: "truncated" }>, "kind">)
  | { kind: "nothing" };

export function sends(load: Load): Sends {
  const status = load.status;
  if (status.kind === "skipped") return { kind: "nothing" };
  if (status.kind === "truncated") return { ...status, kind: "part" };
  if (load.when.kind === "modelMayRead") return { kind: "mayRead" };
  if (load.kind === "listing") return { kind: "line" };
  return load.delivered?.transformed ? { kind: "body" } : { kind: "whole" };
}

/** Colour of each session-start block label, shared by the header bar and the prefix map. */
export const LABEL_COLOR: Record<BlockLabel, string> = {
  instructions: "var(--c2)",
  memory: "var(--c4)",
  skillCatalog: "var(--c3)",
  agentCatalog: "var(--c1)",
  commandCatalog: "var(--c0)",
};
