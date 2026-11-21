// A made-up session for developing the sessions view. Only the mock host reads this file.
import type { KindTotal } from "../gen/KindTotal";
import type { PieceKind } from "../gen/PieceKind";
import type { PieceText } from "../gen/PieceText";
import type { Step } from "../gen/Step";
import type { StepPieces } from "../gen/StepPieces";
import type { Trace } from "../gen/Trace";
import type { TraceInfo } from "../gen/TraceInfo";
import type { TracePiece } from "../gen/TracePiece";
import type { TraceReq } from "../gen/TraceReq";
import { CWD } from "./texts";

const AGENT = "claude";
const ID = "0f8e1c52-4f6e-4e0a-9d3e-0c5a2b7a9d11";
const PART = "a7c1";
const READ = 420_000;
const START = Date.parse("2026-11-20T09:00:00Z");

/** A piece with the text it stands for. Two pieces are equal when their signatures are. */
interface Spec {
  sig: string;
  piece: TracePiece;
  text: () => string;
}

const make = (sig: string, kind: PieceKind, tokens: number, place: TracePiece["place"], label: string | null = null, file: string | null = null): Spec => {
  const text = () => `${sig}\n${"x".repeat(Math.max(0, tokens * 4 - sig.length))}`;
  return { sig, piece: { place, kind, label, file, bytes: tokens * 4 + 1, tokens, head: `${sig}\n${"x".repeat(100)}`.slice(0, 160) }, text };
};

/** The request of call `i`: system blocks, tool definitions, then a conversation that grows. A changed system block at call 5 breaks the cache. */
function request(i: number, part: boolean): Spec[] {
  const out: Spec[] = [
    make(`system-1${part ? "-sub" : ""}`, "system", 120, { kind: "system", block: 0 }),
    make(i === 5 ? "system-2-changed" : "system-2", "instructions", 900, { kind: "system", block: 1 }, "CLAUDE.md", `${CWD}/CLAUDE.md`),
  ];
  ["Read", "Edit", "Bash", "Grep"].forEach((name, index) => out.push(make(`tool-${name}`, "toolDefinition", 400 + index * 60, { kind: "tool", index }, name)));
  let m = 0;
  out.push(make("environment", "environment", 150, { kind: "message", message: m, role: "user", block: 0 }, null));
  out.push(make("prompt-1", "userText", 60, { kind: "message", message: m++, role: "user", block: 1 }));
  for (let turn = 1; turn <= i; turn++) {
    out.push(make(`reply-${turn}`, "modelText", 80, { kind: "message", message: m, role: "model", block: 0 }));
    out.push(make(`call-${turn}`, "toolCall", 40, { kind: "message", message: m++, role: "model", block: 1 }, "Read"));
    out.push(make(`result-${turn}`, "toolResult", 300 + turn * 120, { kind: "message", message: m++, role: "user", block: 0 }, "Read"));
  }
  return out;
}

function stepOf(i: number, part: boolean, before: Spec[] | null): { step: Step; specs: Spec[] } {
  const specs = request(i, part);
  let shared = 0;
  if (before) while (shared < specs.length && shared < before.length && specs[shared]!.sig === before[shared]!.sig) shared++;
  const kinds = new Map<PieceKind, KindTotal>();
  specs.forEach((s, index) => {
    const total = kinds.get(s.piece.kind) ?? { kind: s.piece.kind, shared: 0, fresh: 0 };
    if (index < shared) total.shared += s.piece.tokens;
    else total.fresh += s.piece.tokens;
    kinds.set(s.piece.kind, total);
  });
  const tokens = specs.reduce((n, s) => n + s.piece.tokens, 0);
  const sharedTokens = specs.slice(0, shared).reduce((n, s) => n + s.piece.tokens, 0);
  const noUsage = !part && i === 3;
  const step: Step = {
    at: new Date(START + (part ? 40_000 : 0) + i * 45_000 + (i >= 8 ? 600_000 : 0)).toISOString(),
    model: "mock-model-1",
    usage: noUsage ? null : { input: Math.max(40, tokens - sharedTokens - 500), cacheRead: sharedTokens, cacheWrite: Math.min(500, tokens - sharedTokens), output: 150 + i * 20, thinking: null },
    sent: { exact: i !== 2, pieces: specs.length, shared, byKind: [...kinds.values()], marks: [1, specs.length - 1], fitted: part || i !== 2 } as Step["sent"],
    reply: [
      { kind: "modelText", label: null, bytes: 320, tokens: 80 },
      { kind: "toolCall", label: "Read", bytes: 160, tokens: 40 },
    ],
  };
  if (i === 0) step.sent = { ...step.sent!, shared: 0 };
  return { step, specs };
}

const cache = new Map<string, { trace: Trace; specs: Spec[][] }>();

function build(part: boolean): { trace: Trace; specs: Spec[][] } {
  const hit = cache.get(String(part));
  if (hit) return hit;
  const steps: Step[] = [];
  const allSpecs: Spec[][] = [];
  // `?calls=800` makes a long session, to try the view at scale.
  const count = part ? 4 : Number(new URLSearchParams(window.location.search).get("calls")) || 12;
  for (let i = 0; i < count; i++) {
    const { step, specs } = stepOf(i, part, allSpecs[i - 1] ?? null);
    steps.push(step);
    allSpecs.push(specs);
  }
  // A call with no recorded request.
  if (!part) steps[6] = { ...steps[6]!, sent: null };
  const top = allSpecs
    .flatMap((specs, step) => specs.map((s, piece) => ({ step, piece, kind: s.piece.kind, label: s.piece.label, tokens: s.piece.tokens })))
    .sort((a, b) => b.tokens - a.tokens)
    .slice(0, 30);
  const trace: Trace = {
    info: info(part),
    read: READ,
    steps,
    boundaries: part ? [] : [{ before: 8, preTokens: 9100, postTokens: 2400 }],
    parts: part ? [] : [{ part: PART, label: "explorer", description: "Look for the cache code", bytes: 18_400 }],
    top,
    problems: part ? [] : [{ kind: "noRequests", steps: 1 }, { kind: "inexact", steps: 1 }],
  };
  const built = { trace, specs: allSpecs };
  cache.set(String(part), built);
  return built;
}

const info = (part: boolean): TraceInfo => ({ agent: AGENT, id: ID, file: `${CWD}/.mock/${ID}.jsonl`, bytes: part ? 18_400 : 420_000, title: part ? "explorer" : "Fix the cache regression", started: new Date(START).toISOString(), last: new Date(START + 1_500_000).toISOString(), cwd: CWD });

export const traceList = (): TraceInfo[] => [info(false)];

export const traceOf = (req: TraceReq): Trace | null => (req.key.id === ID ? build(req.key.part === PART).trace : null);

export function stepPieces(req: TraceReq & { read: number; step: number }): StepPieces {
  const built = req.key.id === ID ? build(req.key.part === PART) : null;
  const specs = built?.specs[req.step];
  return req.read !== READ || !specs ? { kind: "stale" } : { kind: "current", pieces: specs.map((s) => s.piece) };
}

export function pieceText(req: TraceReq & { read: number; step: number; piece: { kind: "sent" | "reply"; piece: number } }): PieceText {
  const built = req.key.id === ID ? build(req.key.part === PART) : null;
  const spec = req.piece.kind === "sent" ? built?.specs[req.step]?.[req.piece.piece] : undefined;
  if (req.read !== READ || !spec) return { kind: "stale" };
  const text = spec.text();
  return { kind: "current", text, bytes: text.length, cut: false };
}
