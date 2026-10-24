import os
import json, sys
from http.server import BaseHTTPRequestHandler, HTTPServer
out = open(sys.argv[1], "a")
class H(BaseHTTPRequestHandler):
    def do_POST(self):
        body = self.rfile.read(int(self.headers["Content-Length"]))
        out.write(body.decode() + "\n"); out.flush()
        resp = json.dumps({"answers": {"verdict": {"type": "choice", "choice": "allow", "confidence": 0.9}}}).encode()
        self.send_response(200); self.send_header("Content-Type", "application/json"); self.send_header("Content-Length", str(len(resp))); self.end_headers(); self.wfile.write(resp)
    def do_GET(self):
        self.send_response(200); self.end_headers()
    def log_message(self, *a): pass
HTTPServer(("127.0.0.1", 8011), H).serve_forever()
