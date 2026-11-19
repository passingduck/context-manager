// Runs every step against a real host in a throwaway sandbox. Usage: node run.mjs <path to the headless binary>
// Exit codes: 0 all assertions held, 1 an assertion failed, 3 no Chrome, 4 unsafe sandbox, 5 the host did not start.
import { existsSync, readdirSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { EXIT_NO_CHROME, findChrome, launch } from "./driver.mjs";
import { createSandbox, EXIT_NO_HOST, EXIT_UNSAFE, refuseOutside, startHost } from "./sandbox.mjs";

const bin = process.argv[2] ? resolve(process.argv[2]) : "";
if (!bin || !existsSync(bin)) {
  console.error("usage: node tools/e2e/run.mjs <path to the headless context-manager binary>");
  process.exit(2);
}
const chrome = findChrome();
if (!chrome) {
  console.error("e2e: no Chrome or Chromium found. Set CM_E2E_CHROME (or CHROME_BIN) to the executable, or install Chrome.");
  process.exit(EXIT_NO_CHROME);
}

const STEP_MS = 90_000;
const here = dirname(fileURLToPath(import.meta.url));
// CM_E2E_ONLY=a,b runs only the steps whose file name contains one of the words, for debugging a flow.
const only = (process.env.CM_E2E_ONLY ?? "").split(",").filter(Boolean);
const stepFiles = readdirSync(join(here, "steps")).filter((f) => f.endsWith(".mjs") && (only.length === 0 || only.some((word) => f.includes(word)))).sort();

const box = createSandbox();
try {
  refuseOutside(box.root, { HOME: box.home, "data directory": box.data, project: box.proj });
} catch (error) {
  console.error(`e2e: ${error.message}`);
  box.cleanup();
  process.exit(EXIT_UNSAFE);
}
console.log(`e2e: sandbox ${box.root}\ne2e: chrome ${chrome}\ne2e: binary ${bin}`);

const results = [];
let step = "";
const check = (name, ok, detail = "") => {
  results.push({ step, name, ok: Boolean(ok), detail });
  console.log(`  ${ok ? "ok  " : "FAIL"} ${name}${ok || !detail ? "" : `\n         ${String(detail).replaceAll("\n", "\n         ")}`}`);
};

const ctx = { page: null, host: null, box, check, state: {}, startHost: (opts) => startHost(bin, box, opts) };
let code = 0;
try {
  ctx.host = await startHost(bin, box);
  ctx.page = await launch(chrome);
  for (const file of stepFiles) {
    const { default: def } = await import(pathToFileURL(join(here, "steps", file)).href);
    step = def.name;
    console.log(`\n${def.name}`);
    try {
      await Promise.race([def.run(ctx), new Promise((_, bad) => setTimeout(() => bad(new Error(`the step took longer than ${STEP_MS / 1000} s`)), STEP_MS))]);
    } catch (error) {
      check(`${def.name}: ran to the end`, false, error.stack?.split("\n").slice(0, 3).join("\n") ?? String(error));
    }
  }
} catch (error) {
  console.error(`e2e: ${error.message}`);
  code = error.message.includes("host") ? EXIT_NO_HOST : 1;
} finally {
  await ctx.page?.close();
  await ctx.host?.stop();
  box.cleanup();
}

const failed = results.filter((r) => !r.ok);
console.log(`\ne2e summary: ${results.length - failed.length} of ${results.length} assertions held`);
for (const r of failed) console.log(`  FAILED [${r.step}] ${r.name}`);
if (ctx.page?.exceptions?.length) console.log(`page exceptions:\n${ctx.page.exceptions.slice(-5).join("\n")}`);
process.exit(code || (failed.length > 0 || results.length === 0 ? 1 : 0));
