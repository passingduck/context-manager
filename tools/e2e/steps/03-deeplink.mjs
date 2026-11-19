// A link with a token and view=hooks lands on the hooks view, and the token leaves the address bar.
import { open } from "../helpers.mjs";

export default {
  name: "deep link: token and view=hooks",
  async run(t) {
    const { page, check } = t;
    await open(t, "view=hooks", "[data-testid=hook-row]");
    check("the hooks view shows hook rows", (await page.count("[data-testid=hook-row]")) >= 3);
    const hash = await page.eval("location.hash");
    check("the route stays in the fragment", hash.includes("view=hooks"), hash);
    check("the token is out of the address bar", !hash.includes("token="), hash);
    check("the context tree is not shown", (await page.count("[data-testid=load-row]")) === 0);
  },
};
