// The sessions view: a hand-built record in the sandbox home, read back through the real host and drawn by the real UI.
import { open } from "../helpers.mjs";
import { SESSION_ID, writeSession } from "../session-record.mjs";

const bars = "[data-testid=call-bar]";
const rows = "[data-testid=piece-row]";
const callPane = (n) => `[data-testid=call][data-call="${n}"]`;

/** The "place" of every piece row, in the order the list shows them. */
const places = (page) => page.eval(`[...document.querySelectorAll(${JSON.stringify(rows)})].map((r) => r.querySelector("span.truncate")?.textContent ?? "")`);

export default {
  name: "sessions: a recorded session shows its calls, its cache and its pieces",
  async run(t) {
    const { page, box, check } = t;
    const facts = writeSession(box.home, box.proj);
    const base = `view=sessions&ss=${SESSION_ID}&ag=claude`;

    // The list.
    await open(t, "view=sessions", "[data-testid=session-row]");
    const listed = await page.all("[data-testid=session-row]");
    check("the list shows the one session", listed.length === 1 && listed[0].session === SESSION_ID, JSON.stringify(listed));
    check("the row shows the title of the record", listed[0]?.text.includes("Stand-in session for the e2e run"), listed[0]?.text);

    // The session and its chart.
    await page.click("[data-testid=session-row]");
    await page.waitFor(bars);
    check("the chart has one bar per call", (await page.count(bars)) === 7, String(await page.count(bars)));
    check("the chart has one compaction mark, before the sixth call", (await page.count("[data-testid=compaction]")) === 1 && (await page.eval(`document.querySelector("[data-testid=compaction]").dataset.before`)) === String(facts.compactBefore));
    const stats = await page.eval(`Object.fromEntries([...document.querySelectorAll("[data-testid^=stat-]")].map((e) => [e.dataset.testid, e.querySelector("dd").textContent]))`);
    check("the stats show 7 calls", stats["stat-calls"] === "7", JSON.stringify(stats));
    const lost = await page.eval(`[...document.querySelectorAll(${JSON.stringify(bars)})].map((b, i) => (b.dataset.lost === "true" ? i : -1)).filter((i) => i >= 0)`);
    check("the calls that lost their cache are marked", JSON.stringify(lost) === JSON.stringify([facts.lostByChange, facts.compactBefore, facts.lostByExpiry]), JSON.stringify(lost));
    check("a mark is drawn for each", (await page.count("[data-testid=lost-mark]")) === 3);

    // The call that lost its cache by a changed early piece.
    await page.click(bars, facts.lostByChange);
    await page.waitFor(callPane(facts.lostByChange + 1));
    await page.waitFor("[data-testid=loss-note]");
    const note = await page.eval(`document.querySelector("[data-testid=loss-note]").innerText`);
    check("the panel names the first piece that differs", note.includes("piece 2") && note.includes("System block 2"), note);
    check("the hash names the selected call", (await page.eval("location.hash")).includes(`call=${facts.lostByChange + 1}`));

    // The call that lost it with every piece shared.
    await page.click(bars, facts.lostByExpiry);
    await page.waitFor(`${callPane(facts.lostByExpiry + 1)} [data-testid=loss-note]`);
    const expired = await page.eval(`document.querySelector("[data-testid=loss-note]").innerText`);
    check("the panel says that every piece is shared and gives the time since the previous call", expired.includes("shares every piece") && expired.includes("6 min"), expired);

    // A call's pieces, in the order the model received them.
    await page.click(bars, 2);
    await page.waitFor(`${callPane(3)} ${rows}`);
    check("the rebuilt request matches its digest", (await page.eval(`document.querySelector("[data-testid=exactness]")?.dataset.exactness`)) === "exact");
    const order = await places(page);
    check("the pieces start with the system blocks, then the tools, then the messages", order[0] === "System block 1" && order[1] === "System block 2" && order[2] === "Tool definition 1" && order[3] === "Tool definition 2" && order[4]?.startsWith("Message 1"), JSON.stringify(order));
    const messageNumbers = order.filter((p) => p.startsWith("Message")).map((p) => parseInt(p.split(" ")[1], 10));
    check("the message numbers never go down", messageNumbers.every((n, i) => i === 0 || n >= messageNumbers[i - 1]), JSON.stringify(messageNumbers));
    const fresh = await page.eval(`[...document.querySelectorAll(${JSON.stringify(rows)})].filter((r) => r.dataset.fresh === "true").length`);
    check("some pieces are fresh and some are shared", fresh > 0 && fresh < order.length, `${fresh} of ${order.length}`);
    await page.click("[data-testid=only-fresh]");
    const only = await places(page);
    check("'only what this call added' leaves exactly the fresh pieces", only.length === fresh && only.every((p) => p.startsWith("Message 4") || p.startsWith("Message 5")), JSON.stringify(only));
    await page.click("[data-testid=only-fresh]");

    // A piece's text.
    await page.click(`${rows}[data-index="0"]`);
    await page.waitFor("[data-testid=piece-text]");
    check("a system block's text is the text of the record", (await page.eval(`document.querySelector("[data-testid=piece-text]").textContent`)) === facts.texts.system0);
    const promptRow = (await places(page)).findIndex((p) => p.startsWith("Message 5"));
    await page.click(rows, promptRow);
    await page.sleep(300);
    await page.until(`document.querySelector("[data-testid=piece-text]")?.textContent !== ${JSON.stringify(facts.texts.system0)}`, "the text to change");
    check("a message's text is the text of the record", (await page.eval(`document.querySelector("[data-testid=piece-text]").textContent`)) === facts.texts.promptTwo);

    // The largest pieces jump to their call and piece.
    await page.click("[data-testid=top-piece]", 0);
    const jump = await page.eval(`(() => { const r = document.querySelector("[data-testid=top-piece]"); return { step: r.dataset.step, piece: r.dataset.piece }; })()`);
    await page.waitFor(callPane(Number(jump.step) + 1));
    await page.waitFor(`${rows}[data-index="${jump.piece}"][aria-pressed=true]`);
    check("a row of the largest pieces opens its call and its piece", (await page.count(`${callPane(Number(jump.step) + 1)} ${rows}[data-index="${jump.piece}"][aria-pressed=true]`)) === 1, JSON.stringify(jump));

    // The chart's second mode and the keyboard.
    await page.click("[data-testid=chart-mode-kind]");
    await page.waitFor("[data-testid=chart][data-mode=kind]");
    check("the kind mode says its sizes are estimates", (await page.eval(`document.querySelector("[data-testid=chart-legend]").innerText`)).includes("estimates"));
    check("the mode is in the address", (await page.eval("location.hash")).includes("cm=kind"));
    await page.click("[data-testid=chart-mode-cache]");
    await page.eval(`document.querySelector("[data-testid=chart] [role=group]").focus()`);
    const before = await page.eval(`document.querySelector("[data-testid=call]").dataset.call`);
    const step = Number(before) > 1 ? -1 : 1;
    await page.eval(`document.querySelector("[data-testid=chart] [role=group]").dispatchEvent(new KeyboardEvent("keydown", { key: ${JSON.stringify(step < 0 ? "ArrowLeft" : "ArrowRight")}, bubbles: true }))`);
    await page.until(`document.querySelector("[data-testid=call]")?.dataset.call !== ${JSON.stringify(before)}`, "the arrow key to move the selection");
    check("an arrow key selects the neighbouring call", Number(await page.eval(`document.querySelector("[data-testid=call]").dataset.call`)) === Number(before) + step);

    // A reload keeps the selection.
    const hash = await page.eval("location.hash");
    const selected = await page.eval(`document.querySelector("[data-testid=call]").dataset.call`);
    await page.eval("location.reload()");
    await page.sleep(500);
    await page.waitFor("[data-testid=call]");
    check("a reload keeps the selected call", (await page.eval(`document.querySelector("[data-testid=call]").dataset.call`)) === selected, hash);

    // The subagent and the way back.
    await page.click("[data-testid=part-row]");
    await page.until(`document.querySelectorAll(${JSON.stringify(bars)}).length === 2`, "the subagent's two calls");
    check("the subagent opens with its own calls", (await page.count("[data-testid=part-row]")) === 0);
    await page.click("[data-testid=sessions-back]");
    await page.until(`document.querySelectorAll(${JSON.stringify(bars)}).length === 7`, "the session's seven calls");
    check("the way back returns to the session", (await page.count("[data-testid=part-row]")) === 1 && !(await page.eval("location.hash")).includes("pt="));

    // Korean screenshots, saved only when CM_E2E_SHOTS names a directory.
    const shoot = async (name, route, ready, theme, extra = "") => {
      await page.goto("about:blank");
      await page.goto(t.host.page(route, "ko").replace("?lang=ko", `?lang=ko&theme=${theme}`));
      await page.waitFor(ready, 20000);
      await page.sleep(600);
      if (extra) await page.eval(extra);
      await page.sleep(200);
      await page.screenshot(name);
    };
    for (const theme of ["light", "dark"]) {
      await shoot(`22-list-${theme}`, "view=sessions", "[data-testid=session-row]", theme);
      await shoot(`22-cache-${theme}`, `${base}&call=7`, `${callPane(7)} ${rows}`, theme);
      await shoot(`22-kinds-${theme}`, `${base}&call=3&cm=kind`, `${callPane(3)} ${rows}`, theme);
      await shoot(`22-text-${theme}`, `${base}&call=3&pc=s4`, "[data-testid=piece-text]", theme, `document.querySelector("[data-testid=piece-detail]")?.scrollIntoView({ block: "center" })`);
      await shoot(`22-lost-${theme}`, `${base}&call=4`, `${callPane(4)} ${rows}`, theme);
    }
  },
};
