// Switching a Claude hook off empties its entry and parks it. Switching it on restores the file byte for byte.
import { confirmApply, flipHook, list, open, read } from "../helpers.mjs";

export default {
  name: "hooks: park and unpark a Claude hook",
  async run(t) {
    const { page, box, check } = t;
    const file = `${box.proj}/.claude/settings.json`;
    const before = read(file);
    const parked = `${box.data}/parked`;
    await open(t, "view=hooks", "[data-testid=hook-row]");

    await flipHook(page, "echo cm-e2e-stop");
    await confirmApply(page);
    const off = read(file);
    check("the settings file is still valid JSON", (() => { try { JSON.parse(off); return true; } catch { return false; } })(), off);
    check("the hook's command is gone from the settings file", !off.includes("cm-e2e-stop"), off);
    const records = list(parked).filter((f) => f.endsWith(".json"));
    check("one parked record exists in the data directory", records.length === 1, records.join(", "));
    check("the parked record keeps the command", records.length === 1 && read(`${parked}/${records[0]}`).includes("echo cm-e2e-stop"));
    await page.until(`[...document.querySelectorAll("[data-testid=hook-row]")].some((r) => r.dataset.state === "disabled" && r.innerText.includes("echo cm-e2e-stop"))`, "the hook row to show as disabled");
    check("the hooks view lists the parked hook as disabled", true);

    await flipHook(page, "echo cm-e2e-stop");
    await confirmApply(page);
    const on = read(file);
    check("the settings file is back, byte for byte", on === before, `before:\n${before}\nafter:\n${on}`);
  },
};
