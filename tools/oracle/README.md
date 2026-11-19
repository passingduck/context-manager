# Oracle scripts

These scripts ask a real CLI what it loads, so the scanner can be compared with ground truth. They were written during the research that produced `docs/agents/*.md` and are kept as they were. Their paths still point at the research scratch directory. Parametrize a script before you wire it into a test.

## Codex (`codex/`)

- `probe_codex.py` builds a throwaway `HOME`, `CODEX_HOME` and git repository per case, runs `codex debug prompt-input`, and asserts on the model-visible prompt. It runs offline and needs no login. 44 of 44 checks passed against `codex-cli 0.165.0` (`probe_results.txt`).
- `probe_hooks.py` starts `codex app-server` over stdio and calls `hooks/list`, which returns each hook's key, source, trust status, enabled flag and current hash.

## Claude Code (`claude/`)

- `exp.sh <scenario>` builds a fixture tree and runs `claude -p` with `CLAUDE_CONFIG_DIR` pointed at a fixture config directory, a dummy API key, and `ANTHROPIC_BASE_URL` pointed at a local sink. No request leaves the machine and nothing under `~/.claude` is touched.
- `capture_server.py` is the sink. It saves each Messages API request body.
- `fake_model.py` replaces the sink when a scenario needs Claude to call `Read`, which is how on-demand loading is observed.
- `batch.sh` reruns every scenario. `analyze.py` and `dump.py` read the captured requests.
- `project_key.py <abs-path>` is a tested port of how Claude Code names a project's memory directory.
- `bgrep.py` searches the Claude Code binary for a string.
- `hooks/validate_hooks.py` lints a hooks file against the 33 events, the handler types and their fields.

Claude Code refuses to load instruction files from a path shaped like a session scratchpad, so `exp.sh` must build its fixtures elsewhere.

## Gemini CLI

No script. The CLI is not installed on the machine the specs were written on. Its rules were read from the source at tag `v0.69.0`.
