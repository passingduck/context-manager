import type { AbsPath } from "../gen/AbsPath";

export function tokens(n: number): string {
  if (n < 1000) return String(n);
  if (n >= 1_000_000) return `${(Math.round(n / 100_000) / 10).toFixed(1)}M`;
  const k = n / 1000;
  return `${k >= 10 ? Math.round(k) : (Math.round(k * 10) / 10).toFixed(1)}k`;
}

export function bytes(n: number): string {
  if (n < 1024) return `${n} B`;
  const k = n / 1024;
  if (k < 1024) return `${k >= 10 ? Math.round(k) : (Math.round(k * 10) / 10).toFixed(1)} KB`;
  return `${(k / 1024).toFixed(1)} MB`;
}

/** Shortens an absolute path to `./relative` under the project and `~/` under the home directory. */
export function shortPath(path: AbsPath, cwd: AbsPath, home: AbsPath): string {
  if (path === cwd) return ".";
  if (path.startsWith(`${cwd}/`)) return `./${path.slice(cwd.length + 1)}`;
  if (path === home) return "~";
  if (path.startsWith(`${home}/`)) return `~/${path.slice(home.length + 1)}`;
  return path;
}

export function when(iso: string, lang: string): string {
  const date = new Date(iso);
  if (Number.isNaN(date.getTime())) return iso;
  return date.toLocaleString(lang === "ko" ? "ko-KR" : "en-US", {
    month: "short",
    day: "numeric",
    hour: "2-digit",
    minute: "2-digit",
  });
}

/** The clock time of a call, with seconds. */
export function clock(iso: string, lang: string): string {
  const date = new Date(iso);
  if (Number.isNaN(date.getTime())) return iso;
  return date.toLocaleTimeString(lang === "ko" ? "ko-KR" : "en-US", { hour: "2-digit", minute: "2-digit", second: "2-digit" });
}

/** Whole seconds between two RFC 3339 times, or null when either does not parse. */
export function secondsBetween(from: string | null, to: string | null): number | null {
  const a = from ? Date.parse(from) : NaN;
  const b = to ? Date.parse(to) : NaN;
  return Number.isNaN(a) || Number.isNaN(b) ? null : Math.max(0, Math.round((b - a) / 1000));
}
