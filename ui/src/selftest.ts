import { call } from "./api/client";
import { reportSelftest, transportName } from "./api/transport";

/** The desktop self-test: every read operation and a `watch` that has to wait, all over `invoke`. */
export async function runSelftest(): Promise<void> {
  try {
    const session = await call("session", null);
    const agents = await call("agents", null);
    const scan = await call("scan", { cwd: session.launchCwd });
    const cwd = session.launchCwd;
    const projects = await call("projects", null);
    const rendered = await Promise.all(
      agents.map((agent) => call("render", { cwd, agent: agent.id })),
    );
    const simulated = await call("simulate", { cwd, path: scan.cwd });
    const found = await call("search", { cwd, query: "a", regex: false, caseSensitive: false });
    const firstHook = agents
      .map((agent) => ({ agent, event: agent.hookSchema.events[0]?.name }))
      .find((entry) => entry.event !== undefined);
    const sample = firstHook
      ? await call("hook_sample", { cwd, agent: firstHook.agent.id, event: firstHook.event ?? "" })
      : null;
    const started = performance.now();
    const fingerprint = await call("watch", { cwd: session.launchCwd, since: scan.fingerprint });
    const waitedMs = Math.round(performance.now() - started);
    await reportSelftest({
      transport: transportName(),
      session,
      agents: agents.map((agent) => agent.displayName),
      scan: {
        cwd: scan.cwd,
        fingerprint: scan.fingerprint,
        agents: scan.agents.map((view) => ({
          agent: view.agent,
          presence: view.presence.kind,
          homes: view.homes.map((home) => home.path),
        })),
      },
      operations: {
        projects: projects.length,
        render: rendered.map((blocks) => blocks.length),
        simulate: simulated.map((entry) => ({ agent: entry.agent, fired: entry.fired.length })),
        search: { hits: found.hits.length, capped: found.capped },
        hookSample: sample !== null,
      },
      watch: { waitedMs, unchanged: fingerprint === scan.fingerprint },
    });
  } catch (error) {
    await reportSelftest({ transport: transportName(), error: JSON.stringify(error) });
  }
}
