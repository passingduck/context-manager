// The throwaway world one run lives in: a home, a project, a data directory, and the host that serves them.
import { spawn } from "node:child_process";
import { mkdirSync, mkdtempSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import { createServer } from "node:net";
import { homedir, tmpdir } from "node:os";
import { join, sep } from "node:path";

/** Exit code when the run would reach outside its temp directory. */
export const EXIT_UNSAFE = 4;
/** Exit code when the host does not start. */
export const EXIT_NO_HOST = 5;

export const TOKEN = "cm-e2e-token";

const inside = (root, path) => path === root || path.startsWith(root + sep);

/** Throws unless every path is inside `root`, and none is the real home directory. */
export function refuseOutside(root, paths) {
  const realHome = realpathSync(homedir());
  for (const [name, path] of Object.entries(paths)) {
    const real = realpathSync(path);
    if (!inside(root, real)) throw new Error(`refusing to run: ${name} (${real}) is outside ${root}`);
    if (real === realHome) throw new Error(`refusing to run: ${name} is the real home`);
  }
}

const json = (value) => JSON.stringify(value, null, 2) + "\n";

/** Creates the temp directory and seeds the project, the home and the three agents' hook files. */
export function createSandbox() {
  const root = realpathSync(mkdtempSync(join(tmpdir(), "cm-e2e-")));
  const home = join(root, "home");
  const proj = join(root, "proj");
  const data = join(root, "data");
  for (const dir of [home, proj, data, join(root, "managed"), join(proj, ".git"), join(proj, "pkg"), join(home, ".claude"), join(home, ".codex"), join(home, ".gemini")]) {
    mkdirSync(dir, { recursive: true });
  }
  writeFileSync(join(proj, ".git", "HEAD"), "ref: refs/heads/main\n");
  writeFileSync(join(proj, "CLAUDE.md"), "# Project rules\n\nShared rules follow.\n\n@AGENTS.md\n\nPrefer small changes.\n");
  writeFileSync(join(proj, "AGENTS.md"), "# Shared rules\n\nOne writer for every file.\n");
  writeFileSync(join(proj, "pkg", "CLAUDE.md"), "# Package rules\n\nThe package has its own tests.\n");
  writeFileSync(join(home, ".claude", "settings.json"), json({ hooks: { PreToolUse: [{ matcher: "Bash", hooks: [{ type: "command", command: "echo claude-pre" }] }] } }));
  writeFileSync(join(home, ".codex", "hooks.json"), json({ hooks: { Stop: [{ hooks: [{ type: "command", command: "echo codex-stop" }] }] } }));
  writeFileSync(join(home, ".gemini", "settings.json"), json({ hooks: { AfterAgent: [{ hooks: [{ type: "command", command: "echo gemini-after" }] }] } }));
  // Gemini reads a project's context files and trusts its hooks only for a trusted folder (docs/agents/gemini-cli.md).
  // The environment below points the host at the second file, and the first is where Gemini keeps it by default.
  const trust = json({ [proj]: "TRUST_FOLDER" });
  writeFileSync(join(home, ".gemini", "trustedFolders.json"), trust);
  writeFileSync(join(root, "managed", "gemini-trusted.json"), trust);
  return { root, home, proj, data, cleanup: () => rmSync(root, { recursive: true, force: true }) };
}

async function freePort() {
  return new Promise((ok, bad) => {
    const server = createServer();
    server.once("error", bad);
    server.listen(0, "127.0.0.1", () => {
      const { port } = server.address();
      server.close(() => ok(port));
    });
  });
}

/**
 * Starts the host against the sandbox. The environment is built from nothing, so a variable in the
 * caller's shell cannot point the host at a real agent home, data directory or trash.
 */
export async function startHost(bin, box, { readOnly = false } = {}) {
  refuseOutside(box.root, { HOME: box.home, "data directory": box.data, project: box.proj });
  const env = {
    PATH: process.env.PATH ?? "/usr/bin:/bin",
    HOME: box.home,
    TMPDIR: box.root,
    LANG: "en_US.UTF-8",
    CONTEXT_MANAGER_DATA_DIR: box.data,
    CONTEXT_MANAGER_TOKEN: TOKEN,
    CONTEXT_MANAGER_MANAGED_DIR: join(box.root, "managed"),
    CONTEXT_MANAGER_NO_OS_TRASH: "1",
    GEMINI_CLI_SYSTEM_SETTINGS_PATH: join(box.root, "managed", "gemini-system.json"),
    GEMINI_CLI_SYSTEM_DEFAULTS_PATH: join(box.root, "managed", "gemini-defaults.json"),
    GEMINI_CLI_TRUSTED_FOLDERS_PATH: join(box.root, "managed", "gemini-trusted.json"),
  };
  const port = await freePort();
  const args = ["--serve", "--port", String(port), "--path", box.proj, ...(readOnly ? ["--read-only"] : [])];
  const child = spawn(bin, args, { env, cwd: box.root, stdio: ["ignore", "pipe", "pipe"] });
  let log = "";
  child.stdout.on("data", (b) => (log += b));
  child.stderr.on("data", (b) => (log += b));
  let exited = false;
  child.on("exit", () => (exited = true));
  const url = `http://127.0.0.1:${port}/`;
  for (let i = 0; i < 100; i++) {
    if (exited) throw new Error(`the host exited at start:\n${log}`);
    try {
      if ((await fetch(url)).ok) break;
    } catch {
      // Not listening yet.
    }
    await new Promise((r) => setTimeout(r, 100));
    if (i === 99) throw new Error(`the host did not answer on ${url}:\n${log}`);
  }
  return {
    url,
    /** The address a page opens: English, with the token and an optional route in the fragment. */
    page: (route = "", lang = "en") => `${url}?lang=${lang}#token=${TOKEN}${route ? `&${route}` : ""}`,
    /** One RPC the way the page sends it. Returns the status and the parsed body. */
    async rpc(op, args) {
      const response = await fetch(`${url}api/rpc`, {
        method: "POST",
        headers: { "content-type": "application/json", authorization: `Bearer ${TOKEN}` },
        body: JSON.stringify({ op, args }),
      });
      return { status: response.status, body: await response.json().catch(() => null) };
    },
    async stop() {
      if (exited) return;
      child.kill("SIGTERM");
      await new Promise((ok) => {
        child.once("exit", ok);
        setTimeout(() => {
          child.kill("SIGKILL");
          ok();
        }, 3000);
      });
    },
  };
}
