// Switching a Codex hook off writes only enabled = false under hooks.state, and leaves hooks.json alone.
import { confirmApply, flipHook, open, read } from "../helpers.mjs";

export default {
  name: "hooks: switch a Codex hook off",
  async run(t) {
    const { page, box, check } = t;
    const hooks = `${box.home}/.codex/hooks.json`;
    const config = `${box.home}/.codex/config.toml`;
    const hooksBefore = read(hooks);
    t.state.codexConfigBefore = read(config);
    await open(t, "view=hooks", "[data-testid=hook-row]");
    await flipHook(page, "echo codex-stop");
    await confirmApply(page);
    const text = read(config) ?? "";
    // docs/agents/codex.md: the native switch is `enabled` under `[hooks.state."<key>"]` in the user config.toml.
    check("config.toml holds only enabled = false under hooks.state", /^\s*\[hooks\.state\.(?:"[^"\n]+"|'[^'\n]+')\]\nenabled = false\n$/.test(text), JSON.stringify(text));
    check("hooks.json is untouched", read(hooks) === hooksBefore);
    await page.until(`[...document.querySelectorAll("[data-testid=hook-row]")].some((r) => r.dataset.state === "disabled" && r.innerText.includes("echo codex-stop"))`, "the Codex hook row to show as disabled");
    check("the hooks view lists the Codex hook as disabled", true);
  },
};
