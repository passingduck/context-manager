// Unsaved editor text, one entry per open file. It lives outside React so it survives a switch of rows.
import { useSyncExternalStore } from "react";
import type { AbsPath } from "../gen/AbsPath";
import type { Base } from "../gen/Base";

export interface Buffer {
  /** What the disk held when the text was loaded. A save sends it as its base. */
  base: Base;
  /** The disk text that goes with `base`. */
  loaded: string;
  text: string;
}

const buffers = new Map<AbsPath, Buffer>();
const listeners = new Set<() => void>();

export const isDirty = (buffer: Buffer | undefined): boolean => buffer !== undefined && buffer.text !== buffer.loaded;

export function setBuffer(path: AbsPath, buffer: Buffer | null): void {
  if (buffer === null) buffers.delete(path);
  else buffers.set(path, buffer);
  listeners.forEach((listener) => listener());
}

export const dirtyPaths = (): AbsPath[] => [...buffers].filter(([, buffer]) => isDirty(buffer)).map(([path]) => path);

const subscribe = (listener: () => void) => {
  listeners.add(listener);
  return () => void listeners.delete(listener);
};

export function useBuffer(path: AbsPath): Buffer | undefined {
  return useSyncExternalStore(subscribe, () => buffers.get(path));
}

/** Paths with unsaved text, for the leave-the-page prompt and the row marks. */
export function useDirtyPaths(): AbsPath[] {
  const key = useSyncExternalStore(subscribe, () => dirtyPaths().join("\n"));
  return key === "" ? [] : key.split("\n");
}
