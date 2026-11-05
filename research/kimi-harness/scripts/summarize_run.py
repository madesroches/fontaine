#!/usr/bin/env python3
"""Per-request summary of a run's exchanges-*.jsonl: body keys, message shapes, usage, finish reason.
usage: summarize_run.py RUNDIR [tag]"""
import json, sys, os

def sse(body):
    out = []
    if not isinstance(body, str):
        return out
    for line in body.splitlines():
        if line.startswith("data: ") and line[6:].strip() != "[DONE]":
            try: out.append(json.loads(line[6:]))
            except Exception: pass
    return out

def main(run, tag="kimi"):
    for i, l in enumerate(open(os.path.join(run, f"exchanges-{tag}.jsonl"))):
        r = json.loads(l); b = r.get("req_body")
        print(f"#{i} {r['method']} {r['path']} -> {r['status']} {r.get('elapsed')}s")
        if isinstance(b, dict) and "messages" in b:
            print("  keys:", {k: v for k, v in b.items() if k not in ("messages", "tools")})
            print("  tools:", len(b.get("tools") or []))
            for m in b["messages"]:
                c = m.get("content"); n = len(c) if isinstance(c, str) else len(json.dumps(c))
                extra = [k for k in m if k not in ("role", "content")]
                print(f"   {m['role']:9} {n:6}c extra={extra}" + (f" id={m.get('tool_call_id')}" if m.get('tool_call_id') else ""))
        ch = sse(r.get("resp_body")); usage = None; fin = None; reas = 0; txt = 0; calls = []
        for c in ch:
            if c.get("usage"): usage = c["usage"]
            for x in c.get("choices", []):
                d = x.get("delta", {}); reas += len(d.get("reasoning_content") or ""); txt += len(d.get("content") or "")
                for t in d.get("tool_calls") or []:
                    if t.get("function", {}).get("name"): calls.append(t["function"]["name"])
                fin = x.get("finish_reason") or fin
        print(f"  resp: finish={fin} reasoning_chars={reas} text_chars={txt} calls={calls} usage={usage}")

main(sys.argv[1], *(sys.argv[2:3]))
