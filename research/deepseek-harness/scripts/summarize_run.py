#!/usr/bin/env python3
"""One line per recorded exchange: listener, status, dsh_* fields, body size, message count, usage and cache tokens."""
import json, sys, re
for l in open(sys.argv[1]):
    r = json.loads(l); b = r["req_body"]
    if not isinstance(b, dict): print(r["listener"], r["method"], r["path"], r["status"]); continue
    rb = r["resp_body"] if isinstance(r["resp_body"], str) else json.dumps(r["resp_body"])
    cr = re.findall(r'"cache_read_input_tokens":(\d+)', rb) or re.findall(r'"prompt_cache_hit_tokens":(\d+)', rb)
    it = re.findall(r'"input_tokens":(\d+)', rb) or re.findall(r'"prompt_tokens":(\d+)', rb)
    th = b.get("thinking") or b.get("reasoning_effort")
    print(r["listener"], r["path"], r["status"], "dsh_fields=%s" % [k for k in b if k.startswith("dsh_")], "bytes=%d" % len(json.dumps(b)),
          "msgs=%d" % len(b.get("messages", [])), "tools=%d" % len(b.get("tools", [])), "thinking=%s" % json.dumps(th),
          "input=%s cache_read=%s" % (it[-1:] , cr[-1:]))
