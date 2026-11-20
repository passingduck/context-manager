// Trashing a nested GEMINI.md takes that file only. Its directory and the files beside it stay.
import { confirmTrash, dialogFiles, read, seed, trashFromRow } from "../helpers.mjs";
import { existsSync } from "node:fs";

export default {
  name: "trash: a nested Gemini context file only",
  async run(t) {
    const { page, box, check } = t;
    const files = { "src/GEMINI.md": "# Source rules\n\nKeep it small.\n", "src/main.rs": "fn main() {}\n", "src/lib/x.rs": "pub fn x() {}\n" };
    seed(box.proj, files);
    await trashFromRow(t, "gemini", `${box.proj}/src/GEMINI.md`);
    const listed = await dialogFiles(page);
    check("the preview names GEMINI.md and nothing else", listed.length === 1 && listed[0].includes("src/GEMINI.md"), JSON.stringify(listed));
    check("the preview does not call it a directory", (await page.count("[data-testid=dir-notice]")) === 0);
    const notTrashed = await confirmTrash(page);
    check("the notice lists GEMINI.md only", notTrashed.length === 1 && notTrashed[0].includes("src/GEMINI.md"), JSON.stringify(notTrashed));
    check("GEMINI.md is gone", !existsSync(`${box.proj}/src/GEMINI.md`));
    check("main.rs and lib/x.rs are untouched", read(`${box.proj}/src/main.rs`) === files["src/main.rs"] && read(`${box.proj}/src/lib/x.rs`) === files["src/lib/x.rs"]);
  },
};
