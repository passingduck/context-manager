// A write from outside while the editor holds unsaved text ends in the conflict view, and the disk keeps the outside text.
import { appendFileSync } from "node:fs";
import { open, read, routeTo } from "../helpers.mjs";

export default {
  name: "conflict: an outside write while the editor is dirty",
  async run(t) {
    const { page, box, check } = t;
    const file = `${box.proj}/AGENTS.md`;
    await open(t, routeTo("context", `file:${file}`, "codex"), ".cm-content");
    await page.type(".cm-content", "\nTyped in the browser, never saved.");
    await page.until(`!document.querySelector("[data-testid=file-save]").disabled`, "the Save button to enable");
    appendFileSync(file, "Outside line from a shell.\n");
    await page.waitFor("[data-testid=changed-on-disk]", 15000);
    check("the editor says the file changed on disk", true);
    await page.until(`document.querySelector("[data-testid=file-save]")?.disabled === false`, "the Save button to enable after the reload of the file");
    await page.click("[data-testid=file-save]");
    await page.waitFor("[role=dialog] [data-testid=conflict]", 15000);
    check("the save ends in the conflict view", true);
    const text = read(file);
    check("the disk keeps the outside text", text.includes("Outside line from a shell."), JSON.stringify(text));
    check("the disk does not have the unsaved text", !text.includes("Typed in the browser"), JSON.stringify(text));
    check("the conflict dialog offers no confirm button", (await page.count("[data-testid=apply-confirm]")) === 0);
  },
};
