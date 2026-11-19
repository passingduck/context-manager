import { call } from "./api/client";
import { reportSelftest, transportName } from "./api/transport";

/** The desktop self-test: `session`, `scan` and a `watch` that has to wait, all over `invoke`. */
export async function runSelftest(): Promise<void> {
  try {
    const session = await call("session", null);
    const agents = await call("agents", null);
    const scan = await call("scan", { cwd: session.launchCwd });
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
      watch: { waitedMs, unchanged: fingerprint === scan.fingerprint },
    });
  } catch (error) {
    await reportSelftest({ transport: transportName(), error: JSON.stringify(error) });
  }
}
