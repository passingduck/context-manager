"""Search the embedded JS in the installed Claude Code binary (read-only).

Usage: python3 -I bgrep.py <literal-or-regex> [context=150] [max=20] [--re]
Prints each match with `context` bytes on either side, non-printables shown as '.'.
"""
import mmap
import re
import sys

BIN = "/Users/sungjin/.local/share/claude/versions/2.1.328"

args = [a for a in sys.argv[1:] if a != "--re"]
use_re = "--re" in sys.argv
pat = args[0].encode()
ctx = int(args[1]) if len(args) > 1 else 150
mx = int(args[2]) if len(args) > 2 else 20

with open(BIN, "rb") as f:
    mm = mmap.mmap(f.fileno(), 0, access=mmap.ACCESS_READ)
    rx = re.compile(pat if use_re else re.escape(pat))
    seen = set()
    n = 0
    for m in rx.finditer(mm):
        s = max(0, m.start() - ctx)
        e = min(len(mm), m.end() + ctx)
        chunk = mm[s:e]
        key = chunk
        if key in seen:
            continue
        seen.add(key)
        text = "".join(chr(b) if 32 <= b < 127 else "." for b in chunk)
        print(f"@{m.start()}: {text}\n")
        n += 1
        if n >= mx:
            break
