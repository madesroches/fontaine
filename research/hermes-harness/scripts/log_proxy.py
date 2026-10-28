#!/usr/bin/env python3
"""Logging reverse proxy for the Hermes study (issue #253).

Listens on 0.0.0.0:PORT, forwards every request to the local Ollama at
127.0.0.1:11434, and captures request and response bodies (SSE streams
reassembled) as numbered JSON files under the capture dir. Model traffic
inside the study is plain HTTP, so no TLS interception is needed.

Usage: log_proxy.py <capture_dir> [port]
"""
import http.client
import http.server
import json
import os
import sys
import threading
import time

TARGET_HOST = "127.0.0.1"
TARGET_PORT = 11434

counter_lock = threading.Lock()
counter = 0
capture_dir = sys.argv[1]
port = int(sys.argv[2]) if len(sys.argv) > 2 else 18090
os.makedirs(capture_dir, exist_ok=True)


class Handler(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *args):
        pass

    def _handle(self):
        global counter
        length = int(self.headers.get("Content-Length") or 0)
        body = self.rfile.read(length) if length else b""
        with counter_lock:
            counter += 1
            n = counter
        ts = time.strftime("%H:%M:%S")
        conn = http.client.HTTPConnection(TARGET_HOST, TARGET_PORT, timeout=600)
        headers = {k: v for k, v in self.headers.items()
                   if k.lower() not in ("host", "content-length", "connection")}
        try:
            conn.request(self.command, self.path, body=body, headers=headers)
            resp = conn.getresponse()
            resp_body = resp.read()
        except Exception as exc:  # noqa: BLE001 - capture and surface
            self.send_error(502, str(exc))
            return
        record = {
            "n": n,
            "time": ts,
            "method": self.command,
            "path": self.path,
            "request_headers": dict(self.headers),
            "request_body": _maybe_json(body),
            "response_status": resp.status,
            "response_body": _maybe_json(resp_body),
        }
        path = os.path.join(capture_dir, f"req_{n:04d}.json")
        with open(path, "w") as fh:
            json.dump(record, fh, indent=2)
        self.send_response(resp.status)
        for k, v in resp.getheaders():
            if k.lower() in ("connection", "transfer-encoding"):
                continue
            self.send_header(k, v)
        self.send_header("Content-Length", str(len(resp_body)))
        self.end_headers()
        self.wfile.write(resp_body)

    do_GET = _handle
    do_POST = _handle
    do_DELETE = _handle
    do_PUT = _handle


def _maybe_json(raw):
    if not raw:
        return None
    text = raw.decode("utf-8", "replace")
    stripped = text.strip()
    if stripped.startswith("data:"):
        # SSE stream: collect the JSON payloads of data: lines.
        events = []
        for line in stripped.splitlines():
            line = line.strip()
            if line.startswith("data:"):
                payload = line[5:].strip()
                if payload and payload != "[DONE]":
                    try:
                        events.append(json.loads(payload))
                    except json.JSONDecodeError:
                        events.append(payload)
        return {"sse_events": events}
    try:
        return json.loads(text)
    except json.JSONDecodeError:
        return text


class Server(http.server.ThreadingHTTPServer):
    daemon_threads = True


if __name__ == "__main__":
    Server(("0.0.0.0", port), Handler).serve_forever()
