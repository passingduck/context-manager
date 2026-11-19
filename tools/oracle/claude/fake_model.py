"""Local stand-in for the Messages API that scripts Read tool calls, to observe lazy context loading.

Usage: python3 -I fake_model.py <out_dir> <port> <plan.json>
plan.json is a list of absolute file paths. For the main-loop request (one that offers the Read
tool), step k = number of tool_use blocks already in the conversation; the server answers with a
Read of plan[k], or with plain text once the plan is used up. Every request is saved to out_dir.
"""
import http.server
import json
import os
import sys

OUT, PORT, PLAN = sys.argv[1], int(sys.argv[2]), json.load(open(sys.argv[3]))
os.makedirs(OUT, exist_ok=True)
state = {"n": 0}


def sse(events):
    return "".join(f"event: {e['type']}\ndata: {json.dumps(e)}\n\n" for e in events).encode()


def message(blocks, stop):
    ev = [{"type": "message_start", "message": {"id": "msg_fake", "type": "message", "role": "assistant",
            "model": "claude-opus-5-5", "content": [], "stop_reason": None, "stop_sequence": None,
            "usage": {"input_tokens": 10, "output_tokens": 1}}}]
    for i, b in enumerate(blocks):
        if b["type"] == "tool_use":
            ev.append({"type": "content_block_start", "index": i,
                       "content_block": {"type": "tool_use", "id": b["id"], "name": b["name"], "input": {}}})
            ev.append({"type": "content_block_delta", "index": i,
                       "delta": {"type": "input_json_delta", "partial_json": json.dumps(b["input"])}})
        else:
            ev.append({"type": "content_block_start", "index": i, "content_block": {"type": "text", "text": ""}})
            ev.append({"type": "content_block_delta", "index": i, "delta": {"type": "text_delta", "text": b["text"]}})
        ev.append({"type": "content_block_stop", "index": i})
    ev.append({"type": "message_delta", "delta": {"stop_reason": stop, "stop_sequence": None}, "usage": {"output_tokens": 5}})
    ev.append({"type": "message_stop"})
    return ev


class H(http.server.BaseHTTPRequestHandler):
    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers.get("content-length") or 0)) or b"null")
        state["n"] += 1
        with open(os.path.join(OUT, f"req-{state['n']:03d}.json"), "w") as f:
            json.dump({"path": self.path, "body": body}, f, indent=1)
        tools = [t.get("name") for t in (body or {}).get("tools", [])] if isinstance(body, dict) else []
        if "count_tokens" in self.path:
            payload = json.dumps({"input_tokens": 100}).encode()
            self.send_response(200); self.send_header("content-type", "application/json")
            self.send_header("content-length", str(len(payload))); self.end_headers(); self.wfile.write(payload)
            return
        if "Read" in tools:
            used = sum(1 for m in body["messages"] if m["role"] == "assistant" and isinstance(m["content"], list)
                       for c in m["content"] if c.get("type") == "tool_use")
            if used < len(PLAN):
                blocks, stop = [{"type": "tool_use", "id": f"toolu_{used:03d}", "name": "Read",
                                 "input": {"file_path": PLAN[used]}}], "tool_use"
            else:
                blocks, stop = [{"type": "text", "text": "done"}], "end_turn"
        else:
            blocks, stop = [{"type": "text", "text": "ok"}], "end_turn"
        payload = sse(message(blocks, stop))
        self.send_response(200)
        self.send_header("content-type", "text/event-stream")
        self.send_header("content-length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)

    def log_message(self, *a):
        pass


http.server.ThreadingHTTPServer(("127.0.0.1", PORT), H).serve_forever()
