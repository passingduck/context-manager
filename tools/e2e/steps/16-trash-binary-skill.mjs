// A skill directory the history cannot hold completely is not removed when there is no OS trash.
import { read, records, seed, trashFromRow } from "../helpers.mjs";
import { existsSync, readFileSync, writeFileSync } from "node:fs";

export default {
  name: "trash: a skill with a binary file is refused",
  async run(t) {
    const { page, box, check } = t;
    const dir = `${box.proj}/.claude/skills/bin-skill`;
    const bin = [0xff, 0xfe, 0x00, 0x80];
    seed(dir, { "SKILL.md": "---\nname: bin-skill\ndescription: Holds a binary file.\n---\n\nBody.\n", "refs/notes.md": "notes" });
    writeFileSync(`${dir}/data.bin`, Buffer.from(bin));

    await trashFromRow(t, "claude", `${dir}/SKILL.md`);
    const text = await page.eval(`document.querySelector("[role=dialog]").innerText`);
    check("the preview warns that the directory cannot be restored", /cannot bring it back/.test(text), text.slice(0, 400));
    await page.eval(`[...document.querySelectorAll("[data-testid=apply-tick]")].forEach((c) => { if (!c.checked) c.click(); })`);
    await page.until(`document.querySelector("[data-testid=apply-confirm]")?.disabled === false`, "the confirm button to enable");
    await page.click("[data-testid=apply-confirm]");
    await page.until(`!!document.querySelector("[role=dialog] [role=alert]") && !document.querySelector("[data-testid=apply-confirm]")`, "the dialog to show the refusal");
    const failure = await page.eval(`document.querySelector("[role=dialog] [role=alert]").innerText`);
    check("the dialog shows that the apply failed", /failed|Nothing was written/i.test(failure), failure);
    check("the directory and every file in it are still there", existsSync(`${dir}/SKILL.md`) && read(`${dir}/refs/notes.md`) === "notes" && readFileSync(`${dir}/data.bin`).equals(Buffer.from(bin)));
    const newest = (await records(t)).find((r) => r.summary.edit.kind === "trash" && r.summary.targets.some((p) => p.startsWith(dir)));
    check("the history shows the refused trash as aborted", newest?.state === "aborted", JSON.stringify(newest));
    await page.click("[role=dialog] button");
  },
};
