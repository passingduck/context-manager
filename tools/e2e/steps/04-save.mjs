// An editor save changes the file once, and the same save again changes nothing.
import { open, routeTo, confirmApply, read } from "../helpers.mjs";

const TYPED = "\nRule typed in the browser.";

export default {
  name: "editor: save, then save again",
  async run(t) {
    const { page, host, box, check } = t;
    const file = `${box.proj}/CLAUDE.md`;
    const before = read(file);
    const historyLength = async () => (await host.rpc("history", { cwd: box.proj, before: null, limit: 50 })).body.length;
    const changes = await historyLength();

    await open(t, routeTo("context", `file:${file}`, "claude"), ".cm-content");
    await page.type(".cm-content", TYPED);
    await page.until(`!document.querySelector("[data-testid=file-save]").disabled`, "the Save button to enable after typing");
    await page.click("[data-testid=file-save]");
    await confirmApply(page);
    await page.until(`document.querySelector("[data-testid=file-save]")?.disabled === true`, "the Save button to disable once the file is saved");
    const once = read(file);
    // The editor decides where the caret sits, so the check is the content, not the position.
    check("the save added the typed line and nothing else", once.length === before.length + TYPED.length && once.includes(TYPED) && once.replace(TYPED, "") === before, JSON.stringify(once));
    check("the save is one change in the history", (await historyLength()) === changes + 1);

    // The same save again: the planner sees text equal to the disk and plans nothing.
    const disk = await host.rpc("read_file", { cwd: box.proj, path: file });
    const again = await host.rpc("plan", { cwd: box.proj, edit: { kind: "save", path: file, base: { kind: "rev", rev: disk.body.state.rev }, text: once } });
    check("planning the same text again has no operations", again.status === 200 && again.body?.changeSet?.ops?.length === 0, JSON.stringify(again.body)?.slice(0, 300));
    check("the file is the same after the second look", read(file) === once);
    check("the history has no second change", (await historyLength()) === changes + 1);
  },
};
