// The test run of the added hook shows exit code 0 and its output.
import { hookIndex, open } from "../helpers.mjs";

export default {
  name: "hooks: test run",
  async run(t) {
    const { page, check } = t;
    await open(t, "view=hooks", "[data-testid=hook-row]");
    await page.click("[data-testid=hook-row]", await hookIndex(page, "echo cm-e2e-stop"));
    await page.click("[data-testid=hook-run]");
    await page.waitFor("[data-testid=hook-result]", 20000);
    const result = await page.one("[data-testid=hook-result]");
    check("the test run exits with code 0", result.exitCode === "0" && result.timedOut === "false", JSON.stringify(result));
    const out = await page.one("[data-testid=hook-stdout]");
    check("the test run shows the command's output", out?.text.trim() === "cm-e2e-stop", JSON.stringify(out?.text));
  },
};
