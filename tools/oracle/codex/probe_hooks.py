"""Probe Codex hook discovery through `codex app-server` JSON-RPC `hooks/list`.

Builds isolated HOME/CODEX_HOME + projects under the scratchpad, starts the
real binary over stdio, and prints what each hook source resolves to.
Run: python3 -I probe_hooks.py
"""

import json
import os
import shutil
import subprocess
import sys
import threading
import time

BASE = os.path.join(os.path.dirname(os.path.abspath(__file__)), "probe_runs", "hooks_rpc")


def write(path, text):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "w") as f:
        f.write(text)


shutil.rmtree(BASE, ignore_errors=True)
home = os.path.join(BASE, "home")
ch = os.path.join(home, ".codex")
trusted = os.path.join(BASE, "trusted")
unknown = os.path.join(BASE, "unknown")
for d in (ch, trusted, unknown):
    os.makedirs(d, exist_ok=True)
for d in (trusted, unknown):
    subprocess.run(["git", "init", "-q", d], check=True)

USER_HOOKS_JSON = {
    "hooks": {
        "PreToolUse": [
            {"matcher": "Bash", "hooks": [{"type": "command", "command": "echo user-json-pre", "timeout": 30}]}
        ],
        "Stop": [
            {"matcher": "ignored-for-stop", "hooks": [{"type": "command", "command": "echo user-json-stop"}]}
        ],
        "UserPromptSubmit": [
            {"hooks": [{"type": "prompt"}]}
        ],
    }
}
write(os.path.join(ch, "hooks.json"), json.dumps(USER_HOOKS_JSON, indent=2))
write(os.path.join(ch, "config.toml"), f'''
[[hooks.SessionStart]]
matcher = "startup|resume"
[[hooks.SessionStart.hooks]]
type = "command"
command = "echo user-toml-start"
statusMessage = "starting"

[projects."{os.path.realpath(trusted)}"]
trust_level = "trusted"
''')
for proj, tag in ((trusted, "trusted"), (unknown, "unknown")):
    write(os.path.join(proj, ".codex", "hooks.json"), json.dumps({
        "hooks": {"PostToolUse": [{"matcher": "Edit|Write", "hooks": [{"type": "command", "command": f"echo {tag}-project-json"}]}]}
    }))
    write(os.path.join(proj, ".codex", "config.toml"),
          f'[[hooks.PreCompact]]\nmatcher = "manual"\n[[hooks.PreCompact.hooks]]\ntype = "command"\ncommand = "echo {tag}-project-toml"\n')

env = dict(os.environ, HOME=home, CODEX_HOME=ch)
proc = subprocess.Popen(["codex", "app-server"], cwd=trusted, env=env,
                        stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
responses = {}


def reader():
    for line in proc.stdout:
        try:
            msg = json.loads(line)
        except ValueError:
            continue
        if "id" in msg:
            responses[msg["id"]] = msg


threading.Thread(target=reader, daemon=True).start()


def send(obj):
    proc.stdin.write(json.dumps(obj) + "\n")
    proc.stdin.flush()


def wait(i, timeout=60):
    end = time.time() + timeout
    while time.time() < end:
        if i in responses:
            return responses[i]
        time.sleep(0.1)
    return None


send({"jsonrpc": "2.0", "id": 1, "method": "initialize",
      "params": {"clientInfo": {"name": "probe", "version": "0.0.0"}}})
init = wait(1)
send({"jsonrpc": "2.0", "method": "initialized"})
send({"jsonrpc": "2.0", "id": 2, "method": "hooks/list", "params": {"cwds": [trusted, unknown]}})
res = wait(2)
proc.kill()
if res is None:
    print("no hooks/list response; init =", json.dumps(init)[:400])
    print(proc.stderr.read()[-2000:])
    sys.exit(1)
if "error" in res:
    print("error:", json.dumps(res["error"])[:1500])
    sys.exit(1)
out = json.dumps(res["result"], indent=1)
write(os.path.join(BASE, "hooks_list.json"), out)
for entry in res["result"].get("data", []):
    cwd = entry.get("cwd")
    print("== cwd", cwd)
    for k, v in entry.items():
        if k not in ("hooks", "cwd"):
            print("  ", k, "=", json.dumps(v)[:600])
    for hk in entry.get("hooks", []):
        print("  -", {k: hk.get(k) for k in ("key", "eventName", "matcher", "source", "sourcePath", "trustStatus", "enabled", "timeoutSec", "handler", "isManaged") if k in hk})
