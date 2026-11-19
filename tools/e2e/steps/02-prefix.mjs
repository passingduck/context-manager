// The prefix strip of every agent with blocks is one run of contiguous segments that ends at its always-loaded total.
import { inAgent, open } from "../helpers.mjs";

export default {
  name: "prefix: contiguous segments end at the always-loaded total",
  async run(t) {
    const { page, check } = t;
    await open(t, "", "[data-testid=agent]");
    const agents = (await page.all("[data-testid=agent]")).map((a) => a.agent);
    let checked = 0;
    for (const agent of agents) {
      if ((await page.count(inAgent(agent, "[data-testid=prefix]"))) === 0) continue;
      const total = Number((await page.one(inAgent(agent, "[data-testid=agent-always]"))).tokens);
      const segs = (await page.all(inAgent(agent, "[data-testid=prefix] [data-start]"))).map((s) => ({ start: Number(s.start), end: Number(s.end) }));
      let at = 0;
      const gap = segs.find((s) => {
        const bad = s.start !== at || s.end < s.start;
        at = s.end;
        return bad;
      });
      checked += 1;
      check(`${agent}: ${segs.length} segments are contiguous from 0`, segs.length > 0 && !gap, gap ? `a segment starts at ${gap.start}, expected ${at}` : "no segments");
      check(`${agent}: the last segment ends at the always-loaded total ${total}`, segs.at(-1)?.end === total, `ends at ${segs.at(-1)?.end}`);
    }
    check("at least one agent has a prefix strip", checked > 0, `agents: ${agents.join(", ")}`);
  },
};
