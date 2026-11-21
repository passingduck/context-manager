import { call } from "./api/client";
import { reportSelftest, transportName } from "./api/transport";

/** The desktop self-test: every read operation and a `watch` that has to wait, all over `invoke`. The report holds counts and kinds, never text of a file or of a session. */
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
    const firstFile = Object.keys(scan.files)[0];
    const fileContent = firstFile ? await call("read_file", { cwd, path: firstFile }) : null;
    const loading = scan.agents.find((view) => view.loads.length > 0);
    const loadTexts = loading
      ? await call("load_texts", { cwd, agent: loading.agent, file: loading.loads[0]!.file, fingerprint: scan.fingerprint })
      : null;
    const changes = await call("history", { cwd: null, before: null, limit: 1 });
    const traces = await call("traces", { cwd });
    const first = traces[0];
    const key = first ? { agent: first.agent, id: first.id, part: null } : null;
    const trace = key ? await call("trace", { cwd, key }) : null;
    const step = trace ? trace.steps.findIndex((entry) => entry.sent !== null) : -1;
    const pieces = key && trace && step >= 0 ? await call("trace_step", { cwd, key, read: trace.read, step }) : null;
    const pieceText =
      key && trace && pieces?.kind === "current" && pieces.pieces.length > 0
        ? await call("trace_piece", { cwd, key, read: trace.read, step, piece: { kind: "sent", piece: 0 } })
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
        readFile: fileContent?.state.kind ?? null,
        loadTexts: loadTexts === null ? null : loadTexts.kind === "current" ? loadTexts.texts.length : loadTexts.kind,
        history: changes.length,
        traces: traces.length,
        trace: trace === null ? null : { steps: trace.steps.length, exact: trace.steps.filter((entry) => entry.sent?.exact).length },
        traceStep: pieces === null ? null : pieces.kind === "current" ? pieces.pieces.length : pieces.kind,
        tracePiece: pieceText === null ? null : pieceText.kind,
      },
      watch: { waitedMs, unchanged: fingerprint === scan.fingerprint },
    });
  } catch (error) {
    await reportSelftest({ transport: transportName(), error: JSON.stringify(error) });
  }
}
