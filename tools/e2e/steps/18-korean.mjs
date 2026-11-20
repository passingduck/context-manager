// The language switch turns the labels Korean.
import { open, routeTo } from "../helpers.mjs";

const HANGUL = /[가-힣]/;

export default {
  name: "language: switch the UI to Korean",
  async run(t) {
    const { page, box, check } = t;
    await open(t, routeTo("context", `file:${box.proj}/CLAUDE.md`, "claude"), "[data-testid=file-save]");
    const labels = ["file-save", "file-trash", "new-item"];
    const english = await Promise.all(labels.map(async (id) => (await page.one(`[data-testid=${id}]`)).text));
    check("the labels are English before the switch", english.every((text) => text && !HANGUL.test(text)), JSON.stringify(english));
    await page.click("[data-testid=lang-ko]");
    await page.until(`document.documentElement.lang === "ko"`, "the page language to become Korean");
    for (const id of labels) {
      const text = (await page.one(`[data-testid=${id}]`)).text;
      check(`${id} holds Korean text`, HANGUL.test(text), JSON.stringify(text));
    }
    await page.click("[data-testid=lang-en]");
  },
};
