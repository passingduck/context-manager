# context-manager

[한국어](README.ko.md)

`context-manager` is one binary that shows what Claude Code, Codex CLI and Gemini CLI put in front of the model for a project directory: which instruction files load, in what order, how many tokens they cost, and what is cut off. It also lets you edit those files, hooks and memories, with a preview before every write and a history you can revert.

## Build and install

You need Rust (stable, edition 2024), Node.js and pnpm. The script builds the UI, builds a release binary and copies it to `~/.local/bin`, or to the directory in `PREFIX`. It never uses `sudo`.

```sh
scripts/install.sh               # desktop build: a window, and --serve as well
scripts/install.sh --headless    # no window; only --serve works (about 13.5 MB)
PREFIX=$HOME/bin scripts/install.sh --headless
```

The headless build needs no GUI libraries, so it is the one to put on a Linux server. To check that the code still compiles for Linux, run `scripts/check-linux.sh`. To run every check the project has, run `scripts/check.sh`. To drive the real UI in headless Chrome against a release build on a throwaway home, run `scripts/e2e.sh`. It needs Node 24 and Chrome or Chromium, and it is not part of `check.sh`.

## Run

```sh
context-manager                       # open the window for the current directory
context-manager --path ~/work/app     # a different project directory
context-manager --serve               # serve the same UI over HTTP on 127.0.0.1:7457
context-manager --serve --port 0      # pick a free port
context-manager --serve --host 0.0.0.0 --port 8080
context-manager --read-only           # refuse every write and every hook run
```

`--serve` prints a URL that carries a token:

```
http://127.0.0.1:7457/#token=3f9c...
```

Open that URL. The token sits in the URL fragment, which a browser never sends to a server. The page keeps it for the session and removes it from the address bar. Every API request needs it. To use the same token on every start, set `CONTEXT_MANAGER_TOKEN`:

```sh
CONTEXT_MANAGER_TOKEN=choose-a-long-secret context-manager --serve
```

The server speaks plain HTTP, and a client that holds the token can run hooks on the machine. Bind it to loopback, which is the default, and reach a remote host through an SSH tunnel:

```sh
ssh -L 7457:127.0.0.1:7457 user@server       # on your laptop
context-manager --serve --path ~/work/app    # on the server
```

Then open the printed URL on your laptop. `--host` with a non-loopback address prints a warning, because anyone who can reach that address and has the token can run commands there.

## What it shows and edits

For each agent it finds in the project and your home directory, it shows:

- **Context.** Every instruction file, rule and import, grouped by when it loads: at session start, when the agent touches a file, when you invoke it, or only if the agent chooses to read it. It shows the literal session-start text, the token budget as a sum, and the files that were skipped, shadowed or truncated, with the reason.
- **Memory.** The memory stores and their entries.
- **Hooks.** Every hook with its event, matcher, handler and state, an on and off switch, a form for a new hook, and a test run.
- **Extensions.** Skills, commands and subagents.
- **Doctor.** Problems such as a hook program that does not exist, a settings file that does not parse, or a link to a file that does not exist.
- **History.** Every change the tool made, with a diff and a revert.
- **Search and the path simulator.** Search across what the agents load, and a question of the form "which files load if the agent opens this path".

You can add, edit and delete any file the agents read, except files the agents or a plugin manage themselves. Those open read-only with the reason.

## How a write is protected

1. **Preview.** Every edit is planned first. You see the exact diff and every warning before anything is written. The write applies only if the plan is still the one you previewed.
2. **Conflict.** If the file changed on disk after you opened it, the save becomes a conflict that shows the current text. It never overwrites.
3. **Trash.** A deleted file goes to the operating system trash. Where no trash exists, such as a Linux server without a desktop, a file or directory whose content the history holds completely is removed and the result lists it as not trashed. A directory the history cannot hold completely, such as one with a binary file, is not removed, and the operation fails. Set `CONTEXT_MANAGER_NO_OS_TRASH` before the program starts to make it treat the OS trash as not available. This switch never deletes anything that a revert cannot bring back.
4. **History.** Before a change applies, the tool stores the old content. A change that was interrupted is finished or reported when the tool starts.
5. **Revert.** A history entry reverts as a new change with its own preview. A deleted file comes back from the history copy, so emptying the trash does not break it.

The tool only writes through this path, and it never writes an approval for you: no trust hash, no trust file and no acknowledgement file.

## What switching a hook off does

| Agent | What the tool writes |
|---|---|
| Codex CLI | `enabled = false` under `[hooks.state."<key>"]` in your user `config.toml`. Switching it on removes that key. |
| Gemini CLI | The hook's name, or its command when it has no name, in `hooksConfig.disabled` in `settings.json`. One entry can switch off several hooks, and the preview says how many. |
| Claude Code | Claude Code has no switch. The tool removes the handler from the settings file and stores it in its own data directory. The hook stays in the list as switched off, and switching it on puts it back. |

Hooks that come from a plugin or from managed settings cannot be switched, because an update overwrites them.

## Where the tool keeps its data

In the platform data directory: `~/Library/Application Support/context-manager` on macOS and `$XDG_DATA_HOME/context-manager` (usually `~/.local/share/context-manager`) on Linux. Set `CONTEXT_MANAGER_DATA_DIR` to use another directory. It holds `history/changes` (one JSON file per change), `history/blobs` (the stored file contents) and `parked/` (Claude Code hooks you switched off). The directory is private to your user. The tool stores nothing else, and a scan reads only the file system and the environment.

## What is verified and what is not

- **Codex CLI.** Checked against the installed CLI. The session-start text matches `codex debug prompt-input` byte for byte, and the hook list and hook hashes match `hooks/list`.
- **Claude Code.** Checked against the installed binary, run against a local sink in seven scenarios. After each planned settings change, `claude doctor` accepts the file.
- **Gemini CLI.** Never run. Its rules come from the source at tag v0.69.0, and the adapter's tests check them against fixtures only.
- **The write flows.** Driven through the real UI in headless Chrome on a throwaway home directory, never on a real agent home.
- **Token counts.** Estimates made with the o200k tokenizer. Claude and Gemini use other tokenizers, so their real counts differ.
- **Linux.** The headless build is only compile-checked with `scripts/check-linux.sh`. It was never run on Linux, and the trash behavior on a headless Linux host is untested.
