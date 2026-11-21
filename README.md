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

The headless build needs no GUI libraries, so it is the one to put on a Linux server. To run clippy for Linux with warnings denied, run `scripts/check-linux.sh`. To run every check the project has, run `scripts/check.sh`, which also runs the Linux step when that target is installed. To drive the real UI in headless Chrome against a release build on a throwaway home, run `scripts/e2e.sh`. It needs Node 24 and Chrome or Chromium, and it is not part of `check.sh`.

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

The server speaks plain HTTP, and a client that holds the token can run hooks on the machine. It can also read the recorded sessions of the project, which hold everything that was typed and every tool output. Bind it to loopback, which is the default, and reach a remote host through an SSH tunnel:

```sh
ssh -L 7457:127.0.0.1:7457 user@server       # on your laptop
context-manager --serve --path ~/work/app    # on the server
```

Then open the printed URL on your laptop. `--host` with a non-loopback address prints a warning, because anyone who can reach that address and has the token can run commands there.

## What it shows and edits

For each agent it finds in the project and your home directory, it shows:

- **Context.** Every instruction file, rule and import, grouped by when it loads: at session start, when the agent touches a file, when you invoke it, or only if the agent chooses to read it. It shows the literal session-start text, a bar of where each file sits in that text, the token budget as a sum, and the files that were skipped, shadowed or truncated, with the reason. A file that loads in more than one place, such as a skill body that an instruction file also imports, shows each place with the text the model receives there.
- **Memory.** The memory stores and their entries.
- **Hooks.** Every hook with its event, matcher, handler and state, an on and off switch, a form for a new hook, and a test run.
- **Extensions.** Skills, commands and subagents.
- **Doctor.** Problems such as a hook program that does not exist, a settings file that does not parse, a link to a file that does not exist, or a skill whose body an import also loads in every session.
- **History.** Every change the tool made, with a diff and a revert.
- **Sessions.** What the model received in a session that Claude Code recorded. One bar for each call of the model shows how much of the prompt was read from the cache, written to it, or sent uncached, as the API reported it, with a mark at each compaction. A call that read less from the cache than the one before says why: the first piece of the request that changed, or how long the cache had been idle. A selected call lists the pieces of its request in the order the model received them (system blocks, tool definitions, instruction files, the listing of skills, your text, the model's replies and thinking, tool calls and their output), each with its size, and a piece opens its text. The view also ranks the largest pieces and lists the session's subagents, each with its own calls.
- **Search and the path simulator.** Search across what the agents load, and a question of the form "which files load if the agent opens this path".

You can add, edit and delete any file the agents read, except files the agents or a plugin manage themselves. Those open read-only with the reason.

## How a write is protected

1. **Preview.** Every edit is planned first. You see the exact diff and every warning before anything is written. The write applies only if the plan is still the one you previewed.
2. **Conflict.** If the file changed on disk after you opened it, the save becomes a conflict that shows the current text. It never overwrites.
3. **Trash.** A deleted file goes to the operating system trash. Where no trash exists, such as a Linux server without a desktop, a file or directory whose content the history holds completely is removed and the result lists it as not trashed. A directory the history cannot hold completely, such as one with a binary file, is not removed, and the operation fails. Set `CONTEXT_MANAGER_NO_OS_TRASH` before the program starts to make it treat the OS trash as not available. This switch never deletes anything that a revert cannot bring back.
4. **History.** Before a change applies, the tool stores the old content. A change that fails is undone, and its record says that it was cancelled. A change that was interrupted, for example by a crash, is never finished later without you. When the tool starts, it only looks at the files. If every file holds its new text, it marks the change complete. If every file holds its old text, it marks the change cancelled. Otherwise it lists the change as unfinished, and you revert it or apply the edit again.
5. **Revert.** A history entry reverts as a new change with its own preview. A deleted file comes back from the history copy, so emptying the trash does not break it.

The tool only writes through this path, and it never writes an approval for you: no trust hash, no trust file and no acknowledgement file.

Three cases behave differently:

- A path that is a symbolic link to a file that does not exist is refused.
- A file with more than one hard link is written in place, so that every name keeps the change. That write is not atomic, and the preview warns about it.
- Two instances of the tool can share one data directory. A lock keeps one instance from closing a change that the other is still writing.

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
- **Claude Code.** Checked against the installed binary, run against a local sink in ten scenarios. After each planned settings change, `claude doctor` accepts the file.
- **Gemini CLI.** Never run. Its rules come from the source at tag v0.69.0, and the adapter's tests check them against fixtures only.
- **The write flows.** Driven through the real UI in headless Chrome on a throwaway home directory, never on a real agent home.
- **Recorded sessions.** Claude Code records every request from version 2.1.302. A test lets the installed CLI write nine sessions against a scripted local endpoint and compares each request rebuilt from the record with what the endpoint received. On the machine this was built on, every recorded request rebuilt to the digest its record carries. The total of a request is what the API reported. The size of one piece is an estimate fitted to those totals, and the view labels it so.
- **The write pipeline.** Besides its tests, a TLA+ model of apply, undo, recovery, revert and the lock is checked with TLC: every order of the steps of two instances, with one crash, one failed file operation and two outside edits. `formal/write-pipeline/run.sh` runs it in a few minutes. A pass covers those bounds and nothing beyond them. The model has two open findings, which are the first two limits below.
- **Token counts.** Estimates made with the o200k tokenizer. Claude and Gemini use other tokenizers, so their real counts differ.
- **Linux.** The headless build is only compile-checked with `scripts/check-linux.sh`. It was never run on Linux, and the trash behavior on a headless Linux host is untested.

## Limits

- **A crash during an in-place write.** When a crash interrupts the write of a file with more than one hard link, the file is left half-written, and the change is listed as unfinished. A revert of that change shows the file as a conflict and leaves it as it is, but marks the change cancelled. The text from before is in the history, but the tool does not offer it as a revert.
- **The write window.** The tool checks a file's hash and then renames the new file over it. An edit by another program between those two steps is overwritten and is not kept in the history. The window is about one system call wide.
- **A hook's test run.** A test run that exceeds its time limit is stopped by killing the hook's process group. A hook that starts a process in a new session, with `setsid`, can leave that process running.
- **A new Claude Code memory** always gets `metadata.type: project`.
- **Recorded sessions.** Only Claude Code sessions are read. Turns that a version before 2.1.302 wrote show the conversation and the usage, not the request. Calls that Claude Code does not record, such as the one that writes a compaction summary, do not appear. Which part of a message came from which file or hook is matched by its text, so a few pieces stay unlabelled. A session is listed when it started in the project directory or below it.
- **Large or hostile repositories.** A scan reads at most 8 MiB of one file and 64 MiB in all. It lists at most 2,000 entries and 8 levels of one directory tree, and walks at most 20,000 entries of the project. Import expansion stops at 4 MiB for one file and 8 MiB for a scan. When a budget is spent, the scan goes on, and the Doctor says which budget it was.

## License

MIT. The full text is in [`LICENSE`](LICENSE).
