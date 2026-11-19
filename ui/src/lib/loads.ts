import type { Activation } from "../gen/Activation";
import type { AgentView } from "../gen/AgentView";
import type { BlockLabel } from "../gen/BlockLabel";
import type { Load } from "../gen/Load";
import type { LoadStatus } from "../gen/LoadStatus";
import type { Skip } from "../gen/Skip";
import type { Tone } from "../components/ui";

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

/** Colour of each session-start block label, shared by the header bar and the prefix map. */
export const LABEL_COLOR: Record<BlockLabel, string> = {
  instructions: "var(--c2)",
  memory: "var(--c4)",
  skillCatalog: "var(--c3)",
  agentCatalog: "var(--c1)",
  commandCatalog: "var(--c0)",
};
