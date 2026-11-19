"""Extract per-event hook metadata (summary, matcherMetadata) from the Claude Code binary.

Usage: python3 -I event_meta.py
Prints one line per event: name | summary | matcher field | matcher values
"""
import mmap, re

BIN = "/Users/sungjin/.local/share/claude/versions/2.1.328"
with open(BIN, "rb") as f:
    mm = mmap.mmap(f.fileno(), 0, access=mmap.ACCESS_READ)
    start = mm.find(b"PreToolUse:{summary:")
    end = start + 60000
    blob = mm[start:end].decode("utf-8", "replace")

# events are keys like  Name:{summary:"..."
for m in re.finditer(r'(?:^|[,{])([A-Z][A-Za-z]+):\{summary:"([^"]*)"', blob):
    name, summary = m.group(1), m.group(2)
    # matcherMetadata within this event's object: search up to the next event key
    nxt = re.search(r',[A-Z][A-Za-z]+:\{summary:"', blob[m.end():])
    body = blob[m.end(): m.end() + (nxt.start() if nxt else 4000)]
    mf = re.search(r'matcherMetadata:\{fieldToMatch:"([^"]*)"', body)
    mv = re.search(r'values:(\[[^\]]*\])', body)
    mmeta = "matcherMetadata" in body
    print(f"{name} | {summary} | field={mf.group(1) if mf else '-'} | values={mv.group(1) if mv else '-'} | hasMatcherMeta={mmeta}")
