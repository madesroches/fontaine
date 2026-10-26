#!/usr/bin/env python3
"""Recording reverse proxy for the oh-my-pi study (stdlib only).

Two listeners on the docker bridge address:
  omp port      replaces x-api-key (or `Authorization: Bearer <dummy>`, used by the hand-built probes) with the current `go` access token when (and only when)
                the incoming value is the dummy key; never refreshes, never writes creds.
  fontaine port passes Fontaine's own authorization header through unchanged.
Each exchange is appended to <run-dir>/exchanges.jsonl with secrets redacted.
"""
import argparse, gzip, json, os, sys, threading, time, tomllib, urllib.request, urllib.error
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import urlsplit

REDACT_HEADERS = {"authorization", "x-api-key", "api_key", "apikey"}
REDACT_KEYS = {"authorization", "x-api-key", "api_key", "apikey"}
HOP = {"connection", "keep-alive", "proxy-connection", "te", "trailers", "transfer-encoding", "upgrade", "host", "content-length", "accept-encoding"}

A = None
LOCK = threading.Lock()
INJECTED = set()


def read_creds():
    try:
        with open(A.creds, "rb") as f:
            return tomllib.load(f)["accounts"]["go"]["oauth"]
    except Exception:
        return {}


def cur_dir():
    try:
        name = open(os.path.join(A.runs, "CURRENT")).read().strip()
        if name:
            return os.path.join(A.runs, name)
    except OSError:
        pass
    return A.run_dir


def secrets():
    s = {A.dummy_key}
    s |= INJECTED
    o = read_creds()
    for k in ("access", "refresh"):
        if o.get(k):
            s.add(o[k])
    for root, _d, files in os.walk(A.runs):
        if "install-id" in files:
            try:
                v = open(os.path.join(root, "install-id")).read().strip()
                if v:
                    s.add(v)
            except OSError:
                pass
    return {x for x in s if x and len(x) >= 8}


def scrub_text(t, sec):
    for x in sec:
        t = t.replace(x, "[REDACTED]")
    return t


def scrub_obj(o, sec):
    if isinstance(o, dict):
        return {k: ("[REDACTED]" if k.lower() in REDACT_KEYS else scrub_obj(v, sec)) for k, v in o.items()}
    if isinstance(o, list):
        return [scrub_obj(v, sec) for v in o]
    if isinstance(o, str):
        return scrub_text(o, sec)
    return o


def body_to_obj(raw, ctype):
    try:
        text = raw.decode("utf-8", "replace")
    except Exception:
        return None
    try:
        return json.loads(text)
    except Exception:
        return text


def make_handler(kind):
    class H(BaseHTTPRequestHandler):
        protocol_version = "HTTP/1.1"

        def log_message(self, *a):
            pass

        def _handle(self):
            t0 = time.time()
            rdir = cur_dir()  # resolved on arrival: the run may be switched before a slow response finishes
            n = int(self.headers.get("content-length") or 0)
            raw = self.rfile.read(n) if n else b""
            hdrs = {k: v for k, v in self.headers.items()}
            inc_key = self.headers.get("x-api-key")
            fwd = {k: v for k, v in self.headers.items() if k.lower() not in HOP}
            err = None
            if kind == "omp" and inc_key == A.dummy_key:
                tok = read_creds().get("access")
                exp = read_creds().get("expires_at", 0)
                if not tok or exp <= time.time() + 5:
                    err = {"error": {"type": "proxy_token_expired", "message": "go access token expired or missing; refresh with a plain `fontaine -p` and rerun"}}
                else:
                    INJECTED.add(tok)
                    fwd["x-api-key"] = tok
            if kind == "omp" and self.headers.get("authorization") == "Bearer " + A.dummy_key and not err:
                tok = read_creds().get("access"); exp = read_creds().get("expires_at", 0)
                if not tok or exp <= time.time() + 5:
                    err = {"error": {"type": "proxy_token_expired", "message": "go access token expired or missing"}}
                else:
                    INJECTED.add(tok)
                    fwd["authorization"] = "Bearer " + tok
            sec = secrets()
            rec = {"t": t0, "listener": kind, "method": self.command, "path": self.path,
                   "req_headers": scrub_obj({k.lower(): v for k, v in hdrs.items()}, sec),
                   "req_body": scrub_obj(body_to_obj(
                       gzip.decompress(raw) if self.headers.get("content-encoding") == "gzip" and raw else raw, None), sec)}
            if err:
                out = json.dumps(err).encode()
                self.send_response(503); self.send_header("content-type", "application/json")
                self.send_header("content-length", str(len(out))); self.end_headers(); self.wfile.write(out)
                rec.update(status=503, resp_body=err, proxy_error=True)
                self._write(rec, rdir); return
            url = A.upstream.rstrip("/") + self.path
            req = urllib.request.Request(url, data=raw if self.command != "GET" else None, headers=fwd, method=self.command)
            chunks = []
            try:
                try:
                    resp = urllib.request.urlopen(req, timeout=600)
                except urllib.error.HTTPError as e:
                    resp = e
                status = resp.status if hasattr(resp, "status") else resp.code
                rec["status"] = status
                self.send_response(status)
                for k, v in resp.headers.items():
                    if k.lower() not in HOP and k.lower() != "content-encoding":
                        self.send_header(k, v)
                self.send_header("transfer-encoding", "chunked")
                self.end_headers()
                while True:
                    c = resp.read1(8192) if hasattr(resp, "read1") else resp.read(8192)
                    if not c:
                        break
                    chunks.append(c)
                    self.wfile.write(b"%x\r\n" % len(c) + c + b"\r\n"); self.wfile.flush()
                self.wfile.write(b"0\r\n\r\n"); self.wfile.flush()
                rec["status"] = status
                rec["resp_headers"] = scrub_obj({k.lower(): v for k, v in resp.headers.items()}, sec)
            except Exception as e:  # upstream or client failure
                rec.setdefault("status", 502)
                rec["proxy_error"] = scrub_text(repr(e), sec)  # e.g. BrokenPipe: the client hung up after the stream ended
                try:
                    out = json.dumps({"error": "proxy upstream failure"}).encode()
                    self.send_response(502); self.send_header("content-length", str(len(out))); self.end_headers(); self.wfile.write(out)
                except Exception:
                    pass
            body = b"".join(chunks)
            if body[:2] == b"\x1f\x8b":
                try: body = gzip.decompress(body)
                except Exception: pass
            rec["resp_body"] = scrub_obj(body_to_obj(body, None), secrets())
            rec["elapsed"] = round(time.time() - t0, 3)
            self._write(rec, rdir)

        def _write(self, rec, rdir):
            with LOCK:
                os.makedirs(rdir, exist_ok=True)
                with open(os.path.join(rdir, "exchanges.jsonl"), "a") as f:
                    f.write(json.dumps(rec) + "\n")

        do_GET = do_POST = do_PUT = do_DELETE = _handle
    return H


def main():
    global A
    p = argparse.ArgumentParser()
    p.add_argument("--upstream", default="https://opencode.ai/inference/go")
    p.add_argument("--bind", default="172.17.0.1")
    p.add_argument("--omp-port", type=int, default=18090)
    p.add_argument("--fontaine-port", type=int, default=18091)
    p.add_argument("--runs", required=True, help="runs/ dir; runs/CURRENT names the active run subdir")
    p.add_argument("--run-dir", default="")
    p.add_argument("--creds", default=os.path.expanduser("~/.local/share/fontaine/credentials.toml"))
    p.add_argument("--dummy-key", default="omp-dummy-key")
    A = p.parse_args()
    A.run_dir = A.run_dir or os.path.join(A.runs, "unattributed")
    for kind, port in (("omp", A.omp_port), ("fontaine", A.fontaine_port)):
        srv = ThreadingHTTPServer((A.bind, port), make_handler(kind))
        srv.daemon_threads = True
        threading.Thread(target=srv.serve_forever, daemon=True).start()
    print(f"proxy up on {A.bind}:{A.omp_port} (omp) {A.fontaine_port} (fontaine)", flush=True)
    while True:
        time.sleep(3600)


if __name__ == "__main__":
    main()
