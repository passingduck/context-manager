// Prints a TLC counterexample of WritePipeline.tla as numbered steps in the code's words.
//
//   node trace.mjs WritePipeline.tla tlc-output.txt
//
// Each step names the instance that took it, the action's own comment from the spec, the
// code it models (its `code:` line), and what changed on disk and in the history.
import { readFileSync } from "node:fs";

const [specPath, outPath] = process.argv.slice(2);
const spec = readFileSync(specPath, "utf8").split("\n");
const out = readFileSync(outPath, "utf8").split("\n");

// Action name -> { what, code } from the comment block above its definition.
const actions = new Map();
for (let i = 0; i < spec.length; i++) {
  const m = /^([A-Z][A-Za-z]*)(\(.*\))? ==/.exec(spec[i]);
  if (!m) continue;
  const block = [];
  for (let j = i - 1; j >= 0 && spec[j].startsWith("\\*"); j--) {
    block.unshift(spec[j].replace(/^\\\*\s?/, ""));
  }
  const codeAt = block.findIndex((l) => l.trim().startsWith("code:"));
  const what = (codeAt === -1 ? block : block.slice(0, codeAt)).join(" ").trim();
  const code =
    codeAt === -1
      ? ""
      : block.slice(codeAt).join(" ").replace(/\s+/g, " ").trim().replace(/^code:\s*/, "");
  actions.set(m[1], { what, code });
}

// The states of the trace: an action name and the raw text of each variable.
const states = [];
let invariant = "";
let current = null;
let lastVar = null;
for (const line of out) {
  const inv = /^Error: Invariant (\w+) is violated/.exec(line);
  if (inv) invariant = inv[1];
  const st = /^State (\d+): <(\w+)/.exec(line);
  if (st) {
    current = { action: st[2], vars: {} };
    states.push(current);
    lastVar = null;
    continue;
  }
  if (!current) continue;
  const v = /^\/\\ (\w+) = (.*)$/.exec(line);
  if (v) {
    lastVar = v[1];
    current.vars[lastVar] = v[2];
  } else if (lastVar && /^\s+\S/.test(line)) {
    current.vars[lastVar] += " " + line.trim();
  } else if (line.trim() === "") {
    lastVar = null;
  }
}

if (states.length === 0) {
  console.log("      (no trace in the TLC output)");
  process.exit(0);
}

// `(p1 :> "a" @@ p2 :> "b")` -> { p1: "a", p2: "b" }
const perProc = (text) => {
  const map = {};
  for (const m of (text ?? "").matchAll(/(\w+) :> ("[^"]*"|\w+)/g)) map[m[1]] = m[2].replace(/"/g, "");
  return map;
};

const shown = ["disk", "temps", "pend", "closed", "blobs", "lock"];
console.log(`      Trace: ${states.length - 1} steps to a state that breaks ${invariant}.`);
console.log(`      0. start: disk = ${states[0].vars.disk}`);
for (let n = 1; n < states.length; n++) {
  const before = states[n - 1].vars;
  const after = states[n].vars;
  const pcA = perProc(before.pc);
  const pcB = perProc(after.pc);
  let who = Object.keys(pcB).find((p) => pcA[p] !== pcB[p]);
  const name = states[n].action;
  if (name === "OutsideEdit") who = "operator";
  else if (name === "DayPasses") who = "clock";
  else if (!who) {
    const actA = perProc(before.act);
    who = Object.keys(perProc(after.pc)).find((p) => before.act !== after.act && actA[p] !== perProc(after.act)[p]) ?? "?";
  }
  const info = actions.get(name) ?? { what: "", code: "" };
  console.log(`      ${n}. ${who} ${name}: ${info.what}`);
  if (info.code) console.log(`         code: ${info.code}`);
  for (const v of shown) {
    if (before[v] !== after[v]) console.log(`         ${v}: ${after[v]}`);
  }
}
