import type { Os } from "../gen/Os";
import type { Presence } from "../gen/Presence";
import type { Shell } from "../gen/Shell";

export const en = {
  title: "context-manager",
  language: "Language",
  loading: "Loading",
  failed: "The request failed",
  session: {
    heading: "Session",
    version: "Version",
    shell: "Shell",
    readOnly: "Read only",
    launchCwd: "Project directory",
    home: "Home directory",
    os: "System",
    yes: "yes",
    no: "no",
  },
  agents: {
    heading: "Agents",
    home: "Home",
    missing: "missing",
  },
  shell: { desktop: "Desktop window", serve: "Web server" } satisfies Record<Shell, string>,
  os: { macos: "macOS", linux: "Linux", windows: "Windows", other: "Other" } satisfies Record<Os, string>,
  presence: {
    installed: "Installed",
    configOnly: "Settings found, program not found",
    absent: "Not found",
  } satisfies Record<Presence["kind"], string>,
};
