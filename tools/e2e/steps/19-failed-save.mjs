// A save that cannot write ends as an aborted record and leaves the file as it was.
import { open, read, records, routeTo } from "../helpers.mjs";
import { chmodSync } from "node:fs";

export default {
  name: "save: a failed save is aborted and writes nothing",
  async run(t) {
    const { page, box, check } = t;
    const file = `${box.proj}/CLAUDE.md`;
    const bytes = read(file);
    const aborted = async () => (await records(t)).filter((r) => r.state === "aborted" && r.summary.edit.kind === "save" && r.summary.targets.includes(file));
    const before = (await aborted()).length;

    await open(t, routeTo("context", `file:${file}`, "claude"), ".cm-content");
    await page.type(".cm-content", "\nThis line can never be written.");
    await page.until(`!document.querySelector("[data-testid=file-save]").disabled`, "the Save button to enable");
    chmodSync(box.proj, 0o555);
    try {
      await page.click("[data-testid=file-save]");
      await page.until(`!!document.querySelector("[data-testid=apply-confirm], [data-testid=apply-continue]")`, "the apply dialog");
      if (await page.count("[data-testid=apply-continue]")) {
        await page.click("[data-testid=apply-continue]");
        await page.waitFor("[data-testid=apply-confirm]");
      }
      await page.eval(`[...document.querySelectorAll("[data-testid=apply-tick]")].forEach((c) => { if (!c.checked) c.click(); })`);
      await page.until(`document.querySelector("[data-testid=apply-confirm]")?.disabled === false`, "the confirm button to enable");
      await page.click("[data-testid=apply-confirm]");
      await page.until(`!!document.querySelector("[role=dialog] [role=alert]") && !document.querySelector("[data-testid=apply-confirm]")`, "the dialog to show the error", 15000);
    } finally {
      chmodSync(box.proj, 0o755);
    }
    const error = await page.eval(`document.querySelector("[role=dialog] [role=alert]").innerText`);
    check("the UI shows the error", error.length > 0, error);
    check("the file is unchanged", read(file) === bytes);
    const first = await aborted();
    check("the history shows the record as aborted", first.length === before + 1, JSON.stringify(first.map((r) => r.state)));

    // The directory is writable again. Reading the history must not finish the change.
    const second = await aborted();
    check("a second read of the history still shows it as aborted", second.length === before + 1);
    check("the file is still unchanged", read(file) === bytes);
    const record = second[0];
    await open(t, `view=history&sel=${encodeURIComponent(`change:${record.id}`)}`, "[data-testid=aborted]");
    check("the history view lists an aborted row", (await page.count("[data-testid=history-row][data-state=aborted]")) > 0);
    const note = (await page.one("[data-testid=aborted]")).text;
    check("the detail says that nothing of it is on disk", /Nothing of it is on disk/.test(note), note);
    await open(t, `view=history&sel=${encodeURIComponent(`change:${record.id}`)}`, "[data-testid=aborted]", "ko");
    await page.screenshot("history-aborted-ko");
  },
};
