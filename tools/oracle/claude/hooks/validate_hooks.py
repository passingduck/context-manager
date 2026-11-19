"""Check a Claude Code hooks document against the rules found in the 2.1.328 binary and docs.

Usage: python3 -I validate_hooks.py <kind> <file.json> [...]
  kind = settings   file is a settings.json; hooks live under its "hooks" key
  kind = pluginfile file is a plugin hooks/hooks.json ({$schema?, description?, hooks?, modules?})
  kind = eventmap   file is a bare event map (plugin.json inline "hooks", frontmatter "hooks")

Prints ERROR (Claude Code rejects or drops it), WARN (loads but never fires or is ignored),
and OK lines. Exit 1 if any ERROR.
"""
import json
import re
import sys

EVENTS = ["PreToolUse", "PostToolUse", "PostToolUseFailure", "PostToolBatch", "Notification",
          "UserPromptSubmit", "UserPromptExpansion", "SessionStart", "SessionEnd", "Stop",
          "StopFailure", "SubagentStart", "SubagentStop", "PreCompact", "PostCompact",
          "PreModelSwitch", "PostModelSwitch", "PermissionRequest", "PermissionDenied", "Setup",
          "TeammateIdle", "TaskCreated", "TaskCompleted", "Elicitation", "ElicitationResult",
          "ConfigChange", "WorktreeCreate", "WorktreeRemove", "InstructionsLoaded", "CwdChanged",
          "FileChanged", "DirectoryAdded", "MessageDisplay"]  # binary 2.1.328 `gc`
NO_MATCHER = {"CwdChanged", "MessageDisplay", "PostToolBatch", "Stop", "TaskCompleted",
              "TaskCreated", "TeammateIdle", "UserPromptSubmit", "WorktreeCreate", "WorktreeRemove"}
CLOSED_VALUES = {  # binary 2.1.328 zod input enums / matcherMetadata
    "SessionStart": {"startup", "resume", "clear", "compact", "fork"},
    "Setup": {"init", "maintenance"},
    "PreCompact": {"manual", "auto"}, "PostCompact": {"manual", "auto"},
    "SessionEnd": {"clear", "resume", "logout", "prompt_input_exit", "other"},
    "ConfigChange": {"user_settings", "project_settings", "local_settings", "policy_settings", "skills"},
    "InstructionsLoaded": {"session_start", "nested_traversal", "path_glob_match", "include", "compact"},
    "DirectoryAdded": {"slash_command", "register_repo_root"},
}
TOOL_EVENTS = {"PreToolUse", "PostToolUse", "PostToolUseFailure", "PermissionRequest", "PermissionDenied"}
ALL5 = {"command", "http", "mcp_tool", "prompt", "agent"}
SUPPORTED = {e: {"command", "http", "mcp_tool"} for e in EVENTS}
for e in ["PermissionDenied", "PostToolBatch", "PostToolUse", "PostToolUseFailure", "PreToolUse",
          "Stop", "SubagentStop", "TaskCompleted", "TaskCreated", "TeammateIdle",
          "UserPromptExpansion", "UserPromptSubmit"]:
    SUPPORTED[e] = ALL5
SUPPORTED["PermissionRequest"] = ALL5 - {"agent"}
SUPPORTED["Setup"] = {"command"}
SUPPORTED["SessionStart"] = {"command", "mcp_tool"}

STR, NUM, BOOL, STRLIST, STRMAP, OBJ = "string", "positive number", "boolean", "string[]", "{string:string}", "object"
COMMON = {"type": STR, "if": STR, "timeout": NUM, "statusMessage": STR, "once": BOOL}
FIELDS = {  # binary 2.1.328 zod schema `_g()`; @internal fields included so they are not flagged
    "command": {**COMMON, "command": STR, "args": STRLIST, "shell": STR, "onFailure": STR, "async": BOOL,
                "asyncRewake": BOOL, "rewakeMessage": STR, "rewakeSummary": STR, "cloud": STR},
    "http": {**COMMON, "url": STR, "headers": STRMAP, "allowedEnvVars": STRLIST, "onFailure": STR, "cloud": STR},
    "mcp_tool": {**COMMON, "server": STR, "tool": STR, "input": OBJ, "onFailure": STR},
    "prompt": {**COMMON, "prompt": STR, "model": STR, "continueOnBlock": BOOL},
    "agent": {**COMMON, "prompt": STR, "model": STR},
}
REQUIRED = {"command": ["command"], "http": ["url"], "mcp_tool": ["server", "tool"],
            "prompt": ["prompt"], "agent": ["prompt"]}
ENUMS = {"shell": {"bash", "powershell"}, "onFailure": {"continue", "block"}}

out = []


def say(level, path, msg):
    out.append((level, f"{level} {path}: {msg}"))


def type_ok(v, t):
    return {STR: isinstance(v, str), BOOL: isinstance(v, bool),
            NUM: isinstance(v, (int, float)) and not isinstance(v, bool) and v > 0,
            STRLIST: isinstance(v, list) and all(isinstance(x, str) for x in v),
            STRMAP: isinstance(v, dict) and all(isinstance(x, str) for x in v.values()),
            OBJ: isinstance(v, dict)}[t]


def check_matcher(ev, m, path):
    if ev in NO_MATCHER:
        if m not in (None, "", "*"):
            say("WARN", path, f"{ev} has no matcher support; matcher {m!r} is ignored")
        return
    if m in (None, "", "*"):
        return
    exact = r"^[a-zA-Z0-9_|]+$" if ev in {"FileChanged", "StopFailure"} else r"^[a-zA-Z0-9_|, -]+$"
    if re.match(exact, m) and ev in CLOSED_VALUES:
        names = [s.strip() for s in re.split(r"[|,]" if "," in exact else r"\|", m) if s.strip()]
        unknown = [n for n in names if n not in CLOSED_VALUES[ev]]
        if unknown:
            say("WARN", path, f"{ev} matcher names {unknown}, which {ev} never reports; known: {sorted(CLOSED_VALUES[ev])}")
    if not re.match(exact, m):
        try:
            re.compile(m)  # approximation of JS RegExp
        except re.error as err:
            say("WARN", path, f"matcher {m!r} is not a valid regex ({err}); Claude Code logs it and never matches")


def check_handler(ev, h, path):
    if not isinstance(h, dict) or not isinstance(h.get("type"), str):
        return say("ERROR", path, "handler must be an object with a string type")
    t = h["type"]
    if t not in FIELDS:
        return say("ERROR", path, f"unknown type {t!r}; valid: {sorted(FIELDS)}")
    for k in REQUIRED[t]:
        if k not in h:
            say("ERROR", path, f"{t} handler needs {k!r}")
    for k, v in h.items():
        want = FIELDS[t].get(k)
        if want is None:
            say("WARN", path, f"field {k!r} is not in the {t} schema (zod strips or ignores it)")
        elif not type_ok(v, want):
            say("ERROR", path, f"field {k!r} must be {want}")
        elif k in ENUMS and v not in ENUMS[k]:
            say("ERROR", path, f"field {k!r} must be one of {sorted(ENUMS[k])}")
    if t == "http" and isinstance(h.get("url"), str) and not re.match(r"^[a-zA-Z][a-zA-Z0-9+.-]*://", h["url"]):
        say("ERROR", path, "url must be a valid URL")
    if t not in SUPPORTED[ev]:
        say("WARN", path, f"{t} handlers do not run on {ev} (skipped)")
    if "if" in h and ev not in TOOL_EVENTS:
        say("WARN", path, f"'if' is only evaluated on tool events; on {ev} this handler never runs")


def check_event_map(em, path):
    if not isinstance(em, dict):
        return say("ERROR", path, "must be an object mapping event names to matcher arrays")
    for ev, groups in em.items():
        p = f"{path}.{ev}"
        if ev not in EVENTS:
            say("WARN", p, "unknown hook event; entry ignored")
            continue
        if not isinstance(groups, list):
            say("ERROR", p, "must be an array of matcher groups")
            continue
        for i, g in enumerate(groups):
            gp = f"{p}[{i}]"
            if not isinstance(g, dict) or not isinstance(g.get("hooks"), list):
                say("ERROR", gp, "matcher group must be an object with a 'hooks' array")
                continue
            if "matcher" in g and not isinstance(g["matcher"], str):
                say("ERROR", gp, "matcher must be a string")
            else:
                check_matcher(ev, g.get("matcher"), gp)
            for j, h in enumerate(g["hooks"]):
                check_handler(ev, h, f"{gp}.hooks[{j}]")


def main():
    kind, files = sys.argv[1], sys.argv[2:]
    for f in files:
        doc = json.load(open(f, encoding="utf-8"))
        if kind == "settings":
            check_event_map(doc.get("hooks", {}), f"{f}:hooks")
        elif kind == "pluginfile":
            extra = set(doc) - {"$schema", "description", "hooks", "modules"}
            if extra:
                say("WARN", f, f"top-level keys not in the plugin hooks file schema: {sorted(extra)}")
            check_event_map(doc.get("hooks", {}), f"{f}:hooks")
        else:
            check_event_map(doc, f"{f}")
        if not any(f in line for _, line in out):
            out.append(("OK", f"OK {f}"))
    for _, line in out:
        print(line)
    sys.exit(1 if any(level == "ERROR" for level, _ in out) else 0)


main()
