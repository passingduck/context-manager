import type { AbsPath } from "../gen/AbsPath";
import type { AgentView } from "../gen/AgentView";
import type { ContextBlock } from "../gen/ContextBlock";
import type { Load } from "../gen/Load";
import type { Unsized } from "../gen/Unsized";

/** One file or one run of glue in the session-start prefix. `start` and `end` count tokens from the first block the scan knows. */
export interface Seg {
  /** Position in `Prefix.segs`. */
  index: number;
  block: number;
  /** Null for glue. */
  load: Load | null;
  tokens: number;
  start: number;
  end: number;
}

export interface PrefixBlock {
  block: ContextBlock;
  index: number;
  start: number;
  end: number;
}

/** Content of unknown size that the agent sends in front of a block. `at` is the token position of that block's start. */
export interface Stub {
  at: number;
  block: number;
  kind: Unsized;
}

export interface Prefix {
  blocks: PrefixBlock[];
  segs: Seg[];
  stubs: Stub[];
  total: number;
}

/** The session-start context as one line of tokens, in delivery order. Positions are prefix sums. */
export function buildPrefix(view: AgentView): Prefix {
  const blocks: PrefixBlock[] = [];
  const segs: Seg[] = [];
  const stubs: Stub[] = [];
  let at = 0;
  view.session.forEach((block, index) => {
    const blockStart = at;
    for (const kind of block.precededBy) stubs.push({ at: blockStart, block: index, kind });
    for (const part of block.parts) {
      segs.push({ index: segs.length, block: index, load: part.kind === "load" ? (view.loads[part.load] ?? null) : null, tokens: part.tokens, start: at, end: at + part.tokens });
      at += part.tokens;
    }
    blocks.push({ block, index, start: blockStart, end: at });
  });
  return { blocks, segs, stubs, total: at };
}

/** A row of the "where does each file sit" chart: one file, or the catalog lines of one block together. */
export interface PrefixRow {
  key: string;
  start: number;
  end: number;
  block: number;
  /** One entry for a file. Several for a bundle of catalog lines. */
  segs: Seg[];
}

const BUNDLE_MIN = 2;

export function buildRows(prefix: Prefix): PrefixRow[] {
  const rows: PrefixRow[] = [];
  for (const pb of prefix.blocks) {
    const files = prefix.segs.filter((seg) => seg.block === pb.index && seg.load);
    const lines = files.filter((seg) => seg.load?.kind === "listing");
    const bundle = lines.length >= BUNDLE_MIN;
    for (const seg of files) {
      if (bundle && seg.load?.kind === "listing") continue;
      rows.push({ key: `s${seg.index}`, start: seg.start, end: seg.end, block: pb.index, segs: [seg] });
    }
    if (bundle) {
      rows.push({ key: `b${pb.index}`, start: lines[0]!.start, end: lines[lines.length - 1]!.end, block: pb.index, segs: lines });
    }
  }
  return rows.sort((a, b) => a.start - b.start);
}

/** The kinds of unsized content that lie after token position `end`. A number that counts the tail is then a lower bound. */
export function unsizedAfter(prefix: Prefix, end: number): Unsized[] {
  return prefix.stubs.filter((stub) => stub.at >= end).map((stub) => stub.kind);
}

/** How many stubs lie before a token position. A stub at exactly `tokens` counts when `side` is "after". */
export function stubsBefore(prefix: Prefix, tokens: number, side: "before" | "after"): number {
  return prefix.stubs.filter((stub) => stub.at < tokens || (side === "after" && stub.at === tokens)).length;
}

/** Where a file sits in one agent's prefix, or null when it is not part of it. Counts its first segment. */
export function positionOf(view: AgentView, file: AbsPath): { seg: Seg; total: number; after: number; unsized: Unsized[] } | null {
  const prefix = buildPrefix(view);
  const seg = prefix.segs.find((s) => s.load?.file === file);
  return seg ? { seg, total: prefix.total, after: prefix.total - seg.end, unsized: unsizedAfter(prefix, seg.end) } : null;
}

/** Tick values for an axis over `total` tokens: about six steps of 1, 2 or 5 times a power of ten. */
export function ticks(total: number): number[] {
  if (total <= 0) return [];
  const raw = total / 6;
  const pow = 10 ** Math.floor(Math.log10(raw));
  const step = [1, 2, 5, 10].map((m) => m * pow).find((s) => s >= raw) ?? pow * 10;
  const out: number[] = [];
  for (let v = 0; v < total; v += step) out.push(v);
  return out;
}

export const exact = (n: number): string => n.toLocaleString("en-US");
