#!/usr/bin/env python3
"""Replay-shape probes: does kimi-for-coding accept an assistant tool-call turn without reasoning_content (as a history built on a
non-reasoning model would carry), with an empty-string reasoning_content, and with content omitted vs "" ?
usage: probe_history.py NAME"""
import json, os, sys, urllib.request, urllib.error
HERE = os.path.dirname(os.path.abspath(__file__)); RUNS = os.path.join(HERE, "..", "runs")
name = sys.argv[1]; os.makedirs(os.path.join(RUNS, name), exist_ok=True)
open(os.path.join(RUNS, "CURRENT"), "w").write(name)
URL = "http://172.17.0.1:18080/chat/completions"
TOOLS = [{"type": "function", "function": {"name": "get_time", "description": "Current time", "parameters": {"type": "object", "properties": {}}}}]
def hist(assistant_extra, content=None):
    a = {"role": "assistant", "tool_calls": [{"id": "call_1", "type": "function", "function": {"name": "get_time", "arguments": "{}"}}], **assistant_extra}
    if content is not None: a["content"] = content
    return [{"role": "user", "content": "What time is it? Use the tool."}, a, {"role": "tool", "tool_call_id": "call_1", "content": "12:00 UTC"}]
def call(label, messages, extra={}):
    body = {"model": "kimi-for-coding", "messages": messages, "tools": TOOLS, "stream": False, "max_completion_tokens": 300, **extra}
    req = urllib.request.Request(URL, json.dumps(body).encode(), {"content-type": "application/json", "authorization": "Bearer kimi-dummy-key", "user-agent": "fontaine-study/1"})
    try:
        r = json.load(urllib.request.urlopen(req, timeout=120)); out = {"label": label, "status": 200, "content": r["choices"][0]["message"].get("content"), "finish": r["choices"][0]["finish_reason"]}
    except urllib.error.HTTPError as e:
        out = {"label": label, "status": e.code, "error": e.read().decode()[:300]}
    print(json.dumps(out)); return out
res = [
  call("reasoning_present", hist({"reasoning_content": "I should call get_time."})),
  call("reasoning_absent", hist({})),
  call("reasoning_empty_string", hist({"reasoning_content": ""})),
  call("reasoning_absent_content_empty_string", hist({}, content="")),
  call("reasoning_absent_thinking_disabled", hist({}), {"thinking": {"type": "disabled"}}),
]
json.dump(res, open(os.path.join(RUNS, name, "probe.json"), "w"), indent=1)
