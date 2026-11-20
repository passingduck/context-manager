// What a revert of an unfinished change does with each file, read from its preview.
import type { AbsPath } from "../gen/AbsPath";
import type { Preview } from "../gen/Preview";

/** Where one file of an unfinished change stands on disk. */
export type FileState = "atBefore" | "atAfter" | "neither";

/**
 * A file the revert plans to write or trash is at the state after the change. A file the preview
 * lists as a conflict is at neither state. Any other file already has the state before.
 */
export function fileStates(paths: readonly AbsPath[], preview: Preview): { path: AbsPath; state: FileState }[] {
  const planned = preview.changeSet.ops.map((op) => op.path);
  const conflicted = new Set(preview.conflicts.map((c) => c.path));
  return paths.map((path) => ({
    path,
    state: conflicted.has(path) ? "neither" : planned.some((p) => p === path || p.startsWith(`${path}/`)) ? "atAfter" : "atBefore",
  }));
}
