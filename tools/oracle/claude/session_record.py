"""Rebuild the Messages API requests that Claude Code recorded in a session transcript.

Implements docs/agents/claude-code-sessions.md, sections 3 to 5. The functions here print
nothing; session_shape.py and session_oracle.py are the command-line tools.

A transcript is JSON Lines. Three record types hold the requests (section 3):

- `api-request-shape` holds the system blocks and tool definitions, keyed by `shapeHash`.
- `api-request-blob` holds one message, keyed by its hash.
- `api-request` names a shape, the parameters, and the message hashes of one request.

A message hash that has no blob resolves to a run of `user` or `assistant` records (section 4).
"""
import decimal
import hashlib
import json

# Top-level body keys that Claude Code leaves out of the record and of `requestDigest` (section 3.3).
UNRECORDED_KEYS = frozenset(["metadata", "stream", "thread", "diagnostics", "safeguards", "data_flow"])
REQUEST_TYPES = ("api-request", "api-request-shape", "api-request-blob", "api-request-resumed")


# --------------------------------------------------------------------------------------------
# Canonical JSON, byte for byte as Claude Code writes it (section 3.1)
# --------------------------------------------------------------------------------------------

def _js_number(v):
    """Format a number the way JavaScript's JSON.stringify does."""
    if isinstance(v, bool):
        return "true" if v else "false"
    if isinstance(v, int):
        if abs(v) < 2 ** 53:
            return str(v)
        v = float(v)
    if v != v or v in (float("inf"), float("-inf")):
        return "null"
    if v == 0:
        return "0"
    sign = "-" if v < 0 else ""
    d = decimal.Decimal(repr(abs(v))).normalize()
    _, digits, exp = d.as_tuple()
    s = "".join(map(str, digits))
    k = len(s)
    n = exp + k
    if k <= n <= 21:
        out = s + "0" * (n - k)
    elif 0 < n <= 21:
        out = s[:n] + "." + s[n:]
    elif -6 < n <= 0:
        out = "0." + "0" * (-n) + s
    else:
        e = n - 1
        es = ("+" if e >= 0 else "-") + str(abs(e))
        out = (s if k == 1 else s[0] + "." + s[1:]) + "e" + es
    return sign + out


_ESC = {'"': '\\"', "\\": "\\\\", "\b": "\\b", "\f": "\\f", "\n": "\\n", "\r": "\\r", "\t": "\\t"}


def _js_string(s):
    out = ['"']
    for ch in s:
        o = ord(ch)
        if ch in _ESC:
            out.append(_ESC[ch])
        elif o < 0x20 or 0xD800 <= o <= 0xDFFF:
            out.append("\\u%04x" % o)
        else:
            out.append(ch)
    out.append('"')
    return "".join(out)


def _utf16_key(k):
    return k.encode("utf-16-be", "surrogatepass")


def canon(value, omit_cache_control=False):
    """Compact JSON with keys sorted by UTF-16 code units, as Claude Code hashes it."""
    parts = []

    def walk(v):
        if isinstance(v, str):
            parts.append(_js_string(v))
        elif v is None:
            parts.append("null")
        elif isinstance(v, (bool, int, float)):
            parts.append(_js_number(v))
        elif isinstance(v, list):
            parts.append("[")
            for i, x in enumerate(v):
                if i:
                    parts.append(",")
                walk(x)
            parts.append("]")
        else:
            parts.append("{")
            first = True
            for key in sorted(v, key=_utf16_key):
                if omit_cache_control and key == "cache_control":
                    continue
                if not first:
                    parts.append(",")
                first = False
                parts.append(_js_string(key))
                parts.append(":")
                walk(v[key])
            parts.append("}")

    walk(value)
    return "".join(parts)


def sha256(text):
    return hashlib.sha256(text.encode("utf-8", "surrogatepass")).hexdigest()


def as_blocks(content):
    """A string content becomes one text block before hashing (section 3.1)."""
    if isinstance(content, str):
        return [{"type": "text", "text": content}]
    if isinstance(content, list):
        return content
    return [content]


def message_hash(message):
    m = dict(message)
    m["content"] = as_blocks(m.get("content"))
    return sha256(canon(m, omit_cache_control=True))


def shape_hash(shape):
    return sha256(canon(shape, omit_cache_control=True))


def request_digest(body):
    """`requestDigest`: the whole body, cache marks included, without UNRECORDED_KEYS (section 3.3)."""
    b = {k: v for k, v in body.items() if k not in UNRECORDED_KEYS}
    if isinstance(b.get("messages"), list):
        b["messages"] = [dict(m, content=as_blocks(m.get("content"))) for m in b["messages"]]
    return sha256(canon(b))


def strip_cache_control(v):
    if isinstance(v, list):
        return [strip_cache_control(x) for x in v]
    if isinstance(v, dict):
        return {k: strip_cache_control(x) for k, x in v.items() if k != "cache_control"}
    return v


def cache_marks(body):
    """Every `cache_control` in system, tools and messages, as (path, value), in body order."""
    out = []

    def walk(v, path):
        if isinstance(v, list):
            for i, x in enumerate(v):
                walk(x, path + [i])
        elif isinstance(v, dict):
            for k, x in v.items():
                if k == "cache_control":
                    out.append((path, x))
                else:
                    walk(x, path + [k])

    for key in ("system", "tools", "messages"):
        if key in body:
            walk(body[key], [key])
    return out


# --------------------------------------------------------------------------------------------
# The transcript reader (sections 3 and 4)
# --------------------------------------------------------------------------------------------

class Run:
    """Consecutive `user` records, or `assistant` records with one `message.id` (section 4.2).

    The hash of the first k blocks is kept as a running SHA-256, so a long run costs one pass."""

    __slots__ = ("role", "message_id", "blocks", "hasher")

    def __init__(self, role, message_id):
        self.role, self.message_id, self.blocks = role, message_id, []
        self.hasher = hashlib.sha256(b'{"content":[')

    def continues(self, role, message_id):
        return self.role == role and (role == "user" or (message_id is not None and self.message_id == message_id))

    def add(self, blocks):
        for b in blocks:
            if self.blocks:
                self.hasher.update(b",")
            self.hasher.update(canon(b, omit_cache_control=True).encode("utf-8", "surrogatepass"))
            self.blocks.append(b)
        h = self.hasher.copy()
        h.update(('],"role":' + _js_string(self.role) + "}").encode("utf-8", "surrogatepass"))
        return h.hexdigest()


class Transcript:
    def __init__(self, path):
        self.path = path
        self.records = 0
        self.unparsed = 0
        self.shapes = {}          # shapeHash -> shape
        self.blobs = {}           # hash -> message
        self.blob_hash_ok = 0
        self.shape_hash_ok = 0
        self.runs = {}            # hash -> (role, blocks list, n)
        self.requests = []        # api-request records in file order
        self.by_id = {}
        self.replies = {}         # request id -> list of assistant records
        self.resumed = []
        self._messages = {}
        self._read()

    def _read(self):
        run = None
        with open(self.path, encoding="utf-8", errors="surrogateescape") as f:
            for raw in f:
                if not raw.strip():
                    continue
                try:
                    rec = json.loads(raw)
                except ValueError:
                    self.unparsed += 1
                    continue
                if not isinstance(rec, dict):
                    continue
                self.records += 1
                t = rec.get("type")
                if t == "api-request-shape":
                    self.shapes[rec.get("shapeHash")] = rec.get("shape")
                    if shape_hash(rec.get("shape")) == rec.get("shapeHash"):
                        self.shape_hash_ok += 1
                elif t == "api-request-blob":
                    self.blobs[rec.get("hash")] = rec.get("message")
                    if isinstance(rec.get("message"), dict) and message_hash(rec["message"]) == rec.get("hash"):
                        self.blob_hash_ok += 1
                elif t == "api-request":
                    self.requests.append(rec)
                    self.by_id[rec.get("id")] = rec
                elif t == "api-request-resumed":
                    self.resumed.append(rec)
                elif t in ("user", "assistant"):
                    if t == "assistant" and rec.get("requestRef"):
                        self.replies.setdefault(rec["requestRef"], []).append(rec)
                    msg = rec.get("message")
                    if not isinstance(msg, dict) or not isinstance(msg.get("role"), str) or msg.get("content") is None:
                        run = None
                        continue
                    mid = msg.get("id") if isinstance(msg.get("id"), str) else None
                    if run is None or not run.continues(msg["role"], mid):
                        run = Run(msg["role"], mid)
                    h = run.add(as_blocks(msg["content"]))
                    self.runs.setdefault(h, (run.role, run.blocks, len(run.blocks)))

    def resolve(self, h):
        """Return (source, message) for a message hash, or (None, None)."""
        if h in self.blobs:
            return "blob", self.blobs[h]
        if h in self.runs:
            role, blocks, n = self.runs[h]
            return "transcript", {"role": role, "content": blocks[:n]}
        return None, None

    def message_hashes(self, req):
        """The message hashes of a request: the first `keep` of its base, then `tail`."""
        rid = req.get("id")
        if rid in self._messages:
            return self._messages[rid]
        chain = []
        r = req
        while r is not None and r.get("id") not in self._messages:
            chain.append(r)
            base = r.get("base")
            r = self.by_id.get(base) if base else None
            if base and r is None:
                chain[-1] = dict(chain[-1], _missing_base=True)
        prev = self._messages.get(r.get("id")) if r is not None else None
        for r in reversed(chain):
            if r.get("_missing_base"):
                hashes = None
            elif r.get("base"):
                hashes = None if prev is None else prev[: r.get("keep", 0)] + list(r.get("tail", []))
            else:
                hashes = list(r.get("tail", []))
            self._messages[r.get("id")] = hashes
            prev = hashes
        return self._messages[rid]

    def rebuild(self, req):
        """The request body as the record describes it, or None when a piece is missing.

        Returns (body, missing) where missing lists what could not be resolved."""
        missing = []
        shape = self.shapes.get(req.get("shapeHash"))
        if shape is None:
            missing.append("shape")
        hashes = self.message_hashes(req)
        if hashes is None:
            missing.append("base")
            hashes = []
        messages = []
        for h in hashes:
            _, m = self.resolve(h)
            if m is None:
                missing.append("message")
                messages.append(None)
            else:
                messages.append(strip_cache_control(m))
        if missing:
            return None, missing
        body = dict(req.get("params") or {})
        shape = json.loads(json.dumps(shape))
        if "system" in shape:
            body["system"] = shape["system"]
        if "tools" in shape:
            body["tools"] = shape["tools"]
        body["messages"] = messages
        for mark in req.get("cacheControl", []):
            path, value = mark["path"], mark["value"]
            node = body
            for i, step in enumerate(path):
                if isinstance(node, list) and isinstance(step, int) and step < len(node):
                    if path[0] == "messages" and i == 1:
                        node[step] = dict(node[step], content=as_blocks(node[step]["content"]))
                    node = node[step]
                elif isinstance(node, dict) and step in node:
                    node = node[step]
                else:
                    return None, ["cache path"]
            node["cache_control"] = value
        return body, []

    def final_usage(self, rid):
        """The usage of the last reply record of a request (section 6)."""
        recs = self.replies.get(rid) or []
        return recs[-1]["message"].get("usage") if recs else None
