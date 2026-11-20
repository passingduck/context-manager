// A skill file that an import also loads: each place it loads shows what the model receives there.
import { open, routeTo, seed } from "../helpers.mjs";

const SKILL = ".claude/skills/demo-e2e/SKILL.md";
const CARDS = "[data-testid=why-card][data-agent=claude]";

/** The claude row of the skill file that came in a given way. */
const row = (file, via) => `[data-testid=agent][data-agent=claude] [data-testid=load-row][data-file="${file}"][data-via=${via}]`;

const selected = (page, selector) => page.eval(`document.querySelector(${JSON.stringify(selector)})?.getAttribute("aria-selected")`);

/** Each claude card in the detail pane: how its load came in, whether it is the picked one, its sentence and its text. */
const cards = (page) =>
  page.eval(`[...document.querySelectorAll(${JSON.stringify(CARDS)})].map((c) => ({
    via: c.dataset.via, kind: c.dataset.kind, chosen: c.dataset.chosen === "true",
    sends: c.querySelector("[data-testid=why-sends]")?.textContent ?? "",
    text: c.querySelector("[data-testid=load-text]")?.textContent ?? c.querySelector("[data-testid=load-text-whole]")?.textContent ?? null,
  }))`);

const positionLine = (page) => page.eval(`document.querySelector("[data-testid=position-line][data-prefix-position=claude]")?.innerText ?? ""`);

/** Opens every folded group of the claude tree until the selector shows. */
async function unfold(page, selector) {
  for (let pass = 0; pass < 4 && !(await page.count(selector)); pass++) {
    await page.eval(`[...document.querySelectorAll("[data-testid=agent][data-agent=claude] [aria-expanded=false]")].filter((e) => e.dataset.testid !== "row-menu").forEach((e) => e.click())`);
    await page.sleep(300);
  }
  await page.waitFor(selector);
}

/** Waits until the three claude cards show their texts. */
const textsShown = (page) =>
  page.until(`[...document.querySelectorAll(${JSON.stringify(CARDS)})].filter((c) => c.querySelector("[data-testid=load-text], [data-testid=load-text-whole]")).length === 3`, "three cards with their texts");

export default {
  name: "load text: an imported skill shows each place it loads",
  async run(t) {
    const { page, box, check } = t;
    const skill = `${box.proj}/${SKILL}`;
    seed(box.proj, {
      [SKILL]: "---\nname: demo-e2e\ndescription: Shows the e2e demo skill\n---\n\nStep one.\nStep two.\nStep three.\n",
      "CLAUDE.local.md": `@${SKILL}\n`,
    });
    const listing = row(skill, "listing");
    const imported = row(skill, "import");

    await open(t, routeTo("context", `file:${box.proj}/CLAUDE.md`, "claude"), "[data-testid=load-row]");
    await unfold(page, listing);
    await page.waitFor(imported);
    await page.click(listing);
    await page.until(`location.hash.includes("at=")`, "the route to name the place");
    check("the listing row is selected", (await selected(page, listing)) === "true");
    check("the import row of the same file is not", (await selected(page, imported)) === "false");
    const listingHash = await page.eval("location.hash");

    await textsShown(page);
    const shown = await cards(page);
    const byVia = (via) => shown.find((c) => c.via === via);
    check("the detail pane shows three cards for the skill file", shown.length === 3, JSON.stringify(shown));
    check("the three cards show three different texts", new Set(shown.map((c) => c.text)).size === 3, JSON.stringify(shown.map((c) => c.text)));
    check("the picked listing card comes first", shown[0]?.via === "listing" && shown[0]?.chosen === true, JSON.stringify(shown[0]));
    check("the import card has no frontmatter", byVia("import")?.text?.includes("Step one.") && !byVia("import")?.text?.includes("description:"), byVia("import")?.text);
    check("the listing card is one line", byVia("listing")?.text?.includes("demo-e2e") && !byVia("listing")?.text?.trim().includes("\n"), byVia("listing")?.text);
    check("the listing card's sentence differs from the import card's", byVia("listing")?.sends && byVia("listing")?.sends !== byVia("import")?.sends, `${byVia("listing")?.sends} | ${byVia("import")?.sends}`);
    const atListing = await positionLine(page);

    await page.click(imported);
    await page.until(`document.querySelector(${JSON.stringify(imported)})?.getAttribute("aria-selected") === "true"`, "the import row to be selected");
    check("the listing row is no longer selected", (await selected(page, listing)) === "false");
    const atImport = await positionLine(page);
    check("the position line follows the selected place", atListing !== "" && atImport !== "" && atListing !== atImport, `${atListing} | ${atImport}`);
    const importHash = await page.eval("location.hash");

    // The selection lives in the address, so a reload keeps it, and the folded group that holds the row opens.
    await page.hash(listingHash.replace(/^#/, ""));
    await page.eval("location.reload()");
    await page.sleep(400);
    await page.until(`document.querySelector(${JSON.stringify(listing)})?.getAttribute("aria-selected") === "true"`, "the listing row to be selected after the reload");
    check("after a reload the listing row is still selected", (await selected(page, listing)) === "true");
    check("after a reload the import row is still not selected", (await selected(page, imported)) === "false");

    await page.click("[data-testid=mode-order]");
    const marks = "[data-testid=agent][data-agent=claude] [data-testid=literal-mark]";
    await page.waitFor(marks);
    const marked = await page.all(marks);
    check("the order view marks one piece", marked.length === 1, JSON.stringify(marked.map((m) => m.text)));
    check("the marked piece is the listing line", marked[0]?.text === byVia("listing")?.text, `${marked[0]?.text} | ${byVia("listing")?.text}`);

    await open(t, "view=doctor", "[data-testid=diagnostic]");
    const found = await page.all("[data-testid=diagnostic][data-problem=bodyAlsoImported]");
    check("the doctor lists the imported skill body", found.length === 1, JSON.stringify(found.map((f) => f.text)));

    // Korean screenshots for review, saved only when CM_E2E_SHOTS names a directory.
    const shoot = async (name, route, ready, theme) => {
      await page.goto("about:blank");
      await page.goto(t.host.page(route, "ko").replace("?lang=ko", `?lang=ko&theme=${theme}`));
      await page.waitFor(ready, 20000);
      await page.sleep(500);
      await page.eval(`document.querySelector("[data-testid=why-card][data-chosen=true], [data-testid=literal-mark], [data-testid=diagnostic][data-problem=bodyAlsoImported]")?.scrollIntoView({ block: "center" })`);
      await page.sleep(200);
      await page.screenshot(name);
    };
    const strip = (hash) => hash.replace(/^#/, "").replace(/(^|&)mode=order/, "");
    for (const theme of ["light", "dark"]) {
      await shoot(`21-listing-${theme}`, strip(listingHash), "[data-testid=why-card][data-chosen=true] [data-testid=load-text]", theme);
      await shoot(`21-import-${theme}`, strip(importHash), "[data-testid=why-card][data-chosen=true] [data-testid=load-text]", theme);
    }
    await shoot("21-order", `${strip(listingHash)}&mode=order`, marks, "light");
    await shoot("21-doctor", "view=doctor", "[data-testid=diagnostic][data-problem=bodyAlsoImported]", "light");
  },
};
