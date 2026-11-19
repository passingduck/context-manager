// Adding a memory writes the topic file and one index line.
import { confirmApply, list, open, read } from "../helpers.mjs";

export default {
  name: "memory: add a memory",
  async run(t) {
    const { page, box, check } = t;
    const dir = `${box.home}/.claude/projects/${box.proj.replace(/[^A-Za-z0-9]/g, "-")}/memory`;
    check("the memory directory does not exist yet", list(dir).length === 0, list(dir).join(", "));
    await open(t, "view=memory", "[data-testid=memory-add]");
    await page.click("[data-testid=memory-add]", 0);
    await page.waitFor("[role=dialog] [data-testid=memory-title]");
    await page.fill("[data-testid=memory-title]", "Prefers pnpm");
    await page.fill("[data-testid=memory-description]", "The operator uses pnpm for every JavaScript project.");
    await page.fill("[data-testid=memory-body]", "Use pnpm, never npm or yarn.");
    await page.click("[data-testid=form-review]");
    await confirmApply(page);
    const files = list(dir).sort();
    const topics = files.filter((f) => f !== "MEMORY.md");
    check("the directory holds MEMORY.md and one topic file", files.includes("MEMORY.md") && topics.length === 1, files.join(", "));
    const topic = read(`${dir}/${topics[0]}`) ?? "";
    check("the topic file holds the name, the description and the body", topic.includes("name: prefers-pnpm") && topic.includes("The operator uses pnpm for every JavaScript project.") && topic.includes("Use pnpm, never npm or yarn."), topic);
    const index = (read(`${dir}/MEMORY.md`) ?? "").split("\n").filter((l) => l.trim() !== "");
    check("MEMORY.md has exactly one line, and it points at the topic file", index.length === 1 && index[0].includes(`(${topics[0]})`) && index[0].includes("Prefers pnpm"), JSON.stringify(index));
  },
};
