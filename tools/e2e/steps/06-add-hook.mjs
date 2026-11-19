// A hook added through the form is in the settings file as valid JSON.
import { confirmApply, inAgent, open, read } from "../helpers.mjs";

export default {
  name: "hooks: add a hook through the form",
  async run(t) {
    const { page, box, check } = t;
    const file = `${box.proj}/.claude/settings.json`;
    check("the project settings file does not exist yet", read(file) === null);
    await open(t, "view=hooks", "[data-testid=hook-row]");
    // The first Add button belongs to the first agent in the registry, which is Claude.
    const sections = await page.all("section[aria-label]");
    check("the hooks view has a section for each agent", sections.length >= 3, sections.map((s) => s.text.slice(0, 20)).join("|"));
    await page.click("[data-testid=add-hook]", 0);
    await page.waitFor("[role=dialog] [data-testid=hook-file]");
    await page.fill("[data-testid=hook-file]", file);
    await page.fill("[data-testid=hook-event]", "Stop");
    await page.fill("[data-testid=hook-field-command]", "echo cm-e2e-stop");
    await page.click("[data-testid=form-review]");
    await confirmApply(page);
    const text = read(file);
    let parsed = null;
    try {
      parsed = JSON.parse(text ?? "");
    } catch {
      // The assertion below reports it.
    }
    check("the settings file is valid JSON", parsed !== null, text);
    const handler = parsed?.hooks?.Stop?.[0]?.hooks?.[0];
    check("it holds the command under hooks.Stop", handler?.type === "command" && handler?.command === "echo cm-e2e-stop", JSON.stringify(parsed));
    await page.until(`[...document.querySelectorAll("[data-testid=hook-row]")].some((r) => r.dataset.event === "Stop" && r.innerText.includes("echo cm-e2e-stop"))`, "the new hook row");
    check("the hooks view lists the new hook", true);
    t.state.stopSettings = text;
  },
};
