import { createContext, useCallback, useContext, useMemo, useState, type ReactNode } from "react";
import type { AbsPath } from "../gen/AbsPath";
import { shortPath } from "../lib/format";
import { en } from "./en";
import { ko } from "./ko";
import type { Display } from "./types";

export type Lang = "en" | "ko";
export type Catalog = typeof en;
export const catalogs: Record<Lang, Catalog> = { en, ko };

const KEY = "context-manager.lang";

/** The `lang` query parameter, then local storage, then the browser language. */
export function detectLang(): Lang {
  const asked = new URLSearchParams(window.location.search).get("lang");
  if (asked === "en" || asked === "ko") return asked;
  try {
    const stored = localStorage.getItem(KEY);
    if (stored === "en" || stored === "ko") return stored;
  } catch {
    // Storage can be blocked.
  }
  return navigator.language.toLowerCase().startsWith("ko") ? "ko" : "en";
}

function rememberLang(lang: Lang): void {
  try {
    localStorage.setItem(KEY, lang);
  } catch {
    // Storage can be blocked. The choice then lasts until the page reloads.
  }
}

interface I18n {
  lang: Lang;
  t: Catalog;
  setLang: (lang: Lang) => void;
  /** Shortens an absolute path with the project and home directories of the open session. */
  d: Display;
  setRoots: (cwd: AbsPath, home: AbsPath) => void;
}

const Context = createContext<I18n | null>(null);

export function I18nProvider({ children }: { children: ReactNode }) {
  const [lang, setLangState] = useState<Lang>(detectLang);
  const [roots, setRoots] = useState<{ cwd: AbsPath; home: AbsPath }>({ cwd: "", home: "" });
  const setLang = useCallback((next: Lang) => {
    setLangState(next);
    rememberLang(next);
    document.documentElement.lang = next;
  }, []);
  const setRootsStable = useCallback((cwd: AbsPath, home: AbsPath) => {
    setRoots((old) => (old.cwd === cwd && old.home === home ? old : { cwd, home }));
  }, []);
  const value = useMemo<I18n>(
    () => ({
      lang,
      t: catalogs[lang],
      setLang,
      d: (path) => (roots.cwd ? shortPath(path, roots.cwd, roots.home) : path),
      setRoots: setRootsStable,
    }),
    [lang, roots, setLang, setRootsStable],
  );
  return <Context.Provider value={value}>{children}</Context.Provider>;
}

export function useI18n(): I18n {
  const value = useContext(Context);
  if (!value) throw new Error("I18nProvider is missing");
  return value;
}
