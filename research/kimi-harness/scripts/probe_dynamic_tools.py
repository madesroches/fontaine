#!/usr/bin/env python3
"""Progressive tool disclosure on the wire: a mid-history `role: system` message that carries a `tools` array (what kimi-code emits
after its select_tools call), with only select_tools in the top-level tools[]. Does kimi-for-coding then call the loaded tool?
usage: probe_dynamic_tools.py NAME"""
import json, os, sys, urllib.request, urllib.error
HERE = os.path.dirname(os.path.abspath(__file__)); RUNS = os.path.join(HERE, "..", "runs")
name = sys.argv[1]; os.makedirs(os.path.join(RUNS, name), exist_ok=True)
open(os.path.join(RUNS, "CURRENT"), "w").write(name)
URL = "http://172.17.0.1:18080/chat/completions"
fn = lambda n, d, p: {"type": "function", "function": {"name": n, "description": d, "parameters": p}}
SELECT = fn("select_tools", "Load tools by exact name.", {"type": "object", "properties": {"names": {"type": "array", "items": {"type": "string"}}}, "required": ["names"]})
SHOT = fn("take_screenshot", "Capture the screen and return the file path.", {"type": "object", "properties": {}})
base = [{"role": "system", "content": "You are a helpful agent. Loadable tools: take_screenshot (use select_tools to load before calling)."},
        {"role": "user", "content": "Take a screenshot of the screen."},
        {"role": "assistant", "tool_calls": [{"id": "call_a", "type": "function", "function": {"name": "select_tools", "arguments": "{\"names\":[\"take_screenshot\"]}"}}], "reasoning_content": "Load the tool first."},
        {"role": "tool", "tool_call_id": "call_a", "content": "Loaded: take_screenshot"}]
def call(label, messages, tools):
    body = {"model": "kimi-for-coding", "messages": messages, "tools": tools, "stream": False, "max_completion_tokens": 400}
    req = urllib.request.Request(URL, json.dumps(body).encode(), {"content-type": "application/json", "authorization": "Bearer kimi-dummy-key", "user-agent": "fontaine-study/1"})
    try:
        r = json.load(urllib.request.urlopen(req, timeout=120)); m = r["choices"][0]["message"]
        out = {"label": label, "status": 200, "tool_calls": [t["function"]["name"] for t in m.get("tool_calls") or []], "content": (m.get("content") or "")[:120], "usage": r.get("usage")}
    except urllib.error.HTTPError as e:
        out = {"label": label, "status": e.code, "error": e.read().decode()[:300]}
    print(json.dumps(out)); return out
res = [
  call("system_tools_message", base + [{"role": "system", "tools": [SHOT]}], [SELECT]),
  call("control_no_declaration", base, [SELECT]),
]
json.dump(res, open(os.path.join(RUNS, name, "probe.json"), "w"), indent=1)
