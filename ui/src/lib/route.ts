import { useCallback, useMemo, useSyncExternalStore } from "react";

export const VIEWS = ["context", "memory", "hooks", "doctor", "history"] as const;
export type View = (typeof VIEWS)[number];

/** Everything the address bar remembers. Empty strings mean "not set". */
export interface Route {
  view: View;
  /** Project directory. Empty means the directory the host started in. */
  p: string;
  /** Selection: `file:<path>`, `hook:<id>`, `change:<id>`. */
  sel: string;
  /** Agent whose load the file detail should lead with. */
  ag: string;
  /** With a file selected, the place of it the operator picked: a `loadKey` of one of the agent's loads. Empty means every place of the file. */
  at: string;
  mode: "tree" | "order";
  q: string;
  sim: string;
  /** Line to reveal in the open file. */
  ln: number;
}

function parse(hash: string): Route {
  const params = new URLSearchParams(hash.replace(/^#/, ""));
  const view = params.get("view");
  return {
    view: (VIEWS as readonly string[]).includes(view ?? "") ? (view as View) : "context",
    p: params.get("p") ?? "",
    sel: params.get("sel") ?? "",
    ag: params.get("ag") ?? "",
    at: params.get("at") ?? "",
    mode: params.get("mode") === "order" ? "order" : "tree",
    q: params.get("q") ?? "",
    sim: params.get("sim") ?? "",
    ln: Number(params.get("ln") ?? "0") || 0,
  };
}

function write(route: Route): string {
  const params = new URLSearchParams();
  for (const [key, value] of Object.entries(route)) {
    if (value === "" || value === 0 || (key === "view" && value === "context") || (key === "mode" && value === "tree"))
      continue;
    params.set(key, String(value));
  }
  return params.toString();
}

function subscribe(listener: () => void): () => void {
  window.addEventListener("hashchange", listener);
  return () => window.removeEventListener("hashchange", listener);
}

/** The route lives in `location.hash`, so reload and the back button work. A patch that selects something else without naming a place clears the place. */
export function useRoute(): [Route, (patch: Partial<Route>, replace?: boolean) => void] {
  const hash = useSyncExternalStore(subscribe, () => window.location.hash);
  const route = useMemo(() => parse(hash), [hash]);
  const go = useCallback((patch: Partial<Route>, replace = false) => {
    const place = ("sel" in patch || "ag" in patch) && !("at" in patch) ? { at: "" } : {};
    const next = write({ ...parse(window.location.hash), ...place, ...patch });
    const url = `${window.location.pathname}${window.location.search}${next ? `#${next}` : ""}`;
    if (replace) history.replaceState(null, "", url);
    else history.pushState(null, "", url);
    window.dispatchEvent(new HashChangeEvent("hashchange"));
  }, []);
  return [route, go];
}
