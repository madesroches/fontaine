#!/usr/bin/env python3
"""Summarise a run's exchanges.jsonl (Chat Completions SSE): one block per request with the response's text,
reasoning length, tool calls and usage. usage: trace_run.py runs/<name> [--full] [--listener omp|fontaine]"""
import json, sys

def parse_sse(body):
    if isinstance(body, dict): return {"json": body}
    text, reasoning, calls, usage, fin = "", "", {}, None, None
    for line in str(body).splitlines():
        if not line.startswith("data:"): continue
        d = line[5:].strip()
        if d == "[DONE]": continue
        try: j = json.loads(d)
        except Exception: continue
        if j.get("usage"): usage = j["usage"]
        for ch in j.get("choices", []):
            dl = ch.get("delta", {})
            text += dl.get("content") or ""
            reasoning += dl.get("reasoning_content") or dl.get("reasoning") or ""
            for tc in dl.get("tool_calls") or []:
                c = calls.setdefault(tc.get("index", 0), {"name": "", "args": ""})
                f = tc.get("function", {})
                c["name"] += f.get("name") or ""; c["args"] += f.get("arguments") or ""
            fin = ch.get("finish_reason") or fin
    return {"text": text, "reasoning_chars": len(reasoning), "calls": list(calls.values()), "usage": usage, "finish": fin}

def main():
    run = sys.argv[1]; full = "--full" in sys.argv
    lst = sys.argv[sys.argv.index("--listener") + 1] if "--listener" in sys.argv else None
    n = 0
    for line in open(run + "/exchanges.jsonl"):
        r = json.loads(line)
        if lst and r.get("listener") != lst: continue
        n += 1
        b = r.get("req_body")
        msgs = b.get("messages") if isinstance(b, dict) else None
        print(f"--- #{n} {r['method']} {r['path']} status={r.get('status')} elapsed={r.get('elapsed')}")
        if msgs:
            last = msgs[-1]
            c = last.get("content")
            c = c if isinstance(c, str) else json.dumps(c)
            print(f"  request: {len(msgs)} messages, last={last['role']}: {(c if full else c[:300])!r}")
        p = parse_sse(r.get("resp_body"))
        if "json" in p: print("  response json:", json.dumps(p["json"])[:500]); continue
        if p["text"]: print(f"  text: {(p['text'] if full else p['text'][:300])!r}")
        print(f"  reasoning_chars={p['reasoning_chars']} finish={p['finish']}")
        for c in p["calls"]: print(f"  CALL {c['name']} {(c['args'] if full else c['args'][:400])}")
        if p["usage"]: print(f"  usage: {json.dumps(p['usage'])}")
main()
