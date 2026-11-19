// Realistic file contents for the mock project. Only the mock reads this file.

const lines = (n: number, make: (i: number) => string) => Array.from({ length: n }, (_, i) => make(i + 1)).join("\n");

export const CWD = "/Users/dev/acme-app";
export const HOME = "/Users/dev";

export const claudeUser = `# Personal preferences

- Answer in the language I write in.
- Prefer small diffs. Do not reformat files you did not change.
- Never commit to main. Open a branch and a draft pull request.
- Ask before you delete anything that is not under version control.
`;

export const claudeProject = `# acme-app

A Tauri desktop app with an Axum backend and a React 19 frontend.
@AGENTS.md

## Rules

- One owner per piece of state. Do not copy state into a second place.
- Deleting a file means moving it to the OS trash.
- Show a diff before you write. Refuse to overwrite a file that changed on disk.

## Layout

The long version lives in docs/architecture.md. Read it before you touch the backend.
The backend is Rust (axum, tokio). The frontend is React 19 with Vite.

## Commands

- pnpm dev        start the frontend dev server
- cargo test      run the core tests (no GUI library needed)
- pnpm lint       run eslint and the type check

@docs/missing-style.md
@~/shared/team-notes.md
`;

export const agentsMd = `# AGENTS.md

## Build and test

- Install: pnpm install
- Run the whole gate before you commit: scripts/check.sh
- A single crate: cargo test -p core

## Conventions

- Rust: no unwrap in library code. Return errors with context.
- TypeScript: strict mode. No any. Validate data where it enters the program.
- Commit titles look like type(scope): subject.
`;

export const claudeRuleAlways = `# Security

Never print secrets. Redact tokens in logs and in test output.
Hook scripts run with the user's rights, so treat their input as untrusted.
`;

export const claudeRuleTs = `---
paths: ["**/*.ts", "**/*.tsx"]
---

# TypeScript

- Use \`unknown\` at boundaries and narrow it with a parser.
- Prefer union types with a \`kind\` field over optional fields.
- Components are function components. No default exports.
`;

export const claudeRuleBroken = `---
paths: **/*.rs
---

# Rust

Use thiserror in libraries and anyhow in binaries.
`;

export const claudeApi = `# src/api

- Handlers stay thin. Business logic belongs in core.
- Every handler returns a typed error. No string errors cross this boundary.
- New routes need a test that calls them over HTTP.
`;

export const claudeVendor = `# vendor/lib

Vendored code. Do not edit it by hand.
`;

export const architecture = lines(
  120,
  (i) =>
    i === 1
      ? "# Architecture"
      : i % 12 === 0
        ? `\n## Part ${i / 12}`
        : `The ${["scan", "adapter", "writer", "watcher", "serve shell"][i % 5]} module owns one concern and exposes a small typed surface (line ${i}).`,
);

export const adr3 = `# ADR 3: storage

Status: accepted

We store history as one JSON record per change and keep file blobs by content hash.
See docs/architecture.md for how the writer uses it.
`;

export const memoryIndex = [
  "# Memory index",
  "",
  ...Array.from(
    { length: 212 },
    (_, i) => `- [Note ${i + 1}](notes/note-${i + 1}.md) - one line summary of note ${i + 1}`,
  ),
  "- [Debugging](debugging.md) - how to read the host logs",
].join("\n");

export const memoryDebugging = `---
name: debugging
description: how to read the host logs
---

Set RUST_LOG=debug and run the serve shell. The host logs every request with its operation name.
`;

export const memoryPrefs = `---
name: user-prefs
description: how the user likes answers formatted
---

Short answers first. Details only when asked. Use plain words.
`;

export const memoryTauri = `---
name: tauri-notes
description: quirks of the desktop shell
---

A classic script that declares a top-level isTauri fails silently inside the webview. Ship ES modules only.
`;

export const skillReview = `---
name: review-pr
description: Review a pull request for correctness and style
---

# Review a pull request

1. Read the diff before the description.
2. List the behavior changes, then check each against the tests.
3. Report problems as a short list, most serious first.
`;

export const skillMigrate = `---
name: db-migrate
description: Write and check a database migration
paths: ["db/**"]
---

# Database migrations

Write the up and down steps together. Run both against a scratch database.
`;

export const agentReviewer = `---
name: reviewer
description: Reads a change and reports bugs, not style
tools: Read, Grep, Glob
---

You review code. Report only defects that a user could hit. Cite the file and line.
`;

export const commandRelease = `---
description: Cut a release
---

Bump the version, update the changelog, tag the commit, and open the release pull request.
`;

export const codexUser = `# Codex preferences

- Keep answers short.
- Show the command you ran with its output.
`;

export const codexProject = agentsMd;

export const codexInstructions = `# Local instructions

Use the staging database for any test that writes.
`;

export const codexSkill = `---
name: lint-fix
description: Run the linters and fix what they report
---

Run pnpm lint, then fix each finding without changing behavior.
`;

export const codexMemorySummary = `# Memory summary

The user prefers short answers. The project uses pnpm and Rust.
`;

export const settingsJson = `{
  "hooks": {
    "SessionStart": [
      { "matcher": "startup|resume", "hooks": [{ "type": "command", "command": "\${CLAUDE_PLUGIN_ROOT}/hooks/session-start.sh" }] }
    ],
    "PreToolUse": [
      { "matcher": "Bash", "hooks": [{ "type": "command", "command": "./scripts/guard-bash.sh", "timeout": 10 }] }
    ]
  }
}
`;
