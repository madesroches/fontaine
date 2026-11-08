#!/usr/bin/env python3
"""Logging reverse proxy to api.anthropic.com: writes each /v1/messages body to bodies/."""
import http.client, json, os, ssl, sys, threading, time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

OUT = sys.argv[2]
os.makedirs(OUT, exist_ok=True)
N = [0]
LOCK = threading.Lock()
HOP = {"host", "connection", "keep-alive", "transfer-encoding", "content-length", "accept-encoding"}


class H(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *a):
        pass

    def _go(self):
        n = int(self.headers.get("content-length") or 0)
        body = self.rfile.read(n) if n else b""
        if self.path.startswith("/v1/messages") and body:
            with LOCK:
                N[0] += 1
                i = N[0]
            open(os.path.join(OUT, f"{int(time.time()*1000)}-{i}.json"), "wb").write(body)
        c = http.client.HTTPSConnection("api.anthropic.com", context=ssl.create_default_context(), timeout=300)
        hdrs = {k: v for k, v in self.headers.items() if k.lower() not in HOP}
        c.request(self.command, self.path, body=body, headers=hdrs)
        r = c.getresponse()
        self.send_response(r.status)
        for k, v in r.getheaders():
            if k.lower() not in HOP:
                self.send_header(k, v)
        self.send_header("transfer-encoding", "chunked")
        self.end_headers()
        while True:
            chunk = r.read1(65536) if hasattr(r, "read1") else r.read(65536)
            if not chunk:
                break
            self.wfile.write(b"%x\r\n%s\r\n" % (len(chunk), chunk))
            self.wfile.flush()
        self.wfile.write(b"0\r\n\r\n")
        self.wfile.flush()

    do_POST = _go
    do_GET = _go


if __name__ == "__main__":
    ThreadingHTTPServer(("127.0.0.1", int(sys.argv[1])), H).serve_forever()
