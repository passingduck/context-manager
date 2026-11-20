import { createContext, useContext, useEffect, useMemo, type ReactNode } from "react";
import type { AbsPath } from "../gen/AbsPath";
import type { AgentId } from "../gen/AgentId";
import type { AgentInfo } from "../gen/AgentInfo";
import type { AgentView } from "../gen/AgentView";
import type { Load } from "../gen/Load";
import type { RuleSpec } from "../gen/RuleSpec";
import type { Snapshot } from "../gen/Snapshot";
import { useI18n } from "../i18n";

/** What every view needs about the open project, resolved once. */
export interface Project {
  cwd: AbsPath;
  home: AbsPath;
  snapshot: Snapshot;
  infos: AgentInfo[];
  /** Rules that belong to no agent. */
  coreRules: RuleSpec[];
  readOnly: boolean;
  name: (agent: AgentId) => string;
  /** CSS variable for the agent's accent, by registry position. */
  color: (agent: AgentId) => string;
  /** Explanation of a rule reference in the current language, or the reference itself when unknown. */
  rule: (id: string) => string;
  view: (agent: AgentId) => AgentView | undefined;
  load: (agent: AgentId, id: number) => Load | undefined;
  /** The display path of a file, with the home and project directories shortened. */
  display: (path: AbsPath) => string;
}

const Ctx = createContext<Project | null>(null);

export function ProjectProvider({ value, children }: { value: Omit<Project, "name" | "color" | "rule" | "view" | "load" | "display">; children: ReactNode }) {
  const { lang, d, setRoots } = useI18n();
  useEffect(() => setRoots(value.cwd, value.home), [setRoots, value.cwd, value.home]);
  const project = useMemo<Project>(() => {
    const rules = new Map([...value.coreRules, ...value.infos.flatMap((info) => info.rules)].map((r) => [r.id, r] as const));
    const order = value.snapshot.agents.map((view) => view.agent);
    return {
      ...value,
      name: (agent) => value.infos.find((info) => info.id === agent)?.displayName ?? agent,
      color: (agent) => `var(--c${Math.max(0, order.indexOf(agent)) % 5})`,
      rule: (id) => {
        const spec = rules.get(id);
        return spec ? spec[lang] : id;
      },
      view: (agent) => value.snapshot.agents.find((view) => view.agent === agent),
      load: (agent, id) => value.snapshot.agents.find((view) => view.agent === agent)?.loads[id],
      display: (path) => value.snapshot.files[path]?.display ?? d(path),
    };
  }, [value, lang, d]);
  return <Ctx.Provider value={project}>{children}</Ctx.Provider>;
}

export function useProject(): Project {
  const project = useContext(Ctx);
  if (!project) throw new Error("ProjectProvider is missing");
  return project;
}
