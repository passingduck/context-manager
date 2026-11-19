"""Summarize one experiment run: which MARK- tokens reached the model, in order, and InstructionsLoaded events.

Usage: python3 -I analyze.py <exp_dir> [--context]
"""
import glob
import json
import os
import re
import sys

exp = sys.argv[1]
show_ctx = "--context" in sys.argv
reqs = sorted(glob.glob(os.path.join(exp, "requests", "req-*.json")))
best = None
for p in reqs:
    rec = json.load(open(p))
    print(f"{os.path.basename(p)} {rec['method']} {rec['path']}")
    body = rec.get("body")
    if isinstance(body, dict) and "messages" in body:
        best = body
if best is None:
    sys.exit("no messages request captured")

text = json.dumps(best)
order = []
for m in re.finditer(r"MARK-[A-Z0-9-]+", text):
    if m.group(0) not in order:
        order.append(m.group(0))
print("\nMARK tokens in first-seen order (messages request):")
for t in order:
    print("  ", t)

mem = re.findall(r"MARK-MEM-LINE-(\d+)", text)
if mem:
    print(f"\nMEMORY.md lines present: {min(mem)}..{max(mem)} ({len(set(mem))} distinct)")

if show_ctx:
    for t in ["MARK-REPO-CLAUDE", "MARK-MEM-LINE-200", "MARK-USER-CLAUDE"]:
        i = text.find(t)
        if i >= 0:
            print(f"\n--- context around {t} ---\n" + text[max(0, i - 700): i + 400])

il = os.path.join(exp, "il.jsonl")
if os.path.exists(il):
    print("\nInstructionsLoaded events:")
    for line in open(il):
        line = line.strip()
        if not line:
            continue
        try:
            e = json.loads(line)
        except Exception:
            continue
        fp = e.get("file_path", "").replace(exp, "$EXP")
        extra = {k: e[k] for k in ("globs", "parent_file_path", "trigger_file_path") if k in e}
        for k, v in list(extra.items()):
            if isinstance(v, str):
                extra[k] = v.replace(exp, "$EXP")
        print(f"   {e.get('memory_type'):8} {e.get('load_reason'):18} {fp} {extra if extra else ''}")
