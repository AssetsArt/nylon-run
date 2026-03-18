#!/usr/bin/env python3
"""Simple HTTP server that listens on PORT env var (default 8000)."""
import os
import http.server
import json

port = int(os.environ.get("PORT", "8000"))

class Handler(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.end_headers()
        body = json.dumps({"status": "ok", "port": port, "path": self.path})
        self.wfile.write(body.encode())

    def log_message(self, format, *args):
        pass  # suppress logs

print(f"Listening on :{port}", flush=True)
http.server.HTTPServer(("0.0.0.0", port), Handler).serve_forever()
