import type { AbsPath } from "../gen/AbsPath";

/** Shortens an absolute path for display. */
export type Display = (path: AbsPath) => string;

/** One message per variant of a generated union, keyed on its `kind`. A new variant fails the type check. */
export type Msgs<U extends { kind: string }> = {
  [K in U["kind"]]: (value: Extract<U, { kind: K }>, d: Display) => string;
};

/** Looks up the message for a value's variant. The one place that correlates the two. */
export function say<U extends { kind: string }>(table: Msgs<U>, value: U, d: Display): string {
  const message = table[value.kind as U["kind"]] as (value: U, d: Display) => string;
  return message(value, d);
}
