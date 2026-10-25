#!/usr/bin/env python3
"""E8: for a Fontaine Chat-wire capture, drop guardian requests (thinking disabled / tools<=1), then check that every
assistant turn whose response carried reasoning_content was sent back with reasoning_content in the next request."""
import json, sys, re
ex = [json.loads(l) for l in open(sys.argv[1])]
main = [r for r in ex if isinstance(r["req_body"], dict) and "messages" in r["req_body"] and len(r["req_body"].get("tools", [])) > 1]
guard = [r for r in ex if isinstance(r["req_body"], dict) and (r["req_body"].get("thinking") == {"type": "disabled"})]
print(f"main requests: {len(main)}  guardian/screen requests (thinking disabled): {len(guard)}  statuses: {sorted({r['status'] for r in ex})}")
def resp_reasoning(r):
    return "reasoning_content" in r["resp_body"]
sent_missing = 0; turns_with_reason_sent = 0
for i in range(1, len(main)):
    prev_resp = resp_reasoning(main[i - 1])
    asst = [m for m in main[i]["req_body"]["messages"] if m["role"] == "assistant"]
    last = asst[-1] if asst else {}
    has = bool(last.get("reasoning_content"))
    print(f"request {i}: previous response had reasoning_content={prev_resp}; last assistant turn resent with reasoning_content={has}; assistant turns={len(asst)}; with_reasoning_field={sum(1 for m in asst if 'reasoning_content' in m)}")
    if prev_resp and not has: sent_missing += 1
    if prev_resp and has: turns_with_reason_sent += 1
print(f"reasoning-bearing turns round-tripped: {turns_with_reason_sent}; reasoning-bearing turns dropped: {sent_missing}")
