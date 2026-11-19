"""Cross-check hook event names: docs hooks.md summary table vs binary `gc` array vs
binary matcher-query switch (which events read a matcher field) vs docs matcher table.

Usage: python3 -I crosscheck_events.py <hooks.md path>
"""
import mmap, re, sys

BIN = "/Users/sungjin/.local/share/claude/versions/2.1.328"
doc = open(sys.argv[1], encoding="utf-8").read().splitlines()

# docs summary table: lines "| `Event` | When it fires |" before "### How a hook resolves"
end = next(i for i, l in enumerate(doc) if l.startswith("### How a hook resolves"))
docs_events = [m.group(1) for l in doc[:end] for m in [re.match(r"\| `([A-Za-z]+)` \|", l)] if m]

with open(BIN, "rb") as f:
    mm = mmap.mmap(f.fileno(), 0, access=mmap.ACCESS_READ)
    i = mm.find(b"var gc=[")
    arr = mm[i:mm.find(b"]", i)].decode()
    bin_events = re.findall(r'"([A-Za-z]+)"', arr)
    j = mm.find(b"function Bbn(e){switch(e.hook_event_name){")
    sw = mm[j:mm.find(b"default:return}}", j)].decode()
    # cases that return a value (not followed directly by another case/return;)
    cases = re.findall(r'case"([A-Za-z]+)":(?=(case|return;|return [^;]|return e|\{))', sw)
    no_field = set(re.findall(r'case"([A-Za-z]+)":(?:case"[A-Za-z]+":)*return;', sw))
    # expand fallthrough groups ending in return;
    grp = re.findall(r'((?:case"[A-Za-z]+":)+)return;', sw)
    no_field = {n for g in grp for n in re.findall(r'"([A-Za-z]+)"', g)}
    with_field = {c[0] for c in cases} - no_field
    k = mm.find(b"var w6=new Set([")
    w6 = set(re.findall(r'"([A-Za-z]+)"', mm[k:mm.find(b"])", k)].decode()))

print("docs events:", len(docs_events), " binary gc events:", len(bin_events))
print("in docs not binary:", sorted(set(docs_events) - set(bin_events)))
print("in binary not docs:", sorted(set(bin_events) - set(docs_events)))
print("matcher read by binary (Bbn returns a field):", len(with_field), sorted(with_field))
print("no matcher field (Bbn returns undefined or default):", sorted(set(bin_events) - with_field))
print("wide exact-match set (letters digits _ - space , |) w6:", len(w6))
print("narrow exact-match set (letters digits _ |):", sorted(with_field - w6))
