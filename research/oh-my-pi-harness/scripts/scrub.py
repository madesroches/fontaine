#!/usr/bin/env python3
"""Scrub or check files for literal secrets. stdlib only; run with `python3 -I`.
Literal values come from: the dummy key, the go account's access/refresh tokens, email and org id in credentials.toml,
and every every runs/*/**/install-id. Values are never printed.
  scrub.py PATH...            replace each literal with [REDACTED] in place
  scrub.py --check PATH...    report file:line for each hit, exit 1 if any (also flags credential-shaped Bearer tokens)"""
import os, re, sys, tomllib

HERE = os.path.dirname(os.path.abspath(__file__))
RUNS = os.path.join(HERE, "..", "runs")
CREDS = os.environ.get("FONTAINE_CREDS", os.path.expanduser("~/.local/share/fontaine/credentials.toml"))

def literals():
    vals = {"omp-dummy-key"}
    try:
        with open(CREDS, "rb") as f:
            acct = tomllib.load(f).get("accounts", {})
        for a in acct.values():
            if isinstance(a, dict):
                o = a.get("oauth", {})
                for k in ("access", "refresh", "email", "org_id"):
                    if o.get(k): vals.add(o[k])
                if a.get("api_key"): vals.add(a["api_key"])
    except OSError:
        pass
    for root, _d, files in os.walk(RUNS):
        if "install-id" in files:
            try:
                v = open(os.path.join(root, "install-id")).read().strip()
                if v: vals.add(v)
            except OSError:
                pass
    return {v for v in vals if len(v) >= 6}

BEARER = re.compile(r"Bearer [A-Za-z0-9._-]{20,}")

def files(paths):
    for p in paths:
        if os.path.isdir(p):
            for r, _d, fs in os.walk(p):
                for f in fs: yield os.path.join(r, f)
        else:
            yield p

def main():
    check = "--check" in sys.argv
    paths = [a for a in sys.argv[1:] if a != "--check"]
    lits = literals(); bad = 0
    for fp in files(paths):
        try: text = open(fp, encoding="utf-8", errors="surrogateescape").read()
        except OSError: continue
        if check:
            for n, line in enumerate(text.splitlines(), 1):
                if any(v in line for v in lits) or BEARER.search(line):
                    print(f"{fp}:{n}"); bad += 1
        else:
            new = text
            for v in lits: new = new.replace(v, "[REDACTED]")
            if new != text:
                open(fp, "w", encoding="utf-8", errors="surrogateescape").write(new); print(f"scrubbed {fp}")
    if check:
        print(f"{bad} hit(s)"); sys.exit(1 if bad else 0)

main()
