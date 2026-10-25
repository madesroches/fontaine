#!/usr/bin/env python3
"""Hand-built probes of the Go gateway's Chat and Messages paths for DeepSeek model-support items.
Sends to the host proxy's dsh listener with `Authorization: Bearer dsh-dummy-key` (the proxy injects the real token).
Output: one JSON line per probe on stdout and captures/probe_results.json."""
import json, sys, urllib.request, urllib.error, os, time
BASE = "http://172.17.0.1:18080"
MODEL = os.environ.get("PROBE_MODEL", "deepseek-v4.1-flash")
TOOL = {"type": "function", "function": {"name": "get_time", "description": "Current time in a city.", "parameters": {"type": "object", "properties": {"city": {"type": "string"}}, "required": ["city"]}}}

def post(path, body, hdr=None):
    h = {"content-type": "application/json", "authorization": "Bearer dsh-dummy-key", "x-opencode-session": "dsh-study-probe", "user-agent": "fontaine-study-probe/0.1"}
    h.update(hdr or {})
    req = urllib.request.Request(BASE + path, json.dumps(body).encode(), h)
    try:
        r = urllib.request.urlopen(req, timeout=120); return r.status, json.loads(r.read())
    except urllib.error.HTTPError as e:
        raw = e.read().decode("utf-8", "replace")
        try: return e.code, json.loads(raw)
        except Exception: return e.code, raw[:300]

def chat(messages, tools=True, **extra):
    b = {"model": MODEL, "messages": messages, "max_tokens": 600}
    if tools: b["tools"] = [TOOL]
    b.update(extra); return post("/openai/v1/chat/completions", b)

def summarize(st, body):
    if st != 200: return {"status": st, "error": (body if isinstance(body, str) else json.dumps(body))[:300]}
    m = body["choices"][0]["message"]; u = body.get("usage", {})
    return {"status": 200, "has_reasoning_content": bool(m.get("reasoning_content")), "tool_calls": len(m.get("tool_calls") or []),
            "reasoning_tokens": u.get("completion_tokens_details", {}).get("reasoning_tokens"), "prompt_tokens": u.get("prompt_tokens"),
            "cached_tokens": u.get("prompt_tokens_details", {}).get("cached_tokens"), "finish": body["choices"][0].get("finish_reason")}

def history(reasoning, tool_call=True, user2="Thanks. Now say hi in one word."):
    asst = {"role": "assistant", "content": ""}
    if tool_call:
        asst["tool_calls"] = [{"id": "call_1", "type": "function", "function": {"name": "get_time", "arguments": "{\"city\": \"Paris\"}"}}]
    else:
        asst["content"] = "Paris is a city in France."
    if reasoning is not None: asst["reasoning_content"] = reasoning
    msgs = [{"role": "user", "content": "What time is it in Paris? Use the tool." if tool_call else "Tell me one fact about Paris."}, asst]
    if tool_call: msgs.append({"role": "tool", "tool_call_id": "call_1", "content": "14:05"})
    msgs.append({"role": "user", "content": user2}); return msgs

THINK = {"thinking": {"type": "enabled"}}
probes = {
 "P1 tool turn WITH reasoning_content, thinking on": lambda: chat(history("I should call the tool."), **THINK),
 "P2 tool turn, reasoning_content ABSENT, thinking on": lambda: chat(history(None), **THINK),
 "P3 tool turn, reasoning_content '' (rig fallback), thinking on": lambda: chat(history(""), **THINK),
 "P4 tool turn, reasoning_content ABSENT, no thinking param (Fontaine default)": lambda: chat(history(None)),
 "P5 NON-tool assistant turn, reasoning ABSENT, tools sent, thinking on": lambda: chat(history(None, tool_call=False), **THINK),
 "P6 non-tool assistant turn, reasoning ABSENT, no tools, thinking on": lambda: chat(history(None, tool_call=False), tools=False, **THINK),
 "P7 tool turn WITH reasoning_content, thinking disabled": lambda: chat(history("I should call the tool."), thinking={"type": "disabled"}),
 "P8 first turn, no thinking param: does the model think by default?": lambda: chat([{"role": "user", "content": "What is 17*23? Think carefully."}], tools=False),
 "P9 first turn, thinking disabled": lambda: chat([{"role": "user", "content": "What is 17*23? Think carefully."}], tools=False, thinking={"type": "disabled"}),
 "P10 temperature=0.2 with thinking on": lambda: chat([{"role": "user", "content": "Say hi."}], tools=False, temperature=0.2, **THINK),
 "P11 top_p=0.1, presence_penalty=1 with thinking on": lambda: chat([{"role": "user", "content": "Say hi."}], tools=False, top_p=0.1, presence_penalty=1.0, **THINK),
}
for eff in ("low", "medium", "high", "max", "xhigh", "none"):
    probes[f"E reasoning_effort={eff} (thinking on)"] = (lambda e=eff: chat([{"role": "user", "content": "What is 17*23?"}], tools=False, reasoning_effort=e, **THINK))
probes["E reasoning_effort=high without thinking param"] = lambda: chat([{"role": "user", "content": "What is 17*23?"}], tools=False, reasoning_effort="high")
def msg(effort, thinking):
    b = {"model": MODEL, "max_tokens": 600, "messages": [{"role": "user", "content": "What is 17*23?"}]}
    if thinking: b["thinking"] = {"type": thinking}
    if effort: b["output_config"] = {"effort": effort}
    st, body = post("/anthropic/v1/messages", b, {"x-api-key": "dsh-dummy-key", "anthropic-version": "2023-06-01", "authorization": ""})
    if st == 200:
        kinds = [c["type"] for c in body.get("content", [])]
        return st, {"choices": [{"message": {"reasoning_content": "thinking" in kinds}, "finish_reason": body.get("stop_reason")}], "usage": {"prompt_tokens": body["usage"]["input_tokens"]}}
    return st, body
for eff in ("low", "high", "max", "xhigh"):
    probes[f"M messages output_config.effort={eff} thinking enabled"] = (lambda e=eff: msg(e, "enabled"))
probes["M messages thinking absent"] = lambda: msg(None, None)
long_user = "Reference text: " + " ".join(f"item{i} alpha beta gamma delta" for i in range(900)) + "\nQuestion: say OK."
probes["C1 cache: long prompt, first send"] = lambda: chat([{"role": "user", "content": long_user}], tools=False)
probes["C2 cache: same prompt again"] = lambda: chat([{"role": "user", "content": long_user}], tools=False)
probes["C3 cache: same prefix, changed trailing question"] = lambda: chat([{"role": "user", "content": long_user.replace("say OK", "say YES")}], tools=False)
probes["C4 cache: system prompt prepended (prefix change)"] = lambda: chat([{"role": "system", "content": "You are terse."}, {"role": "user", "content": long_user}], tools=False)
probes["C5 cache: system message appended AFTER history (in-history update shape)"] = lambda: chat([{"role": "user", "content": long_user}, {"role": "assistant", "content": "OK"}, {"role": "system", "content": "You are terse."}, {"role": "user", "content": "Again."}], tools=False)
sel = sys.argv[1:]
out = {}
for k, fn in probes.items():
    if sel and not any(k.startswith(s) for s in sel): continue
    st, body = fn(); r = summarize(st, body); out[k] = r
    print(json.dumps({k: r}), flush=True); time.sleep(0.3)
os.makedirs(os.path.join(os.path.dirname(__file__), "..", "captures"), exist_ok=True)
if not sel: json.dump(out, open(os.path.join(os.path.dirname(__file__), "..", "captures", "probe_results.json" if MODEL == "deepseek-v4.1-flash" else "probe_results_%s.json" % MODEL), "w"), indent=1)
