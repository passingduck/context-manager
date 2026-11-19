import { useCallback, useEffect, useState } from "react";

export type Theme = "light" | "dark";
const KEY = "context-manager.theme";

/** The `theme` query parameter, then local storage, then the system setting. */
function detect(): Theme {
  const asked = new URLSearchParams(window.location.search).get("theme");
  if (asked === "light" || asked === "dark") return asked;
  try {
    const stored = localStorage.getItem(KEY);
    if (stored === "light" || stored === "dark") return stored;
  } catch {
    // Storage can be blocked.
  }
  return window.matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light";
}

export function useTheme(): [Theme, () => void] {
  const [theme, setTheme] = useState<Theme>(detect);
  useEffect(() => {
    document.documentElement.dataset.theme = theme;
  }, [theme]);
  const toggle = useCallback(() => {
    setTheme((old) => {
      const next = old === "dark" ? "light" : "dark";
      try {
        localStorage.setItem(KEY, next);
      } catch {
        // Storage can be blocked. The choice then lasts until the page reloads.
      }
      return next;
    });
  }, []);
  return [theme, toggle];
}
