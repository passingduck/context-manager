// The only file that knows whether the page runs inside Tauri or behind `--serve`.
import type { ApiError } from "../gen/ApiError";
import type { Request } from "../gen/Request";

const TOKEN_KEY = "context-manager.token";

export type TransportName = "invoke" | "http";

/** `?mock=1` answers from the bundled mock snapshot instead of the host. */
export function mockEnabled(): boolean {
  return new URLSearchParams(window.location.search).get("mock") === "1";
}

export function transportName(): TransportName {
  return "__TAURI_INTERNALS__" in window ? "invoke" : "http";
}

/**
 * Serve mode: the token arrives in the URL fragment. Keep it in `sessionStorage` and take it out
 * of the address bar so it is not copied, bookmarked or shared by accident. Call once at start.
 */
export function initTransport(): void {
  if (transportName() !== "http") return;
  const match = /(?:^|[#&])token=([^&]+)/.exec(window.location.hash);
  if (!match?.[1]) return;
  try {
    sessionStorage.setItem(TOKEN_KEY, decodeURIComponent(match[1]));
  } catch {
    // Storage can be blocked. The token then lives only until the page reloads.
    memoryToken = decodeURIComponent(match[1]);
  }
  history.replaceState(null, "", window.location.pathname + window.location.search);
}

let memoryToken: string | null = null;

function token(): string | null {
  try {
    return sessionStorage.getItem(TOKEN_KEY) ?? memoryToken;
  } catch {
    return memoryToken;
  }
}

/** Sends one request. A failure is thrown as the `ApiError` the host returned. */
export async function send(request: Request): Promise<unknown> {
  if (mockEnabled()) {
    const { handle } = await import("../mock/host");
    return handle(request);
  }
  if (transportName() === "invoke") {
    const { invoke } = await import("@tauri-apps/api/core");
    return invoke("rpc", { request });
  }
  const bearer = token();
  const response = await fetch("/api/rpc", {
    method: "POST",
    headers: { "content-type": "application/json", ...(bearer ? { authorization: `Bearer ${bearer}` } : {}) },
    body: JSON.stringify(request),
  });
  if (response.ok) return response.json();
  const error: ApiError = await response.json().catch(() => ({ kind: "io", detail: `HTTP ${response.status}` }));
  throw error;
}

/** Desktop self-test only: hands the report to the host, which writes it and exits. */
export async function reportSelftest(report: unknown): Promise<void> {
  const { invoke } = await import("@tauri-apps/api/core");
  await invoke("selftest_report", { report });
}
