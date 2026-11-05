#!/usr/bin/env python3
"""Hand-built Chat probes against the Kimi coding endpoint through the recording proxy (dummy key; the proxy injects the real one).
usage: probe_kimi.py NAME   -> writes runs/NAME/probe.json (summary); the raw exchanges land in the proxy's exchanges-kimi.jsonl"""
import json, os, sys, urllib.request, urllib.error
HERE = os.path.dirname(os.path.abspath(__file__))
RUNS = os.path.join(HERE, "..", "runs")
name = sys.argv[1]
os.makedirs(os.path.join(RUNS, name), exist_ok=True)
open(os.path.join(RUNS, "CURRENT"), "w").write(name)
URL = "http://172.17.0.1:18080/chat/completions"
Q = [{"role": "user", "content": "How many prime numbers are there between 100 and 130? Answer with just the number."}]

def call(label, extra, messages=Q, model="kimi-for-coding"):
    body = {"model": model, "messages": messages, "stream": False, **extra}
    req = urllib.request.Request(URL, json.dumps(body).encode(), {"content-type": "application/json", "authorization": "Bearer kimi-dummy-key", "user-agent": "fontaine-study/1"})
    try:
        r = json.load(urllib.request.urlopen(req, timeout=120)); st = 200
    except urllib.error.HTTPError as e:
        st = e.code; r = e.read().decode()[:400]
        try: r = json.loads(r)
        except Exception: pass
    out = {"label": label, "extra": extra, "status": st}
    if isinstance(r, dict) and "choices" in r:
        m = r["choices"][0]["message"]
        out.update(content=m.get("content"), reasoning_chars=len(m.get("reasoning_content") or ""), usage=r.get("usage"), finish=r["choices"][0].get("finish_reason"))
    else:
        out["error"] = r
    print(json.dumps(out)); return out

res = [
    call("default", {}),
    call("effort_low", {"thinking": {"type": "enabled", "effort": "low"}}),
    call("effort_max", {"thinking": {"type": "enabled", "effort": "max"}}),
    call("thinking_disabled", {"thinking": {"type": "disabled"}}),
    call("temperature_0.2", {"temperature": 0.2}),
    call("max_tokens_legacy", {"max_tokens": 64}),
    call("max_completion_tokens", {"max_completion_tokens": 64}),
    call("prompt_cache_key", {"prompt_cache_key": "study-probe"}),
]
json.dump(res, open(os.path.join(RUNS, name, "probe.json"), "w"), indent=1)
