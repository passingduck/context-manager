// Switching a Gemini hook off adds one entry to hooksConfig.disabled in the user settings. Switching it on restores the bytes.
import { confirmApply, flipHook, json, open, read } from "../helpers.mjs";
import { deepStrictEqual } from "node:assert";

export default {
  name: "hooks: switch a Gemini hook off and on",
  async run(t) {
    const { page, box, check } = t;
    const file = `${box.home}/.gemini/settings.json`;
    const start = read(file);
    const hooksBefore = json(start).hooks;

    await open(t, "view=hooks", "[data-testid=hook-row]");
    await flipHook(page, "echo gemini-after");
    await confirmApply(page);
    let parsed = null;
    try {
      parsed = json(read(file));
    } catch {
      // The assertion below reports it.
    }
    check("the settings file parses as JSON", parsed !== null, read(file));
    // docs/agents/gemini-cli.md: the native switch is the `hooksConfig.disabled` list.
    const disabled = parsed?.hooksConfig?.disabled;
    check("hooksConfig.disabled gains one entry", Array.isArray(disabled) && disabled.length === 1 && typeof disabled[0] === "string", JSON.stringify(parsed?.hooksConfig));
    let same = false;
    try {
      deepStrictEqual(parsed?.hooks, hooksBefore);
      same = true;
    } catch {
      // Reported below.
    }
    check("the hook's own definition is unchanged", same, JSON.stringify(parsed?.hooks));

    await page.until(`[...document.querySelectorAll("[data-testid=hook-row]")].some((r) => r.dataset.state === "disabled" && r.innerText.includes("echo gemini-after"))`, "the Gemini hook row to show as disabled");
    await flipHook(page, "echo gemini-after");
    await confirmApply(page);
    check("switching it on again restores the starting bytes", read(file) === start, read(file));
  },
};
