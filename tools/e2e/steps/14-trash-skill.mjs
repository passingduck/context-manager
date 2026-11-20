// Trash a skill: the whole directory goes, and a revert brings every file back.
import { confirmTrash, dialogFiles, read, recordOf, revertFromHistory, seed, trashFromRow } from "../helpers.mjs";
import { existsSync } from "node:fs";

const FILES = {
  "SKILL.md": "---\nname: e2e-skill\ndescription: A skill the e2e run trashes.\n---\n\nDo the thing.\n",
  "refs/notes.md": "Notes with no final newline",
  "scripts/run.sh": "#!/bin/sh\necho e2e\n",
};

export default {
  name: "trash: a skill directory, then revert",
  async run(t) {
    const { page, box, check } = t;
    const dir = `${box.proj}/.claude/skills/e2e-skill`;
    seed(dir, FILES);
    await trashFromRow(t, "claude", `${dir}/SKILL.md`);
    check("the preview says the whole directory goes", (await page.count("[data-testid=dir-notice]")) === 1 && (await page.one("[data-testid=dir-notice]")).text.includes("directory"));
    const listed = (await dialogFiles(page)).join("\n");
    check("it lists every file of the skill", Object.keys(FILES).every((f) => listed.includes(f)), listed);
    const notTrashed = await confirmTrash(page);
    check("the notice lists the directory as not trashed", notTrashed.length === 1 && notTrashed[0].includes("e2e-skill"), JSON.stringify(notTrashed));
    check("the directory is gone", !existsSync(dir));
    const record = await recordOf(t, "trash", `${dir}`) ?? (await recordOf(t, "trash", `${dir}/SKILL.md`));
    check("the history holds the trash", record?.state === "sealed", JSON.stringify(record));
    await revertFromHistory(t, record.id);
    check("the revert brings every file back byte for byte", Object.entries(FILES).every(([f, text]) => read(`${dir}/${f}`) === text), JSON.stringify(Object.keys(FILES).map((f) => read(`${dir}/${f}`))));
  },
};
