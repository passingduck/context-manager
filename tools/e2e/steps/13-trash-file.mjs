// Trash an instruction file from its row menu, then revert it from the history view.
import { confirmTrash, dialogFiles, read, recordOf, revertFromHistory, seed, trashFromRow } from "../helpers.mjs";
import { existsSync } from "node:fs";

export default {
  name: "trash: an instruction file, then revert",
  async run(t) {
    const { page, box, check } = t;
    await t.host.stop();
    t.host = await t.startHost();
    const file = `${box.proj}/trashme/CLAUDE.md`;
    const bytes = "# Trash me\n\nA rule with é and a tab\tand no final newline.";
    seed(box.proj, { "trashme/CLAUDE.md": bytes });

    await trashFromRow(t, "claude", file);
    const listed = await dialogFiles(page);
    check("the preview names the file and nothing else", listed.length === 1 && listed[0].includes("trashme/CLAUDE.md"), JSON.stringify(listed));
    check("the preview does not call it a directory", (await page.count("[data-testid=dir-notice]")) === 0);
    const notTrashed = await confirmTrash(page);
    check("the notice lists the file as not trashed", notTrashed.length === 1 && notTrashed[0].includes("trashme/CLAUDE.md"), JSON.stringify(notTrashed));
    check("the file is gone after the apply", !existsSync(file));
    check("its directory is still there", existsSync(`${box.proj}/trashme`));

    const record = await recordOf(t, "trash", file);
    check("the history holds the trash as a sealed change", record?.state === "sealed", JSON.stringify(record));
    await revertFromHistory(t, record.id);
    check("the revert brings the file back byte for byte", read(file) === bytes, JSON.stringify(read(file)));
  },
};
