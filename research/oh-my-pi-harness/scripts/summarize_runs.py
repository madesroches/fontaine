#!/usr/bin/env python3
"""One row per run: model requests (200 chat completions, guardian/judge calls included), tool calls, prompt/completion tokens.
usage: summarize_runs.py runs/<name>... """
import json, sys
sys.path.insert(0, __file__.rsplit("/", 1)[0])
def sse(body):
    calls, usage = 0, None
    for line in str(body).splitlines():
        if line.startswith("data:") and line[5:].strip() not in ("", "[DONE]"):
            try: j = json.loads(line[5:])
            except Exception: continue
            if j.get("usage"): usage = j["usage"]
            for ch in j.get("choices", []):
                for tc in ch.get("delta", {}).get("tool_calls") or []:
                    if tc.get("function", {}).get("name"): calls += 1
    return calls, usage
print(f"{'run':34} {'reqs':>4} {'toolcalls':>9} {'prompt':>7} {'compl':>6}")
for run in sys.argv[1:]:
    reqs = tc = pt = ct = 0
    for line in open(run + "/exchanges.jsonl"):
        r = json.loads(line)
        if r.get("status") != 200 or "completions" not in r["path"]: continue
        c, u = sse(r["resp_body"]); reqs += 1; tc += c
        if u: pt += u.get("prompt_tokens", 0); ct += u.get("completion_tokens", 0)
    print(f"{run.rstrip('/').split('/')[-1]:34} {reqs:>4} {tc:>9} {pt:>7} {ct:>6}")
