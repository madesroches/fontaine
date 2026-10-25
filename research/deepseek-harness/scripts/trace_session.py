#!/usr/bin/env python3
"""Compact trace of a dsh session.jsonl: tool calls, results, reasoning, reminders, compaction."""
import json, sys
W = int(sys.argv[2]) if len(sys.argv) > 2 else 260
def t(c):
    if isinstance(c, list): return " ".join(x.get("text", "") if isinstance(x, dict) else str(x) for x in c)
    return str(c)
for l in open(sys.argv[1]):
    r = json.loads(l); ty = r.get("type"); d = r.get("data", {})
    if ty == "assistant/message":
        for c in d["message"]["content"]:
            if c["type"] == "reasoning": print(f"[{r['seq']}] REASON:", c["text"][:W].replace("\n", " "))
            elif c["type"] == "tool-call": print(f"[{r['seq']}] CALL {c['name']}:", c["arguments"][:W].replace("\n", " "))
            elif c["type"] == "text": print(f"[{r['seq']}] TEXT:", c["text"][:W].replace("\n", " "))
    elif ty == "tool/result": print(f"[{r['seq']}] RESULT err={d['message'].get('isError')}:", t(d["message"]["content"])[:W].replace("\n", " | "))
    elif ty == "user/message" and d.get("source", {}).get("kind") not in ("user",) : print(f"[{r['seq']}] USERMSG src={d.get('source')}:", t(d["content"])[:W].replace("\n", " | "))
    elif ty and ty.startswith(("compaction", "goal", "plan", "todo", "approval", "sandbox")): print(f"[{r['seq']}] {ty}:", json.dumps(d)[:W])
