// A revert from the history view undoes the newest change.
import { confirmApply, open, read } from "../helpers.mjs";

export default {
  name: "history: revert the newest change",
  async run(t) {
    const { page, host, box, check } = t;
    const config = `${box.home}/.codex/config.toml`;
    const newest = (await host.rpc("history", { cwd: box.proj, before: null, limit: 50 })).body
      .filter((r) => r.state === "sealed")
      .sort((a, b) => b.id.localeCompare(a.id))[0];
    check("the newest change is the Codex switch", newest?.summary.edit.kind === "setHookEnabled" && newest.summary.targets.includes(config), JSON.stringify(newest?.summary));

    await open(t, "view=history", "[data-testid=history-row]");
    await page.until(`document.querySelector("[data-testid=history-row]")?.getAttribute("aria-pressed") === "true"`, "the newest change to be selected");
    check("the history lists the newest change first", (await page.one("[data-testid=history-row]")).change === newest.id);
    await page.click("[data-testid=change-revert]");
    await confirmApply(page);
    check("config.toml is back to what it was before the switch", read(config) === t.state.codexConfigBefore, JSON.stringify(read(config)));
    const after = (await host.rpc("history", { cwd: box.proj, before: null, limit: 50 })).body;
    check("the revert is a new change in the history", after.length > 0 && after.some((r) => r.summary.edit.kind === "revert"), JSON.stringify(after.map((r) => r.summary.edit.kind)));
  },
};
