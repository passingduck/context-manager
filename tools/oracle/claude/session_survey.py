"""Count, over every transcript under a Claude Code projects directory, the facts that
docs/agents/claude-code-sessions.md cites as OBSERVED. Each output line names its section.

Usage (from the repository root):

    python3 tools/oracle/claude/session_survey.py <config>/projects [--digest]

--digest also rebuilds every request and checks it against `requestDigest`, which takes minutes
on large files. The output holds counts and these categorical values only: record types,
attachment types, roles, versions, query sources with custom agent names replaced by `*`, error
kinds, compaction triggers and key names. It never prints message text, tool input or output,
paths, titles or identifier values, so it is safe to run on private transcripts.
"""
import collections
import json
import os
import re
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import session_record as sr  # noqa: E402
from session_shape import query_source  # noqa: E402

SAFE = re.compile(r"^[A-Za-z0-9_.\-<>]{1,48}$")
KEY = re.compile(r"^[A-Za-z_][A-Za-z0-9_]{0,48}$")
IDENTITY = re.compile(r"(account|organi[sz]ation|email|owner|device|user_?id|host)", re.I)
OPEN, CLOSE = "<system-reminder>\n", "\n</system-reminder>"
C = collections.defaultdict(collections.Counter)


def cat(v):
    s = str(v)
    return s if SAFE.match(s) else "<other>"


def texts(content):
    out = []
    for b in sr.as_blocks(content):
        if not isinstance(b, dict):
            continue
        if b.get("type") == "text":
            out.append(b.get("text", ""))
        elif b.get("type") == "tool_result":
            c = b.get("content")
            out.append(c if isinstance(c, str) else "".join(x.get("text", "") for x in c or [] if isinstance(x, dict)))
    return out


def key_paths(v, path, out, depth=0):
    if depth > 6:
        return
    if isinstance(v, dict):
        for k, x in v.items():
            if not KEY.match(k) or k.startswith("toolu"):
                continue
            if k in ("message", "toolUseResult", "input", "content", "rendered", "snapshot", "shape"):
                continue
            p = path + "." + k
            out.add(p)
            key_paths(x, p, out, depth + 1)
    elif isinstance(v, list):
        for x in v[:20]:
            key_paths(x, path + "[]", out, depth + 1)


def survey(path, digest):
    sub = "/subagents/" in path
    tag = "subagent" if sub else "main"
    C["1 files"][tag + " transcripts"] += 1
    recs = []
    with open(path, encoding="utf-8", errors="surrogateescape") as f:
        for raw in f:
            try:
                r = json.loads(raw)
            except ValueError:
                continue
            if isinstance(r, dict):
                recs.append(r)
    t = sr.Transcript(path)
    if t.requests:
        C["1 files"][tag + " transcripts with request records"] += 1
    pos = {}
    boundaries = []
    for i, r in enumerate(recs):
        ty = cat(r.get("type"))
        C["2 record types"][ty] += 1
        v = cat(r.get("version")) if r.get("version") else None
        if v:
            if ty == "api-request":
                C["12 by version"][v + " api-request"] += 1
            elif ty == "assistant":
                C["12 by version"][v + " assistant"] += 1
                C["12 by version"][v + " assistant with requestRef"] += bool(r.get("requestRef"))
            elif ty == "attachment":
                C["12 by version"][v + " attachment"] += 1
                C["12 by version"][v + " attachment with rendered"] += "rendered" in r
                C["12 by version"][v + " attachment with renderedRole"] += "renderedRole" in r
        if ty == "api-request":
            pos[r.get("id")] = i
            C["3.4 query sources"][f"{query_source(r.get('querySource'))} ({tag})"] += 1
            for k in ("withheld", "cut", "agentId"):
                C["3.2 optional request fields"][f"{k} ({tag})"] += k in r
        elif ty == "system":
            C["7 system subtypes"][cat(r.get("subtype"))] += 1
            if r.get("subtype") == "compact_boundary":
                boundaries.append(i)
                C["7 compaction"]["trigger " + cat((r.get("compactMetadata") or {}).get("trigger"))] += 1
        elif ty == "user":
            C["11 entrypoint"][f"{cat(r.get('entrypoint'))} ({tag})"] += 1
            for flag in ("isMeta", "isCompactSummary", "isVisibleInTranscriptOnly", "isSidechain"):
                C["2 user record flags"][flag] += bool(r.get(flag))
        elif ty == "assistant":
            m = r.get("message") or {}
            if m.get("model") == "<synthetic>":
                C["6 synthetic records"][f"error {cat(r.get('error'))}, requestRef {bool(r.get('requestRef'))}"] += 1
            u = m.get("usage") or {}
            it = u.get("iterations")
            if isinstance(it, list):
                kinds = ",".join(cat(x.get("type")) for x in it if isinstance(x, dict))
                same = len(it) == 1 and all(it[0].get(k) == u.get(k) for k in
                                            ("input_tokens", "output_tokens", "cache_read_input_tokens",
                                             "cache_creation_input_tokens"))
                C["6 usage.iterations"][f"{len(it)} [{kinds}] equals top-level {same}"] += 1
        paths = set()
        key_paths(r, ty, paths)
        for p in paths:
            if IDENTITY.search(p.rsplit(".", 1)[-1]):
                C["14 identity-like key paths"][p] += 1
        if ty == "attachment":
            a = r.get("attachment") or {}
            C["10 attachment fields"][f"{cat(a.get('type'))}: " + ",".join(sorted(k for k in a if KEY.match(k)))] += 1
    # 4: resolution, chains and digests
    ref = set()
    for r in t.requests:
        hs = t.message_hashes(r)
        if hs is None:
            C["4 chains"]["base not in the file"] += 1
            continue
        ref.update(hs)
        base = t.by_id.get(r.get("base"))
        if base is None:
            first = r is t.requests[0]
            after_boundary = any(b < pos[r["id"]] for b in boundaries)
            C["4 chains"][f"keep 0 ({'first in file' if first else 'after a compact boundary' if after_boundary else 'other'})"] += 1
        else:
            bh = t.message_hashes(base)
            if r["keep"] < len(bh):
                dropped = [t.resolve(h)[1] for h in bh[r["keep"]:]]
                added = [t.resolve(h)[1] for h in r["tail"][:len(dropped)]]
                grew = all(d and a and d["role"] == a["role"] and
                           sr.as_blocks(a["content"])[:len(sr.as_blocks(d["content"]))] == sr.as_blocks(d["content"])
                           for d, a in zip(dropped, added) if d and d["role"] == "user")
                C["4.4 keep below the base"][f"dropped {len(dropped)}, roles {[d['role'] if d else '?' for d in dropped]}, "
                                              f"dropped user message is a prefix of its replacement {grew}, "
                                              f"base has a reply record {base['id'] in t.replies}"] += 1
            else:
                C["4 chains"]["extend the base"] += 1
        # 5: cache marks
        shape = t.shapes.get(r.get("shapeHash")) or {}
        sysmarks = [m["path"][1] for m in r.get("cacheControl", []) if m["path"][0] == "system"]
        C["5 system marks"][f"{sysmarks} of {len(shape.get('system') or [])} blocks"] += 1
        C["5 tool marks"][str(sum(1 for m in r.get("cacheControl", []) if m["path"][0] == "tools"))] += 1
        for m in r.get("cacheControl", []):
            C["5 mark values"]["/".join("N" if isinstance(x, int) else cat(x) for x in m["path"]) + " "
                               + json.dumps(m.get("value"), sort_keys=True)] += 1
            if m["path"][0] == "messages":
                idx, blk = m["path"][1], m["path"][3]
                msg = t.resolve(hs[idx])[1]
                last_block = msg is not None and blk == len(sr.as_blocks(msg["content"])) - 1
                after = [t.resolve(h)[1] for h in hs[idx + 1:]]
                C["5 message mark"][f"on message {len(hs) - 1 - idx} from the end ({msg['role'] if msg else '?'}), "
                                    f"last block {last_block}, messages after it "
                                    f"{[x['role'] if x else '?' for x in after]}"] += 1
        # 6: usage along a chain
        if base is not None and r["keep"] >= len(t.message_hashes(base) or []):
            u0, u1 = t.final_usage(base["id"]), t.final_usage(r["id"])
            if u0 and u1:
                prev = u0.get("cache_read_input_tokens", 0) + u0.get("cache_creation_input_tokens", 0)
                C["5 cache arithmetic"][f"read(next) == read + write(previous): {u1.get('cache_read_input_tokens') == prev}"] += 1
                cc = u1.get("cache_creation") or {}
                C["5 cache arithmetic"][f"1h + 5m == cache_creation_input_tokens: "
                                        f"{cc.get('ephemeral_1h_input_tokens', 0) + cc.get('ephemeral_5m_input_tokens', 0) == u1.get('cache_creation_input_tokens')}"] += 1
        if digest:
            body, missing = t.rebuild(r)
            want = r.get("requestDigest") or r.get("cutRequestDigest")
            C["4.3 requestDigest"]["not rebuilt" if body is None else
                                   "match" if sr.request_digest(body) == want else "differ"] += 1
    for h in ref:
        C["4.3 message hashes"][t.resolve(h)[0] or "unresolved"] += 1
    C["4.3 message hashes"]["blob records whose hash matches"] += t.blob_hash_ok
    C["4.3 message hashes"]["blob records"] += sum(1 for r in recs if r.get("type") == "api-request-blob")
    # 6: replies
    for rid, reps in t.replies.items():
        C["6 reply records per request"][str(min(len(reps), 6))] += 1
        C["6 reply records per request"][f"share one usage {len({json.dumps(x['message'].get('usage'), sort_keys=True) for x in reps}) == 1}"] += 1
        C["6 reply records per request"][f"share one message id {len({x['message'].get('id') for x in reps}) == 1}"] += 1
        if rid not in t.by_id:
            C["6 reply records per request"]["requestRef names no request record"] += 1
    C["6 reply records per request"]["requests without a reply record"] += sum(1 for r in t.requests if r["id"] not in t.replies)
    # 7: the first request after each boundary
    for b in boundaries:
        nxt = next((r for r in recs[b + 1:] if r.get("type") == "api-request"), None)
        summary = any(r.get("isCompactSummary") for r in recs[b + 1:b + 12])
        C["7 compaction"][f"summary record follows {summary}; next request "
                          + ("none" if nxt is None else f"keep {'0' if nxt['keep'] == 0 else '>0'}")] += 1
    # 9 and 10: what reached the next request
    pending = []
    for r in recs:
        if r.get("type") == "attachment" and isinstance(r.get("rendered"), list):
            pending.append(r)
        elif r.get("type") == "api-request" and pending:
            hs = t.message_hashes(r) or []
            msgs = [t.resolve(h)[1] for h in hs[-8:]]
            for a in pending:
                at, role = cat((a.get("attachment") or {}).get("type")), a.get("renderedRole")
                for piece in a["rendered"]:
                    c = piece.get("content") if isinstance(piece, dict) else None
                    if not isinstance(c, str):
                        continue
                    u = c[len(OPEN):-len(CLOSE)] if c.startswith(OPEN) and c.endswith(CLOSE) else c
                    hit = "verbatim" if any(m and any(c in x for x in texts(m["content"])) for m in msgs) else \
                        "unwrapped" if any(m and any(u in x for x in texts(m["content"])) for m in msgs) else "not found"
                    where = sorted({m["role"] for m in msgs if m and any((c if hit == "verbatim" else u) in x
                                                                          for x in texts(m["content"]))})
                    C["10 rendered attachments in the next request"][f"{at} renderedRole={role}: {hit} {where}"] += 1
            pending = []
    for h in ref:
        m = t.resolve(h)[1]
        if m and any(x.startswith("<persisted-output>") for x in texts(m["content"])):
            C["9 persisted output"]["request messages with a persisted-output stand-in"] += 1
    if sub:
        meta = path[:-len(".jsonl")] + ".meta.json"
        C["8 subagents"][f"meta.json beside it {os.path.isfile(meta)}"] += 1
        agent = os.path.basename(path)[len("agent-"):-len(".jsonl")]
        C["8 subagents"][f"every request record carries the file's agentId {all(r.get('agentId') == agent for r in t.requests)}"] += 1


def main(argv):
    digest = "--digest" in argv
    roots = [a for a in argv if not a.startswith("--")]
    if len(roots) != 1 or not os.path.isdir(roots[0]):
        print(__doc__)
        return 2
    for d, _, fs in os.walk(roots[0]):
        for f in sorted(fs):
            p = os.path.join(d, f)
            if f.endswith(".jsonl"):
                survey(p, digest)
            elif f.endswith(".meta.json"):
                C["1 files"]["meta.json"] += 1
            if os.sep + "tool-results" + os.sep in p:
                C["1 files"]["tool-results files *" + os.path.splitext(f)[1]] += 1
    for name in sorted(C, key=lambda s: [int(x) if x.isdigit() else x for x in re.split(r"(\d+)", s.split(" ")[0])]):
        print(f"\n[{name}]")
        for k, n in sorted(C[name].items(), key=lambda kv: -kv[1]):
            if n:
                print(f"  {n:7d}  {k}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
