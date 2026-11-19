// On a second start with --read-only, every write control and the hook run controls are disabled, and apply is refused.
import { hookIndex, open, read, routeTo } from "../helpers.mjs";

const allDisabled = async (page, selector) => {
  const found = await page.all(selector);
  return found.length > 0 && found.every((e) => e.disabled);
};

export default {
  name: "read-only: controls are off and apply is refused",
  async run(t) {
    const { page, box, check } = t;
    await t.host.stop();
    t.host = await t.startHost({ readOnly: true });
    const { host } = t;
    const file = `${box.proj}/CLAUDE.md`;
    const bytes = read(file);

    await open(t, "view=hooks", "[data-testid=hook-row]");
    check("every hook switch is disabled", await allDisabled(page, "[data-testid=hook-switch]"));
    check("every Add hook button is disabled", await allDisabled(page, "[data-testid=add-hook]"));
    const index = await hookIndex(page, "echo claude-pre");
    const hookId = (await page.all("[data-testid=hook-row]"))[index].hookId;
    await page.hash(`view=hooks&sel=${encodeURIComponent(`hook:${hookId}`)}`);
    await page.waitFor("[data-testid=hook-run]");
    for (const id of ["hook-edit", "hook-delete", "hook-test-open", "hook-run"]) {
      check(`${id} is disabled`, await allDisabled(page, `[data-testid=${id}]`));
    }

    await open(t, "view=memory", "[data-testid=memory-add]");
    check("every Add memory button is disabled", await allDisabled(page, "[data-testid=memory-add]"));

    await open(t, routeTo("context", `file:${file}`, "claude"), ".cm-content");
    check("the New menu is disabled", await allDisabled(page, "[data-testid=new-item]"));
    await page.type(".cm-content", "typed in a read-only host");
    check("Save stays disabled with unsaved text", await allDisabled(page, "[data-testid=file-save]"));
    check("Delete is disabled in the file detail", await allDisabled(page, "[data-testid=file-trash]"));
    await page.click("[data-testid=row-menu]", 0);
    check("Delete is disabled in the row menu", await allDisabled(page, "[data-testid=row-trash]"));

    const refused = await host.rpc("apply", { cwd: box.proj, edit: { kind: "removeHook", hook: hookId }, digest: "0" });
    check("apply over HTTP is refused as read-only", refused.status === 403 && refused.body?.kind === "readOnly", JSON.stringify(refused));
    const ran = await host.rpc("run_hook", { cwd: box.proj, hook: hookId, input: {} });
    check("run_hook over HTTP is refused as read-only", ran.status === 403 && ran.body?.kind === "readOnly", JSON.stringify(ran));
    check("no file changed", read(file) === bytes);
  },
};
