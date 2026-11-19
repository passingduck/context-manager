import { useEffect, useState } from "react";
import { call } from "./api/client";
import type { AgentInfo } from "./gen/AgentInfo";
import type { Session } from "./gen/Session";
import type { Snapshot } from "./gen/Snapshot";
import { catalogs, detectLang, rememberLang, type Lang } from "./i18n";
import { runSelftest } from "./selftest";

interface Loaded {
  session: Session;
  agents: AgentInfo[];
  snapshot: Snapshot;
}

async function load(): Promise<Loaded> {
  const session = await call("session", null);
  const [agents, snapshot] = await Promise.all([call("agents", null), call("scan", { cwd: session.launchCwd })]);
  return { session, agents, snapshot };
}

export function App() {
  const [lang, setLang] = useState<Lang>(detectLang);
  const [state, setState] = useState<{ loaded: Loaded } | { error: unknown } | null>(null);
  const t = catalogs[lang];

  useEffect(() => {
    load().then(
      (loaded) => setState({ loaded }),
      (error: unknown) => setState({ error }),
    );
    if (new URLSearchParams(window.location.search).has("selftest")) void runSelftest();
  }, []);

  function choose(next: Lang) {
    setLang(next);
    rememberLang(next);
    document.documentElement.lang = next;
  }

  return (
    <main className="mx-auto max-w-3xl p-6 font-sans">
      <header className="mb-6 flex items-center justify-between">
        <h1 className="text-xl font-semibold">{t.title}</h1>
        <div role="group" aria-label={t.language} className="flex gap-1 text-sm">
          {(["ko", "en"] as const).map((code) => (
            <button
              key={code}
              type="button"
              aria-pressed={lang === code}
              onClick={() => choose(code)}
              className="rounded border px-2 py-1 aria-pressed:bg-neutral-900 aria-pressed:text-white"
            >
              {code === "ko" ? "한국어" : "EN"}
            </button>
          ))}
        </div>
      </header>
      {state === null ? (
        <p>{t.loading}</p>
      ) : "error" in state ? (
        <p role="alert">
          {t.failed}: {JSON.stringify(state.error)}
        </p>
      ) : (
        <Overview t={t} loaded={state.loaded} />
      )}
    </main>
  );
}

function Overview({ t, loaded }: { t: (typeof catalogs)[Lang]; loaded: Loaded }) {
  const { session, agents, snapshot } = loaded;
  const rows: [string, string][] = [
    [t.session.version, session.version],
    [t.session.shell, t.shell[session.shell]],
    [t.session.readOnly, session.readOnly ? t.session.yes : t.session.no],
    [t.session.launchCwd, session.launchCwd],
    [t.session.home, session.home],
    [t.session.os, t.os[session.os]],
  ];
  return (
    <>
      <section className="mb-8">
        <h2 className="mb-2 text-lg font-medium">{t.session.heading}</h2>
        <dl className="grid grid-cols-[10rem_1fr] gap-y-1 text-sm">
          {rows.map(([label, value]) => (
            <div key={label} className="contents">
              <dt className="text-neutral-500">{label}</dt>
              <dd className="break-all">{value}</dd>
            </div>
          ))}
        </dl>
      </section>
      <section>
        <h2 className="mb-2 text-lg font-medium">{t.agents.heading}</h2>
        <ul className="divide-y rounded border">
          {snapshot.agents.map((view) => {
            const info = agents.find((agent) => agent.id === view.agent);
            const home = view.homes[0];
            return (
              <li key={view.agent} className="p-3 text-sm">
                <div className="flex justify-between">
                  <span className="font-medium">{info?.displayName ?? view.agent}</span>
                  <span>{t.presence[view.presence.kind]}</span>
                </div>
                {home && (
                  <div className="break-all text-neutral-500">
                    {t.agents.home}: {home.path}
                    {home.exists ? "" : ` (${t.agents.missing})`}
                  </div>
                )}
              </li>
            );
          })}
        </ul>
      </section>
    </>
  );
}
