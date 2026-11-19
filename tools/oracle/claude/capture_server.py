"""Local HTTP sink that records every request Claude Code sends, then fails it.

Usage: python3 -I capture_server.py <out_dir> <port>
Writes <out_dir>/req-<n>.json (method, path, body) and answers 400 so the CLI stops.
"""
import http.server
import json
import os
import sys

OUT = sys.argv[1]
PORT = int(sys.argv[2])
os.makedirs(OUT, exist_ok=True)
counter = {"n": 0}


class Handler(http.server.BaseHTTPRequestHandler):
    def _handle(self):
        length = int(self.headers.get("content-length") or 0)
        body = self.rfile.read(length) if length else b""
        counter["n"] += 1
        rec = {"method": self.command, "path": self.path}
        try:
            rec["body"] = json.loads(body) if body else None
        except Exception:
            rec["body_raw"] = body.decode("utf-8", "replace")
        with open(os.path.join(OUT, f"req-{counter['n']:03d}.json"), "w") as f:
            json.dump(rec, f, indent=1)
        payload = json.dumps({"type": "error", "error": {"type": "invalid_request_error", "message": "capture sink"}}).encode()
        self.send_response(400)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)

    do_POST = _handle
    do_GET = _handle

    def log_message(self, *a):
        pass


http.server.ThreadingHTTPServer(("127.0.0.1", PORT), Handler).serve_forever()
