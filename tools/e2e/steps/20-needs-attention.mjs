// A change a crash left half done shows each file's state, and a revert from the UI resolves it.
import { createHash } from "node:crypto";
import { mkdirSync, writeFileSync } from "node:fs";
import { confirmApply, open, read, records, seed, waitReview } from "../helpers.mjs";

const ID = "01JNEEDSATTENTION00000000A";
const sha = (text) => createHash("sha256").update(text).digest("hex");

/**
 * Writes the pending record docs/design.md section 4 documents: `history/changes/<id>.pending.json`
 * with a blob for each side of each file under `history/blobs/<sha256>`.
 */
function seedPending(box, ops) {
  const blobs = `${box.data}/history/blobs`;
  const changes = `${box.data}/history/changes`;
  for (const dir of [blobs, changes]) mkdirSync(dir, { recursive: true, mode: 0o700 });
  const blob = (text) => {
    writeFileSync(`${blobs}/${sha(text)}`, text, { mode: 0o600 });
    return sha(text);
  };
  const record = {
    id: ID,
    at: new Date().toISOString(),
    cwd: box.proj,
    summary: { edit: { kind: "save" }, agent: "claude", targets: ops.map((o) => o.path) },
    ops: ops.map((o) => ({ path: o.path, before: blob(o.before), after: blob(o.after) })),
    state: "pending",
  };
  writeFileSync(`${changes}/${ID}.pending.json`, JSON.stringify(record, null, 2) + "\n", { mode: 0o600 });
}

export default {
  name: "history: a change that needs attention",
  async run(t) {
    const { page, box, check } = t;
    const written = `${box.proj}/attn-a/CLAUDE.md`;
    const untouched = `${box.proj}/attn-b/CLAUDE.md`;
    // `written` is at the state after the change, `untouched` at the state before it.
    seed(box.proj, { "attn-a/CLAUDE.md": "A after the change\n", "attn-b/CLAUDE.md": "B before the change\n" });
    await t.host.stop();
    seedPending(box, [
      { path: written, before: "A before the change\n", after: "A after the change\n" },
      { path: untouched, before: "B before the change\n", after: "B after the change\n" },
    ]);
    t.host = await t.startHost();

    const listed = (await records(t)).find((r) => r.id === ID);
    check("the host classified the record as needing attention", listed?.state === "needsAttention", JSON.stringify(listed?.state));

    const route = `view=history&sel=${encodeURIComponent(`change:${ID}`)}`;
    await open(t, route, "[data-testid=attention-file]");
    await page.until(`document.querySelectorAll("[data-testid=attention-file]").length >= 2`, "both file states");
    const states = Object.fromEntries((await page.all("[data-testid=attention-file]")).map((r) => [r.text.split("\n")[0], r.fileState]));
    check("the history view shows the changed file at after", states["attn-a/CLAUDE.md"] === "atAfter" || Object.entries(states).some(([k, v]) => k.endsWith("attn-a/CLAUDE.md") && v === "atAfter"), JSON.stringify(states));
    check("the history view shows the other file at before", Object.entries(states).some(([k, v]) => k.endsWith("attn-b/CLAUDE.md") && v === "atBefore"), JSON.stringify(states));
    const row = (await page.all("[data-testid=history-row]")).find((r) => r.change === ID);
    check("the row carries the needs-attention state", row?.state === "needsAttention", JSON.stringify(row));
    await open(t, route, "[data-testid=attention-file]", "ko");
    await page.until(`document.querySelectorAll("[data-testid=attention-file]").length >= 2`, "both file states in Korean");
    await page.screenshot("history-attention-ko");

    await page.click("[data-testid=change-revert]");
    await waitReview(page);
    const plan = await page.all("[data-testid=unfinished-file]");
    check("the revert dialog says what it does for each file", plan.length === 2 && plan.some((p) => p.fileState === "atAfter") && plan.some((p) => p.fileState === "atBefore"), JSON.stringify(plan));
    await open(t, route, "[data-testid=change-revert]", "ko");
    await page.click("[data-testid=change-revert]");
    await waitReview(page);
    await page.screenshot("revert-dialog-ko");
    await open(t, route, "[data-testid=change-revert]");
    await page.click("[data-testid=change-revert]");
    await confirmApply(page);

    check("the revert restored the file the change wrote", read(written) === "A before the change\n", JSON.stringify(read(written)));
    check("the other file is as it was", read(untouched) === "B before the change\n");
    const after = (await records(t)).find((r) => r.id === ID);
    check("the record is closed", after !== undefined && after.state !== "needsAttention" && after.state !== "pending", JSON.stringify(after?.state));
  },
};
