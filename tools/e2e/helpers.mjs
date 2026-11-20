import { existsSync, mkdirSync, readdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";

export const read = (path) => (existsSync(path) ? readFileSync(path, "utf8") : null);
export const list = (dir) => (existsSync(dir) ? readdirSync(dir) : []);

/** Opens the app at a route on a fresh page, and waits until the selector shows. */
export async function open(t, route, ready, lang = "en") {
  await t.page.goto("about:blank");
  await t.page.goto(t.host.page(route, lang));
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
  // With no OS trash the dialog ends on a notice that lists what was not trashed, such as a parked record the tool removes.
  await page.until(`!document.querySelector("[role=dialog]") || !!document.querySelector("[data-testid=apply-close]")`, "the dialog to close or show its notice", 15000);
  if (await page.count("[data-testid=apply-close]")) await page.click("[data-testid=apply-close]");
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

/** Writes files under a directory. `files` maps a relative path to a string or bytes. */
export function seed(dir, files) {
  for (const [rel, content] of Object.entries(files)) {
    mkdirSync(dirname(join(dir, rel)), { recursive: true });
    writeFileSync(join(dir, rel), content);
  }
}

/** The history records, newest first. */
export async function records(t) {
  return (await t.host.rpc("history", { cwd: t.box.proj, before: null, limit: 100 })).body.sort((a, b) => b.id.localeCompare(a.id));
}

/** The newest record that edits with `kind` and names `file`. */
export async function recordOf(t, kind, file) {
  return (await records(t)).find((r) => r.summary.edit.kind === kind && r.summary.targets.includes(file));
}

/** Opens the delete dialog of the row for `file` in one agent's section, and waits for the review. */
export async function trashFromRow(t, agent, file) {
  const { page } = t;
  await open(t, "", `[data-testid=agent][data-agent=${agent}] [data-testid=load-row]`);
  const row = `[data-testid=agent][data-agent=${agent}] [data-testid=load-row][data-file="${file}"]`;
  // The tree folds a group of several skills. Open every folded group of this agent until the row shows.
  for (let pass = 0; pass < 4 && !(await page.count(row)); pass++) {
    await page.eval(`[...document.querySelectorAll("[data-testid=agent][data-agent=${agent}] [aria-expanded=false]")].filter((e) => e.dataset.testid !== "row-menu").forEach((e) => e.click())`);
    await page.sleep(300);
  }
  await page.waitFor(row);
  await page.click(`${row} [data-testid=row-menu]`);
  await page.click("[data-testid=row-trash]");
  await waitReview(page);
}

/** Waits until the apply dialog shows the review, clicking through a first-use explanation. */
export async function waitReview(page) {
  await page.until(`!!document.querySelector("[data-testid=apply-confirm], [data-testid=apply-continue]")`, "the apply dialog");
  if (await page.count("[data-testid=apply-continue]")) {
    await page.click("[data-testid=apply-continue]");
    await page.waitFor("[data-testid=apply-confirm]");
  }
}

/** The files the open dialog lists in its diff, as the text of each file heading. */
export const dialogFiles = (page) => page.eval(`[...document.querySelectorAll("[role=dialog] h4")].map((h) => h.innerText)`);

/** Reverts a change from the history view and confirms. */
export async function revertFromHistory(t, id) {
  const { page } = t;
  await open(t, `view=history&sel=${encodeURIComponent(`change:${id}`)}`, "[data-testid=change-revert]");
  await page.click("[data-testid=change-revert]");
  await confirmApply(page);
}

/**
 * Confirms a trash on a host with no OS trash. The dialog then ends on a notice that lists the
 * paths as not trashed. Returns those paths and closes the dialog.
 */
export async function confirmTrash(page) {
  await page.until(`!!document.querySelector("[data-testid=apply-confirm]")`, "the confirm button");
  await page.eval(`[...document.querySelectorAll("[data-testid=apply-tick]")].forEach((c) => { if (!c.checked) c.click(); })`);
  await page.until(`document.querySelector("[data-testid=apply-confirm]")?.disabled === false`, "the confirm button to enable");
  await page.click("[data-testid=apply-confirm]");
  await page.waitFor("[data-testid=not-trashed]", 15000);
  const listed = await page.eval(`[...document.querySelectorAll("[data-testid=not-trashed] li")].map((e) => e.innerText)`);
  await page.click("[data-testid=apply-close]");
  await page.waitGone("[role=dialog]");
  return listed;
}
