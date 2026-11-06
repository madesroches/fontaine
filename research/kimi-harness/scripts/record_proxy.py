#!/usr/bin/env python3
"""Recording reverse proxy for the Kimi Code study (stdlib only).

Two listeners on the docker bridge address:
  harness port  replaces `Authorization: Bearer <dummy>` with the real credential when (and only when) the incoming value is the dummy key.
                --auth apikey: flat [accounts.<account>].api_key, no expiry. --auth oauth: [accounts.<account>.oauth].access, 503 near expiry.
                Never refreshes, never writes creds.
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
            acct = tomllib.load(f)["accounts"][A.account]
        if A.auth == "apikey":
            return {"access": acct.get("api_key")}
        return acct["oauth"]
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
    for k in ("access", "refresh", "email", "org_id"):
        if o.get(k):
            s.add(o[k])
    for root, _d, files in os.walk(A.runs):
        for fn in ("device_id", ".anonymous-user-id"):
            if fn in files:
                try:
                    v = open(os.path.join(root, fn)).read().strip()
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
            n = int(self.headers.get("content-length") or 0)
            raw = self.rfile.read(n) if n else b""
            hdrs = {k: v for k, v in self.headers.items()}
            fwd = {k: v for k, v in self.headers.items() if k.lower() not in HOP}
            err = None
            if kind == "harness" and self.headers.get("authorization") == "Bearer " + A.dummy_key:
                o = read_creds()
                tok = o.get("access")
                exp = o.get("expires_at", 0)
                if not tok or (A.auth == "oauth" and exp <= time.time() + 5):
                    err = {"error": {"type": "proxy_token_expired", "message": "credential expired or missing; refresh with a plain `fontaine -p` and rerun"}}
                else:
                    INJECTED.add(tok)
                    fwd["authorization"] = "Bearer " + tok
            # urllib would add its own User-Agent, which the edge bans (error 1010); a client that sends none stays UA-less.
            if not any(k.lower() == "user-agent" for k in fwd):
                fwd["User-Agent"] = ""
            sec = secrets()
            rec = {"t": t0, "listener": kind, "upstream": A.upstream, "method": self.command, "path": self.path,
                   "req_headers": scrub_obj({k.lower(): v for k, v in hdrs.items()}, sec),
                   "req_body": scrub_obj(body_to_obj(
                       gzip.decompress(raw) if self.headers.get("content-encoding") == "gzip" and raw else raw, None), sec)}
            if err:
                out = json.dumps(err).encode()
                self.send_response(503); self.send_header("content-type", "application/json")
                self.send_header("content-length", str(len(out))); self.end_headers(); self.wfile.write(out)
                rec.update(status=503, resp_body=err, proxy_error=True)
                self._write(rec); return
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
            self._write(rec)

        def _write(self, rec):
            with LOCK:
                os.makedirs(cur_dir(), exist_ok=True)
                with open(os.path.join(cur_dir(), "exchanges-%s.jsonl" % A.tag), "a") as f:
                    f.write(json.dumps(rec) + "\n")

        do_GET = do_POST = do_PUT = do_DELETE = _handle
    return H


def main():
    global A
    p = argparse.ArgumentParser()
    p.add_argument("--upstream", required=True)
    p.add_argument("--account", required=True, help="credentials.toml account name")
    p.add_argument("--auth", choices=["apikey", "oauth"], required=True)
    p.add_argument("--tag", required=True, help="suffix of exchanges-<tag>.jsonl")
    p.add_argument("--bind", default="172.17.0.1")
    p.add_argument("--harness-port", type=int, default=18080)
    p.add_argument("--fontaine-port", type=int, default=18081)
    p.add_argument("--runs", required=True, help="runs/ dir; runs/CURRENT names the active run subdir")
    p.add_argument("--run-dir", default="")
    p.add_argument("--creds", default=os.path.expanduser("~/.local/share/fontaine/credentials.toml"))
    p.add_argument("--dummy-key", default="kimi-dummy-key")
    A = p.parse_args()
    A.run_dir = A.run_dir or os.path.join(A.runs, "unattributed")
    for kind, port in (("harness", A.harness_port), ("fontaine", A.fontaine_port)):
        srv = ThreadingHTTPServer((A.bind, port), make_handler(kind))
        srv.daemon_threads = True
        threading.Thread(target=srv.serve_forever, daemon=True).start()
    print(f"proxy up on {A.bind}:{A.harness_port} (harness) {A.fontaine_port} (fontaine)", flush=True)
    while True:
        time.sleep(3600)


if __name__ == "__main__":
    main()
