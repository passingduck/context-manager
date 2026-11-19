import { keepPreviousData, useQuery, useQueryClient } from "@tanstack/react-query";
import { useEffect, useRef } from "react";
import type { AbsPath } from "../gen/AbsPath";
import type { AgentId as Agent } from "../gen/AgentId";
import { call } from "./client";

// Query keys follow docs/design.md section 10: ['scan', cwd], ['file', path, rev], ['history', cwd].

export const useSession = () => useQuery({ queryKey: ["session"], queryFn: () => call("session", null), staleTime: Infinity });
export const useAgents = () => useQuery({ queryKey: ["agents"], queryFn: () => call("agents", null), staleTime: Infinity });
export const useProjects = () => useQuery({ queryKey: ["projects"], queryFn: () => call("projects", null), staleTime: 60_000 });

export const useScan = (cwd: AbsPath | null) =>
  useQuery({
    queryKey: ["scan", cwd],
    queryFn: () => call("scan", { cwd: cwd as AbsPath }),
    enabled: cwd !== null,
    placeholderData: keepPreviousData,
  });

export const useFile = (cwd: AbsPath, path: AbsPath | null, rev: string) =>
  useQuery({
    queryKey: ["file", path, rev],
    queryFn: () => call("read_file", { cwd, path: path as AbsPath }),
    enabled: path !== null,
  });

export const useRender = (cwd: AbsPath, agent: Agent, fingerprint: string, enabled: boolean) =>
  useQuery({
    queryKey: ["render", cwd, agent, fingerprint],
    queryFn: () => call("render", { cwd, agent }),
    enabled,
    placeholderData: keepPreviousData,
  });

export const useSearch = (cwd: AbsPath, query: string, regex: boolean, caseSensitive: boolean) =>
  useQuery({
    queryKey: ["search", cwd, query, regex, caseSensitive],
    queryFn: () => call("search", { cwd, query, regex, caseSensitive }),
    enabled: query.trim().length > 0,
    placeholderData: keepPreviousData,
  });

export const useSimulate = (cwd: AbsPath, path: string, fingerprint: string) =>
  useQuery({
    queryKey: ["simulate", cwd, path, fingerprint],
    queryFn: () => call("simulate", { cwd, path }),
    enabled: path.trim().length > 0,
    placeholderData: keepPreviousData,
  });

export const useHistory = (cwd: AbsPath) =>
  useQuery({ queryKey: ["history", cwd], queryFn: () => call("history", { cwd, before: null, limit: 50 }) });

export const useChange = (id: string | null) =>
  useQuery({ queryKey: ["change", id], queryFn: () => call("change", { id: id as string }), enabled: id !== null });

/**
 * One long-poll loop per open project. It refetches the scan when the fingerprint changes and
 * waits while the page is hidden.
 */
export function useWatch(cwd: AbsPath | null, fingerprint: string | undefined): void {
  const client = useQueryClient();
  const latest = useRef(fingerprint);
  latest.current = fingerprint;
  const ready = fingerprint !== undefined;

  useEffect(() => {
    if (cwd === null || !ready) return;
    let stopped = false;
    const visible = () =>
      document.hidden
        ? new Promise<void>((resolve) => {
            const onChange = () => {
              if (!document.hidden) {
                document.removeEventListener("visibilitychange", onChange);
                resolve();
              }
            };
            document.addEventListener("visibilitychange", onChange);
          })
        : Promise.resolve();

    void (async () => {
      while (!stopped) {
        await visible();
        if (stopped) return;
        const since = latest.current;
        if (since === undefined) return;
        try {
          const started = Date.now();
          const next = await call("watch", { cwd, since });
          if (stopped) return;
          if (next !== since) {
            await client.invalidateQueries({ queryKey: ["scan", cwd] });
            await client.invalidateQueries({ queryKey: ["file"] });
          } else if (Date.now() - started < 500) {
            await new Promise((resolve) => setTimeout(resolve, 1000));
          }
        } catch {
          // The host may be restarting. Wait before the next poll.
          await new Promise((resolve) => setTimeout(resolve, 2000));
        }
      }
    })();
    return () => {
      stopped = true;
    };
  }, [cwd, ready, client]);
}

/** What a revert of this change would find: files edited since the change show up as conflicts. Nothing is written. */
export const useRevertCheck = (cwd: AbsPath, id: string, fingerprint: string) =>
  useQuery({ queryKey: ["revert-check", cwd, id, fingerprint], queryFn: () => call("plan", { cwd, edit: { kind: "revert", change: id } }) });
