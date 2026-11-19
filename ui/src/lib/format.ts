import type { AbsPath } from "../gen/AbsPath";

export function tokens(n: number): string {
  if (n < 1000) return String(n);
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
