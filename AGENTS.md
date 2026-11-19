# context-manager

`context-manager` is one binary that shows what Claude Code, Codex CLI and Gemini CLI put in front of the model for a project directory, and lets the operator edit it. It opens a Tauri window by default and serves the same UI over HTTP with `--serve`.

Read these before you change code:

- `docs/design.md` is the contract: the shape, the core types, the write pipeline, the build sequence.
- `docs/agents/claude-code.md`, `docs/agents/codex.md` and `docs/agents/gemini-cli.md` specify the rules each adapter implements. Each rule has evidence and a status. Cite the section you implement in the code that implements it.

## Layout

```
crates/cm-core/   reads only: model, API table, engines, the three adapters, scan, plan
crates/cm-host/   effects: apply, history, recovery, parked hooks, hook runner, watch
crates/app/       the binary: CLI, serve shell (axum), desktop shell (Tauri, feature `desktop`)
ui/               Vite, React, TypeScript, Tailwind, CodeMirror 6
docs/agents/      the three specs
fixtures/         agent homes and projects for tests
tools/oracle/     scripts that ask the real CLIs what they load
scripts/check.sh  the gate
```

## Commands

Run `scripts/check.sh` before you hand back or commit. It runs the formatter, clippy, every Rust test, the binding drift check, the TypeScript type check and the UI build. The scripts add Homebrew's rustup directory to `PATH` when `cargo` is not on it.

Use pnpm for the UI. Run a single crate's tests with `cargo test -p cm-core`.

## Rules that hold everywhere

1. **One writer.** `cm-core` contains no code that writes a file. `cm-host::apply` is the only code that writes agent files. Every mutation is an `Edit` that becomes a `ChangeSet`.
2. **One reader.** An adapter reads the disk only through `ProbeFs` and the environment only through `ScanEnv`. It never calls `std::fs` or `std::env` directly.
3. **Facts, not sentences.** The backend returns typed values. It never returns a sentence for the user to read. Explanations of agent rules live in the adapter's `rules.toml`, in English and Korean.
4. **No agent-specific code in `ui/`.** The UI renders `AgentInfo`, `Load`, `Hook`, `Extension`, `Slot` and `Diagnostic`. If the UI needs to know an agent's name to do something, the model is missing a field.
5. **Types cross the boundary by generation.** Never edit `ui/src/gen/` by hand. Change the Rust type and regenerate.
6. **Never approve on the user's behalf.** Do not write `trusted_hash`, trust files, or acknowledgement files. The specs list what is read-only and why.
7. **Tests never touch a real agent home.** Build a fixture home in a temp directory and point `ScanEnv` at it. A test that reads `~/.claude`, `~/.codex` or `~/.gemini` is wrong.
8. **ES modules only in the UI.** Detect Tauri with `'__TAURI_INTERNALS__' in window`. Never declare a top-level `isTauri`.
9. **Record deviations.** If the code must differ from `docs/design.md`, add a line to its Deviations section in the same commit and say so when you hand back.

## Tests

Test what a caller can observe: the snapshot a scan returns for a fixture tree, the bytes on disk after an apply, the HTTP status of a request. Derive the expected value from the spec, not from the code under test.

Where a real CLI can answer, compare with it. `tools/oracle/` holds the scripts. An oracle test skips when the CLI is not installed and says so in its output.

## Text

Code, comments, commit messages and docs are in English. UI strings exist in English and Korean.

Korean UI strings and `rules.toml` entries are full sentences with a subject and a verb. They use ordinary words and no symbols such as arrows or middle dots for a relation. A program stores, reads, sends, returns and runs. It does not know, want or remember.

## Commits

One verified unit per commit. Title: `type(scope): subject`.
