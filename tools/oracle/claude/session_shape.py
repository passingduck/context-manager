"""Print the numbers a reader of a Claude Code transcript must reproduce, without its content.

Usage (from the repository root):

    python3 tools/oracle/claude/session_shape.py [--no-digest] [--row] <transcript.jsonl> ...

For each file it prints record counts by type, the versions that wrote it, the request records
and their chains, how every message hash resolves (section 4 of
docs/agents/claude-code-sessions.md), whether each blob and shape hash matches its content, and
whether the rebuilt body of each request hashes to its `requestDigest` (section 3.3).
--no-digest skips that last check, which costs one pass over every request body.
--row prints one table row per file instead: versions, requests, distinct message hashes,
resolved hashes, blobs whose hash matches, digests that match.

It prints numbers and these categorical values only: record types, versions, query sources with
any custom agent name replaced by `*`, and cache mark forms. It never prints message text, tool
input or output, paths, titles or identifiers, so it is safe to run on a private transcript.
"""
import collections
import json
import os
import re
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import session_record as sr  # noqa: E402

SAFE = re.compile(r"^[A-Za-z0-9_.\-]{1,48}$")


def cat(v):
    s = str(v)
    return s if SAFE.match(s) else "<other>"


def query_source(v):
    s = str(v)
    if s.startswith("agent:custom"):
        return "agent:custom:*" if s != "agent:custom" else s
    return s if re.match(r"^[a-z_]+(:[a-z_:\-]+)?$", s) and len(s) <= 64 else "<other>"


def version_key(v):
    try:
        return [int(x) for x in v.split(".")]
    except ValueError:
        return [0]


def analyse(path, digest=True):
    types, versions = collections.Counter(), collections.Counter()
    blob_records = shape_records = 0
    blob_strings = 0
    with open(path, encoding="utf-8", errors="surrogateescape") as f:
        for raw in f:
            try:
                r = json.loads(raw)
            except ValueError:
                continue
            if not isinstance(r, dict):
                continue
            types[cat(r.get("type"))] += 1
            if isinstance(r.get("version"), str):
                versions[cat(r["version"])] += 1
            if r.get("type") == "api-request-blob":
                blob_records += 1
                if isinstance((r.get("message") or {}).get("content"), str):
                    blob_strings += 1
            elif r.get("type") == "api-request-shape":
                shape_records += 1
    t = sr.Transcript(path)
    out = {"bytes": os.path.getsize(path), "records": t.records, "unparsed": t.unparsed, "types": types,
           "versions": sorted(versions, key=version_key)}
    qs, chain = collections.Counter(), collections.Counter()
    referenced, marks = set(), collections.Counter()
    digests = collections.Counter()
    for r in t.requests:
        qs[query_source(r.get("querySource"))] += 1
        for k in ("withheld", "cut", "agentId"):
            if k in r:
                chain["with " + k] += 1
        base = r.get("base")
        if not base:
            chain["start a chain (keep 0, no base)"] += 1
        elif base not in t.by_id:
            chain["base not in this file"] += 1
        else:
            bh = t.message_hashes(t.by_id[base])
            if bh is not None and r.get("keep", 0) < len(bh):
                chain["keep fewer messages than the base holds"] += 1
            else:
                chain["extend the base"] += 1
        hashes = t.message_hashes(r)
        if hashes is not None:
            referenced.update(hashes)
        for m in r.get("cacheControl", []):
            form = "/".join("N" if isinstance(x, int) else cat(x) for x in m.get("path", []))
            marks[form + " " + json.dumps(m.get("value"), sort_keys=True)] += 1
        if digest:
            body, missing = t.rebuild(r)
            if body is None:
                digests["not rebuilt: " + ",".join(sorted(set(missing)))] += 1
            else:
                want = r.get("requestDigest") or r.get("cutRequestDigest")
                digests["match" if sr.request_digest(body) == want else "differ"] += 1
    sources = collections.Counter(t.resolve(h)[0] or "unresolved" for h in referenced)
    replied = sum(1 for r in t.requests if r.get("id") in t.replies)
    same_usage = sum(1 for v in t.replies.values()
                     if len({json.dumps(x["message"].get("usage"), sort_keys=True) for x in v}) == 1)
    out.update(
        requests=len(t.requests), query_sources=qs, chain=chain, marks=marks, digests=digests,
        shapes={"records": shape_records, "distinct": len(t.shapes), "hash matches": t.shape_hash_ok},
        blobs={"records": blob_records, "distinct": len(t.blobs), "hash matches": t.blob_hash_ok,
               "stored with string content": blob_strings},
        hashes={"distinct referenced": len(referenced), "blob": sources.get("blob", 0),
                "transcript": sources.get("transcript", 0), "unresolved": sources.get("unresolved", 0)},
        replies={"requests with a reply record": replied, "requests without": len(t.requests) - replied,
                 "reply records naming no request record": sum(1 for k in t.replies if k not in t.by_id),
                 "requests whose reply records share one usage": same_usage},
    )
    return out


def show(i, path, a):
    print(f"[{i}] {a['bytes']} bytes, {a['records']} records, {a['unparsed']} unparsed, "
          f"versions {', '.join(a['versions']) or '-'}")
    print("  record types: " + ", ".join(f"{k} {n}" for k, n in a["types"].most_common()))
    print(f"  requests: {a['requests']}; query sources: " + ", ".join(f"{k} {n}" for k, n in a["query_sources"].most_common()))
    print("  chains: " + ", ".join(f"{k} {n}" for k, n in a["chain"].most_common()))
    for name in ("shapes", "blobs", "hashes", "replies"):
        print(f"  {name}: " + ", ".join(f"{k} {n}" for k, n in a[name].items()))
    if a["digests"]:
        print("  requestDigest of the rebuilt body: " + ", ".join(f"{k} {n}" for k, n in a["digests"].most_common()))
    for k, n in a["marks"].most_common():
        print(f"  cache mark {k}: {n}")


def main(argv):
    digest = "--no-digest" not in argv
    row = "--row" in argv
    paths = [a for a in argv if not a.startswith("--")]
    if not paths:
        print(__doc__)
        return 2
    if row:
        print("| # | versions | requests | distinct message hashes | resolved | blobs whose hash matches | digests that match |")
        print("| -: | :- | -: | -: | -: | :- | :- |")
    for i, p in enumerate(paths):
        a = analyse(p, digest)
        if row:
            v = a["versions"]
            vr = (v[0] if len(v) == 1 else f"{v[0]} to {v[-1]}") if v else "-"
            h = a["hashes"]
            d = a["digests"]
            print(f"| {i} | {vr} | {a['requests']} | {h['distinct referenced']} | "
                  f"{h['blob'] + h['transcript']} | {a['blobs']['hash matches']} of {a['blobs']['records']} | "
                  f"{d.get('match', 0) if digest else '-'} of {a['requests']} |")
        else:
            show(i, p, a)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
