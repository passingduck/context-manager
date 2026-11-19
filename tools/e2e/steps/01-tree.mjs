// The tree shows the project's instruction file, its import and a nested file, with the right load badges.
import { inAgent, open } from "../helpers.mjs";

export default {
  name: "tree: files and load badges",
  async run(t) {
    const { page, box, check } = t;
    await open(t, "", "[data-testid=agent]");
    await page.until(`document.querySelectorAll("[data-testid=load-row]").length > 0`, "load rows");
    const rows = async (agent) => {
      const all = await page.all(inAgent(agent, "[data-testid=load-row]"));
      // A file can appear twice: once by discovery and once under its host. Keep the most specific row.
      return Object.fromEntries(all.sort((a, b) => Number(a.via !== "discovery") - Number(b.via !== "discovery")).map((r) => [r.file, r]));
    };

    // docs/agents/claude-code.md: CLAUDE.md loads at session start, an @import loads with its host, and a
    // CLAUDE.md under a subdirectory loads when the agent reads a file there.
    const claude = await rows("claude");
    const root = claude[`${box.proj}/CLAUDE.md`];
    const imported = claude[`${box.proj}/AGENTS.md`];
    const nested = claude[`${box.proj}/pkg/CLAUDE.md`];
    check("Claude loads CLAUDE.md at session start", root?.when === "sessionStart" && root?.status === "loaded", JSON.stringify(root));
    check("Claude loads the imported AGENTS.md with its host, by import", imported?.when === "withHost" && imported?.via === "import", JSON.stringify(imported));
    check("Claude loads the nested CLAUDE.md on access", nested?.when === "onAccess", JSON.stringify(nested));

    // docs/agents/codex.md: AGENTS.md at the project root is project documentation, loaded at session start.
    const codex = await rows("codex");
    const doc = codex[`${box.proj}/AGENTS.md`];
    check("Codex loads AGENTS.md at session start", doc?.when === "sessionStart" && doc?.status === "loaded", JSON.stringify(doc));
    check("Codex does not load the nested CLAUDE.md", !(`${box.proj}/pkg/CLAUDE.md` in codex), Object.keys(codex).join("\n"));

    // docs/agents/gemini-cli.md: Gemini reads GEMINI.md, not AGENTS.md or CLAUDE.md, by default.
    const gemini = await rows("gemini");
    check("Gemini loads none of the project's files", !(`${box.proj}/AGENTS.md` in gemini) && !(`${box.proj}/CLAUDE.md` in gemini), Object.keys(gemini).join("\n"));
  },
};
