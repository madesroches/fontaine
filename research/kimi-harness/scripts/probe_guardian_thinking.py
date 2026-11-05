#!/usr/bin/env python3
"""Replay one recorded Fontaine guardian request (tool_choice required, max_tokens 1024) against kimi-for-coding with the default
thinking and with thinking disabled; compare latency, reasoning tokens and the submitted verdict. Not a scorecard (n=3 each).
usage: probe_guardian_thinking.py NAME SOURCE_RUN"""
import json, os, sys, time, urllib.request, urllib.error
HERE = os.path.dirname(os.path.abspath(__file__)); RUNS = os.path.join(HERE, "..", "runs")
name, src = sys.argv[1], sys.argv[2]
os.makedirs(os.path.join(RUNS, name), exist_ok=True); open(os.path.join(RUNS, "CURRENT"), "w").write(name)
rec = [json.loads(l) for l in open(os.path.join(RUNS, src, "exchanges-kimi.jsonl"))]
body = next(r["req_body"] for r in rec if r["req_body"].get("tool_choice") == "required")
def call(extra):
    b = {**body, **extra}
    req = urllib.request.Request("http://172.17.0.1:18080/chat/completions", json.dumps(b).encode(), {"content-type": "application/json", "authorization": "Bearer kimi-dummy-key", "user-agent": "fontaine-study/1"})
    t = time.time()
    try:
        r = json.load(urllib.request.urlopen(req, timeout=120))
        m = r["choices"][0]["message"]; args = json.loads(m["tool_calls"][0]["function"]["arguments"]) if m.get("tool_calls") else {}
        return {"s": round(time.time() - t, 2), "status": 200, "decision": args.get("decision"), "rule": args.get("rule"), "confidence": args.get("confidence"),
                "reasoning_tokens": (r.get("usage", {}).get("completion_tokens_details") or {}).get("reasoning_tokens"), "completion_tokens": r["usage"]["completion_tokens"]}
    except urllib.error.HTTPError as e:
        return {"status": e.code, "error": e.read().decode()[:200]}
res = {"default": [call({}) for _ in range(3)], "thinking_disabled": [call({"thinking": {"type": "disabled"}}) for _ in range(3)]}
print(json.dumps(res, indent=1)); json.dump(res, open(os.path.join(RUNS, name, "probe.json"), "w"), indent=1)
