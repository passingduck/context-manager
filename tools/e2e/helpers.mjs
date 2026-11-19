import { existsSync, readdirSync, readFileSync } from "node:fs";

export const read = (path) => (existsSync(path) ? readFileSync(path, "utf8") : null);
export const list = (dir) => (existsSync(dir) ? readdirSync(dir) : []);

/** Opens the app at a route on a fresh page, and waits until the selector shows. */
export async function open(t, route, ready) {
  await t.page.goto("about:blank");
  await t.page.goto(t.host.page(route));
  await t.page.waitFor(ready, 20000);
}

/** A route that selects a file, or a hook, in the detail pane. */
export const routeTo = (view, sel, agent = "") => new URLSearchParams({ ...(view === "context" ? {} : { view }), sel, ...(agent ? { ag: agent } : {}) }).toString();

/** A CSS selector for something inside one agent's section of the context view. */
export const inAgent = (agent, selector) => `[data-testid=agent][data-agent=${agent}] ${selector}`;

/**
 * Takes an open apply dialog from its first screen to the end: the one-time explanation, every
 * warning that needs a tick, then the confirm button. Resolves when the dialog is gone.
 */
export async function confirmApply(page) {
  await page.until(`!!document.querySelector("[data-testid=apply-confirm], [data-testid=apply-continue]")`, "the apply dialog to show a confirm or a continue button");
  if (await page.count("[data-testid=apply-continue]")) {
    await page.click("[data-testid=apply-continue]");
    await page.waitFor("[data-testid=apply-confirm]");
  }
  // A tick can arrive after the diff does, so tick until the confirm button is enabled.
  await page.eval(`[...document.querySelectorAll("[data-testid=apply-tick]")].forEach((c) => { if (!c.checked) c.click(); })`);
  await page.until(`document.querySelector("[data-testid=apply-confirm]")?.disabled === false`, "the confirm button to enable");
  await page.click("[data-testid=apply-confirm]");
  await page.waitGone("[role=dialog]", 15000);
}

export const json = (text) => JSON.parse(text);

/** The position of the first hook row whose text contains `needle`. Fails the step when there is none. */
export async function hookIndex(page, needle) {
  const rows = await page.all("[data-testid=hook-row]");
  const index = rows.findIndex((row) => row.text.includes(needle));
  if (index < 0) throw new Error(`no hook row mentions ${needle}`);
  return index;
}

/** Presses the on/off switch of the hook row that mentions `needle`. */
export async function flipHook(page, needle) {
  await page.click("[data-testid=hook-switch]", await hookIndex(page, needle));
}
