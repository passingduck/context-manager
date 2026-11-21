import type { PieceKind } from "../gen/PieceKind";
import type { PiecePlace } from "../gen/PiecePlace";
import type { Sent } from "../gen/Sent";
import type { Step } from "../gen/Step";
import type { Trace } from "../gen/Trace";
import type { Usage } from "../gen/Usage";

/** The prompt size the API reported: everything the model read, from the cache or not. */
export const promptOf = (usage: Usage): number => usage.input + usage.cacheRead + usage.cacheWrite;

/** What the numbers of a whole session say, from `Usage` only. Estimates never enter here. */
export interface Totals {
  calls: number;
  largest: number;
  output: number;
  /** Share of all prompt tokens that were read from the cache, 0 to 1. Null when no call has usage. */
  cacheShare: number | null;
}

export function totals(trace: Trace): Totals {
  let largest = 0;
  let output = 0;
  let read = 0;
  let prompt = 0;
  for (const step of trace.steps) {
    if (!step.usage) continue;
    largest = Math.max(largest, promptOf(step.usage));
    output += step.usage.output;
    read += step.usage.cacheRead;
    prompt += promptOf(step.usage);
  }
  return { calls: trace.steps.length, largest, output, cacheShare: prompt > 0 ? read / prompt : null };
}

/** Why a call read less from the cache than the call before it had read and written. Only facts of the two requests decide. */
export type Loss =
  /** The request shares fewer pieces with the previous one than that one had. `at` is the index of the first piece that differs. */
  | { kind: "changed"; at: number; shared: number; before: number }
  /** The request shares every piece of the previous one, and the cache was not there. `gapMs` is the time since the previous call. */
  | { kind: "expired"; gapMs: number | null }
  /** One of the two calls has no recorded request, so the record cannot say which. */
  | { kind: "unknown" };

export interface CacheLoss {
  /** What the previous call had read and written together. */
  expected: number;
  /** What this call read. */
  read: number;
  why: Loss;
  /** A compaction lies between this call and the previous one. */
  afterCompaction: boolean;
}

const ms = (iso: string): number | null => {
  const n = Date.parse(iso);
  return Number.isNaN(n) ? null : n;
};

/** The loss of step `index`, or null when it read at least what the step before it read and wrote together. */
export function cacheLoss(trace: Trace, index: number): CacheLoss | null {
  const cur = trace.steps[index];
  const prev = trace.steps[index - 1];
  if (!cur?.usage || !prev?.usage) return null;
  const expected = prev.usage.cacheRead + prev.usage.cacheWrite;
  if (cur.usage.cacheRead >= expected) return null;
  const afterCompaction = trace.boundaries.some((b) => b.before === index);
  return { expected, read: cur.usage.cacheRead, why: why(prev, cur), afterCompaction };
}

function why(prev: Step, cur: Step): Loss {
  if (!prev.sent || !cur.sent) return { kind: "unknown" };
  if (cur.sent.shared < prev.sent.pieces) return { kind: "changed", at: cur.sent.shared, shared: cur.sent.shared, before: prev.sent.pieces };
  const a = ms(prev.at);
  const b = ms(cur.at);
  return { kind: "expired", gapMs: a !== null && b !== null ? Math.max(0, b - a) : null };
}

/** How much the prompt grew since the call before. A difference of two real numbers, null when either call has no usage. */
export function growth(trace: Trace, index: number): number | null {
  const cur = trace.steps[index]?.usage;
  const prev = trace.steps[index - 1]?.usage;
  return cur && prev ? promptOf(cur) - promptOf(prev) : null;
}

/** Whether the reader fitted the piece sizes of a request to the prompt size the API reported. */
export const fittedOf = (sent: Sent): boolean => (sent as Sent & { fitted?: boolean }).fitted === true;

/** True when every call that has a request has fitted sizes, so the kind mode shares the scale of the real chart. */
export const allFitted = (trace: Trace): boolean => {
  const sent = trace.steps.flatMap((s) => (s.sent ? [s.sent] : []));
  return sent.length > 0 && sent.every(fittedOf);
};

/** The estimated size of a request: its tokens by kind, summed. */
export const estimateOf = (step: Step): number => (step.sent ? step.sent.byKind.reduce((n, k) => n + k.shared + k.fresh, 0) : 0);

/** One colour family per group of kinds, and a strength inside the family. A kind always also has its name in text. */
const FAMILY: Record<PieceKind, [string, number]> = {
  system: ["--c2", 100],
  toolDefinition: ["--c2", 70],
  catalog: ["--c2", 45],
  instructions: ["--c1", 100],
  environment: ["--c1", 70],
  reminder: ["--c1", 52],
  hookOutput: ["--c1", 38],
  userText: ["--c4", 100],
  command: ["--c4", 78],
  fileMention: ["--c4", 58],
  extensionBody: ["--c4", 42],
  summary: ["--c4", 30],
  toolCall: ["--c0", 100],
  toolResult: ["--c0", 62],
  modelText: ["--c3", 100],
  thinking: ["--c3", 55],
  frame: ["--fg-faint", 60],
  other: ["--fg-faint", 100],
};

export const KINDS = Object.keys(FAMILY) as PieceKind[];

export function kindColor(kind: PieceKind): string {
  const [token, strength] = FAMILY[kind];
  return strength >= 100 ? `var(${token})` : `color-mix(in srgb, var(${token}) ${strength}%, var(--bg))`;
}

/** Marks a piece the previous call already sent. Weight and pattern differ from a fresh piece, not only colour. */
export const SHARED_HATCH = "repeating-linear-gradient(135deg, color-mix(in srgb, var(--bg) 55%, transparent) 0 2px, transparent 2px 5px)";

export function placeOf(place: PiecePlace): { group: "system" | "tool" | "message"; n: number } {
  switch (place.kind) {
    case "system":
      return { group: "system", n: place.block + 1 };
    case "tool":
      return { group: "tool", n: place.index + 1 };
    case "message":
      return { group: "message", n: place.message + 1 };
  }
}

/** A piece named in the address: `s3` is the fourth piece of the request, `r0` the first piece of the reply. */
export type Picked = { side: "sent" | "reply"; piece: number };

export function parsePick(pc: string): Picked | null {
  const m = /^([sr])(\d+)$/.exec(pc);
  return m ? { side: m[1] === "s" ? "sent" : "reply", piece: Number(m[2]) } : null;
}

export const pickText = (side: Picked["side"], piece: number): string => `${side === "sent" ? "s" : "r"}${piece}`;

/** Ticks for a token axis, as `ticks` of the prefix map, over a maximum. */
export function axisTicks(max: number): number[] {
  if (max <= 0) return [];
  const raw = max / 4;
  const pow = 10 ** Math.floor(Math.log10(raw));
  const step = [1, 2, 2.5, 5, 10].map((m) => m * pow).find((s) => s >= raw) ?? pow * 10;
  const out: number[] = [];
  for (let v = step; v <= max; v += step) out.push(v);
  return out;
}
