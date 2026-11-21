import { useEffect, useMemo } from "react";
import { useAgents, useScan, useSession, useWatch } from "./api/queries";
import { ProjectProvider } from "./app/project";
import { Rail } from "./app/Rail";
import { useTheme } from "./app/theme";
import { TopBar } from "./app/TopBar";
import { ErrorNote, Loading } from "./components/ui";
import { useI18n } from "./i18n";
import type { Snapshot } from "./gen/Snapshot";
import { useRoute, type Route } from "./lib/route";
import { runSelftest } from "./selftest";
import { ContextView } from "./views/ContextView";
import { Detail } from "./views/Detail";
import { CallDetail } from "./views/CallDetail";
import { DoctorView } from "./views/DoctorView";
import { HistoryView } from "./views/HistoryView";
import { HooksView } from "./views/HooksView";
import { MemoryView } from "./views/MemoryView";
import { SearchPanel } from "./views/SearchPanel";
import { SessionsView } from "./views/SessionsView";
import { dirtyPaths } from "./write/buffers";
import { WriteProvider } from "./write/flow";

/** With nothing selected, each view shows its first item so the detail pane is never blank. */
function defaultSelection(view: Route["view"], snapshot: Snapshot): Partial<Route> {
  switch (view) {
    case "context": {
      for (const agent of snapshot.agents) {
        const load = agent.loads.find((l) => l.when.kind === "sessionStart");
        if (load) return { sel: `file:${load.file}`, ag: agent.agent };
      }
      return {};
    }
    case "memory": {
      const store = snapshot.agents.flatMap((agent) => agent.memory.map((m) => ({ agent: agent.agent, store: m }))).find(({ store: m }) => m.index ?? m.entries[0]);
      const file = store?.store.index ?? store?.store.entries[0]?.file;
      return file && store ? { sel: `file:${file}`, ag: store.agent } : {};
    }
    case "hooks": {
      for (const agent of snapshot.agents) if (agent.hooks[0]) return { sel: `hook:${agent.hooks[0].id}`, ag: agent.agent };
      return {};
    }
    case "doctor": {
      for (const diagnostic of snapshot.diagnostics) {
        if (diagnostic.at.kind === "file") return { sel: `file:${diagnostic.at.path}`, ag: diagnostic.agent ?? "", ln: diagnostic.at.line ?? 0 };
        if (diagnostic.at.kind === "hook") return { sel: `hook:${diagnostic.at.hook}`, ag: diagnostic.agent ?? "" };
      }
      return {};
    }
    case "history":
    case "sessions":
      return {};
  }
}

if (new URLSearchParams(window.location.search).has("selftest")) void runSelftest();

export function App() {
  const { t } = useI18n();
  const [route, go] = useRoute();
  const [theme, toggleTheme] = useTheme();
  const session = useSession();
  const agents = useAgents();
  const cwd = route.p || session.data?.launchCwd || null;
  const scan = useScan(cwd);
  useWatch(cwd, scan.data?.fingerprint);

  const value = useMemo(
    () =>
      session.data && agents.data && scan.data && cwd
        ? { cwd, home: session.data.home, snapshot: scan.data, infos: agents.data, coreRules: session.data.coreRules, readOnly: session.data.readOnly }
        : null,
    [session.data, agents.data, scan.data, cwd],
  );

  // With nothing selected, the context view shows its first file so the detail pane is never blank.
  const shown = value && !route.sel ? { ...route, ...defaultSelection(route.view, value.snapshot) } : route;

  // Leaving the page with unsaved text asks first.
  useEffect(() => {
    const guard = (event: BeforeUnloadEvent) => {
      if (dirtyPaths().length === 0) return;
      event.preventDefault();
      event.returnValue = "";
    };
    window.addEventListener("beforeunload", guard);
    return () => window.removeEventListener("beforeunload", guard);
  }, []);

  const failure = session.error ?? agents.error ?? scan.error;
  return (
    <div className="grid h-full grid-rows-[auto_1fr]">
      <TopBar cwd={cwd ?? ""} home={session.data?.home ?? ""} route={route} go={go} theme={theme} toggleTheme={toggleTheme} />
      {failure ? (
        <ErrorNote error={failure} />
      ) : !value ? (
        <Loading />
      ) : (
        <ProjectProvider value={value}>
          <WriteProvider>
          <div className="flex min-h-0 flex-col">
          {value.readOnly && <p role="status" className="border-b border-warn bg-warn-bg px-3 py-1 text-warn">{t.write.readOnlyHost}</p>}
          <div className="grid min-h-0 flex-1 grid-cols-[13rem_minmax(0,5fr)_minmax(0,6fr)]">
            <Rail route={route} go={go} snapshot={value.snapshot} pending={session.data?.pending.length ?? 0} />
            <main className="flex min-h-0 min-w-0 flex-col border-r border-line" aria-label={t.title}>
              {route.q.trim() ? (
                <SearchPanel route={route} go={go} />
              ) : route.view === "context" ? (
                <ContextView route={shown} go={go} />
              ) : route.view === "memory" ? (
                <MemoryView route={route} go={go} />
              ) : route.view === "hooks" ? (
                <HooksView route={route} go={go} />
              ) : route.view === "doctor" ? (
                <DoctorView route={route} go={go} />
              ) : route.view === "sessions" ? (
                <SessionsView route={route} go={go} />
              ) : (
                <HistoryView route={route} go={go} />
              )}
            </main>
            <aside className="min-h-0 min-w-0 bg-bg" aria-label={t.detail.why}>
              {route.view === "sessions" ? <CallDetail route={route} go={go} /> : <Detail route={shown} go={go} />}
            </aside>
          </div>
          </div>
          </WriteProvider>
        </ProjectProvider>
      )}
    </div>
  );
}
