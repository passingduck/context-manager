import { en } from "./en";
import { ko } from "./ko";

export type Lang = "en" | "ko";
export type Catalog = typeof en;

export const catalogs: Record<Lang, Catalog> = { en, ko };

const KEY = "context-manager.lang";

/** Local storage first, then the browser language. */
export function detectLang(): Lang {
  try {
    const stored = localStorage.getItem(KEY);
    if (stored === "en" || stored === "ko") return stored;
  } catch {
    // Storage can be blocked.
  }
  return navigator.language.toLowerCase().startsWith("ko") ? "ko" : "en";
}

export function rememberLang(lang: Lang): void {
  try {
    localStorage.setItem(KEY, lang);
  } catch {
    // Storage can be blocked. The choice then lasts until the page reloads.
  }
}
