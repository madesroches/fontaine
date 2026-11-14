#!/usr/bin/env python3
"""Scrubs a recorded stream-json transcript for checking in: machine paths, session and
message ids, the user's installed commands, plugins and agents, and timings."""
import json, os, re, sys

UUID = re.compile(r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}")
EMAIL = re.compile(r"[\w.+-]+@[\w-]+(\.[\w-]+)+")
home = os.path.expanduser("~")
cwd = os.environ.get("SCRUB_CWD", "")
seen = {}


def uuid(m):
    return seen.setdefault(m.group(0), "00000000-0000-4000-8000-%012d" % (len(seen) + 1))


def text(s):
    if cwd:
        s = s.replace(cwd, "/work")
    s = s.replace(home, "/home/user")
    s = EMAIL.sub("user@example.com", s)
    return UUID.sub(uuid, s)


DROP = {"commands", "agents", "plugins", "skills", "slash_commands", "models", "account", "mcp_servers", "plugins"}


def walk(v, key=None):
    if isinstance(v, dict):
        out = {}
        for k, x in v.items():
            if k in DROP and isinstance(x, (list, dict)):
                out[k] = type(x)()
            elif k in ("signature", "credential_expires_at", "organization", "email"):
                out[k] = "scrubbed"
            elif k in ("pid", "duration_ms", "duration_api_ms", "total_cost_usd"):
                out[k] = 0
            else:
                out[k] = walk(x, k)
        return out
    if isinstance(v, list):
        return [walk(x) for x in v]
    if isinstance(v, str):
        return text(v)
    return v


for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    try:
        print(json.dumps(walk(json.loads(line)), separators=(",", ":")))
    except json.JSONDecodeError:
        pass
