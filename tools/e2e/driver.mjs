// A small Chrome DevTools Protocol driver. Node 24 supplies `WebSocket` and `fetch`, so it needs no package.
import { spawn } from "node:child_process";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

/** Exit code when no Chrome or Chromium can be found. */
export const EXIT_NO_CHROME = 3;

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

const CANDIDATES = [
  "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
  "/Applications/Chromium.app/Contents/MacOS/Chromium",
  "/usr/bin/google-chrome",
  "/usr/bin/google-chrome-stable",
  "/usr/bin/chromium",
  "/usr/bin/chromium-browser",
  "/snap/bin/chromium",
];

/** `CM_E2E_CHROME`, then `CHROME_BIN`, then the usual install paths, then `PATH`. Null when there is none. */
export function findChrome(env = process.env) {
  for (const name of ["CM_E2E_CHROME", "CHROME_BIN"]) {
    if (env[name]) return existsSync(env[name]) ? env[name] : null;
  }
  for (const path of CANDIDATES) if (existsSync(path)) return path;
  for (const dir of (env.PATH ?? "").split(":")) {
    for (const name of ["google-chrome", "chromium", "chromium-browser", "chrome"]) {
      if (dir && existsSync(join(dir, name))) return join(dir, name);
    }
  }
  return null;
}

/** Starts headless Chrome with a throwaway profile and returns a page to drive. */
export async function launch(chromePath) {
  const profile = mkdtempSync(join(tmpdir(), "cm-e2e-chrome-"));
  const chrome = spawn(
    chromePath,
    ["--headless=new", "--disable-gpu", "--hide-scrollbars", "--remote-debugging-port=0", `--user-data-dir=${profile}`, "--window-size=1440,900", "--no-first-run", "--no-default-browser-check", "--disable-background-networking", "about:blank"],
    { stdio: "ignore" },
  );
  let exited = false;
  chrome.on("exit", () => (exited = true));

  const debugUrl = async () => {
    for (let i = 0; i < 100 && !exited; i++) {
      const file = join(profile, "DevToolsActivePort");
      if (existsSync(file)) {
        const port = readFileSync(file, "utf8").split("\n")[0];
        try {
          const list = await (await fetch(`http://127.0.0.1:${port}/json/list`)).json();
          const page = list.find((t) => t.type === "page");
          if (page) return page.webSocketDebuggerUrl;
        } catch {
          // Chrome is still starting.
        }
      }
      await sleep(100);
    }
    throw new Error("Chrome did not start");
  };

  const ws = new WebSocket(await debugUrl());
  await new Promise((ok, bad) => {
    ws.onopen = ok;
    ws.onerror = () => bad(new Error("cannot connect to Chrome"));
  });
  let nextId = 1;
  const pending = new Map();
  const consoleLines = [];
  ws.onmessage = (event) => {
    const msg = JSON.parse(event.data);
    if (msg.id && pending.has(msg.id)) {
      const { ok, bad } = pending.get(msg.id);
      pending.delete(msg.id);
      if (msg.error) bad(new Error(msg.error.message));
      else ok(msg.result);
    } else if (msg.method === "Page.javascriptDialogOpening") {
      // A page with unsaved text asks before it unloads. A run leaves pages in that state on purpose.
      void send("Page.handleJavaScriptDialog", { accept: true });
    } else if (msg.method === "Runtime.exceptionThrown") {
      consoleLines.push("EXCEPTION " + (msg.params.exceptionDetails.exception?.description ?? msg.params.exceptionDetails.text));
    }
  };
  const send = (method, params = {}) =>
    new Promise((ok, bad) => {
      const id = nextId++;
      const timer = setTimeout(() => {
        pending.delete(id);
        bad(new Error(`Chrome did not answer ${method} within 30 s`));
      }, 30_000);
      pending.set(id, { ok: (v) => (clearTimeout(timer), ok(v)), bad: (e) => (clearTimeout(timer), bad(e)) });
      ws.send(JSON.stringify({ id, method, params }));
    });
  await send("Page.enable");
  await send("Runtime.enable");
  await send("Emulation.setDeviceMetricsOverride", { width: 1440, height: 900, deviceScaleFactor: 1, mobile: false });

  const q = (selector) => JSON.stringify(selector);
  const page = {
    sleep,
    /** Uncaught page exceptions, for the summary. */
    exceptions: consoleLines,
    async goto(url) {
      await send("Page.navigate", { url });
      await sleep(400);
    },
    async eval(expression) {
      const r = await send("Runtime.evaluate", { expression, awaitPromise: true, returnByValue: true });
      if (r.exceptionDetails) throw new Error(r.exceptionDetails.exception?.description ?? r.exceptionDetails.text);
      return r.result.value;
    },
    /** Sets the URL fragment, which is the app's route. */
    async hash(fragment) {
      await page.eval(`location.hash = ${JSON.stringify(fragment)}`);
      await sleep(300);
    },
    /** Waits until the expression is truthy. The message names what was awaited. */
    async until(expression, what, ms = 10000) {
      const end = Date.now() + ms;
      while (Date.now() < end) {
        if (await page.eval(expression)) return;
        await sleep(100);
      }
      throw new Error(`timed out waiting for ${what}`);
    },
    /** Waits for an element that matches the selector. */
    waitFor: (selector, ms) => page.until(`!!document.querySelector(${q(selector)})`, `an element ${selector}`, ms),
    waitGone: (selector, ms) => page.until(`!document.querySelector(${q(selector)})`, `${selector} to go away`, ms),
    count: (selector) => page.eval(`document.querySelectorAll(${q(selector)}).length`),
    /** For every match: its data attributes, visible text and, for controls, whether it is disabled. */
    all: (selector) =>
      page.eval(`[...document.querySelectorAll(${q(selector)})].map((e) => ({ ...e.dataset, text: e.innerText, disabled: e.disabled === true, value: e.value ?? null, checked: e.checked ?? null }))`),
    /** The same, for the first match or null. */
    async one(selector) {
      return (await page.all(selector))[0] ?? null;
    },
    /** Waits for the element, then clicks it. `index` picks one of several matches. */
    async click(selector, index = 0) {
      await page.waitFor(selector);
      const ok = await page.eval(`(() => { const e = document.querySelectorAll(${q(selector)})[${index}]; if (!e || e.disabled) return false; e.scrollIntoView({ block: "center" }); e.click(); return true; })()`);
      if (!ok) throw new Error(`cannot click ${selector}[${index}]: missing or disabled`);
      await sleep(200);
    },
    /** Sets an input, select or textarea the way React expects. */
    async fill(selector, value) {
      await page.waitFor(selector);
      await page.eval(`(() => { const e = document.querySelector(${q(selector)});
        const proto = e.tagName === "TEXTAREA" ? HTMLTextAreaElement.prototype : e.tagName === "SELECT" ? HTMLSelectElement.prototype : HTMLInputElement.prototype;
        Object.getOwnPropertyDescriptor(proto, "value").set.call(e, ${JSON.stringify(value)});
        e.dispatchEvent(new Event("input", { bubbles: true })); e.dispatchEvent(new Event("change", { bubbles: true })); })()`);
      await sleep(150);
    },
    /** Types into an editor or field with a real key-event stream: focus it, caret to the end, then insert. */
    async type(selector, text) {
      await page.waitFor(selector);
      await page.eval(`(() => { const e = document.querySelector(${q(selector)}); e.focus();
        if (e.isContentEditable) { const r = document.createRange(); r.selectNodeContents(e); r.collapse(false); const s = getSelection(); s.removeAllRanges(); s.addRange(r); } })()`);
      await sleep(100);
      await send("Input.insertText", { text });
      await sleep(250);
    },
    /** Saves a PNG to `$CM_E2E_SHOTS/<name>.png` when that variable names a directory. Does nothing otherwise. */
    async screenshot(name) {
      const dir = process.env.CM_E2E_SHOTS;
      if (!dir) return;
      const { data } = await send("Page.captureScreenshot", { format: "png" });
      mkdirSync(dir, { recursive: true });
      writeFileSync(join(dir, `${name}.png`), Buffer.from(data, "base64"));
    },
    async close() {
      try {
        ws.close();
      } catch {
        // Already closed.
      }
      chrome.kill();
      await sleep(300);
      rmSync(profile, { recursive: true, force: true });
    },
  };
  return page;
}
