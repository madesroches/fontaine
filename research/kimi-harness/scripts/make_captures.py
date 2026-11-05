#!/usr/bin/env python3
"""Write the committed excerpts under captures/ from runs/. Every capture cited in the research doc comes from here or from a probe_*.py run.
Credentials never appear in runs/ exchanges (the proxy redacts them); scrub.py --check captures/ is still run before commit."""
import glob, json, os, subprocess, sys
HERE = os.path.dirname(os.path.abspath(__file__)); ROOT = os.path.join(HERE, ".."); RUNS = os.path.join(ROOT, "runs"); CAP = os.path.join(ROOT, "captures")
os.makedirs(CAP, exist_ok=True)

def w(name, text):
    with open(os.path.join(CAP, name), "w") as f: f.write(text if text.endswith("\n") else text + "\n")

def ex(run, tag="kimi"):
    p = os.path.join(RUNS, run, f"exchanges-{tag}.jsonl")
    return [json.loads(l) for l in open(p)] if os.path.exists(p) else []

def sh(*a):
    return subprocess.run(a, capture_output=True, text=True).stdout

def summarize(run, tag="kimi"):
    return sh("python3", "-I", os.path.join(HERE, "summarize_run.py"), os.path.join(RUNS, run), tag)

# E1 -------------------------------------------------------------------------
m = ex("e1-models", "kimi")[0]["resp_body"]["data"]
w("e1_models_kimi_coding.json", json.dumps([{k: x.get(k) for k in ("id", "display_name", "context_length", "supports_reasoning", "supports_thinking_type", "think_efforts", "supports_dynamic_tools", "modalities", "limit")} for x in m], indent=1))
g = [x for x in ex("e1-models", "go")[0]["resp_body"]["data"] if "kimi" in x["id"]]
w("e1_models_go_kimi.json", json.dumps(g, indent=1))
for r in ("e1-boot", "e1-off"):
    w(f"e1_egress_{r}.txt", sh("python3", "-I", os.path.join(HERE, "pcap_endpoints.py"), os.path.join(RUNS, r, "egress.pcap")) + "\n" + open(os.path.join(RUNS, r, "locale.txt")).read())
tel = sorted(glob.glob(os.path.join(RUNS, "e1-boot", "kimi-home", "telemetry", "*.jsonl")))
w("e1_telemetry_event.json", json.dumps(json.loads(open(tel[0]).readline()), indent=1))
w("e1_files_telemetry_on.txt", sh("sh", "-c", f"cd {RUNS}/e1-boot/kimi-home && find . -type f | grep -v -E 'sessions/|cache/' | sort"))
w("e1_files_telemetry_off.txt", sh("sh", "-c", f"cd {RUNS}/e1-off/kimi-home && find . -type f | grep -v -E 'sessions/|cache/' | sort"))

# E2 -------------------------------------------------------------------------
r0 = ex("e2-kc")[0]; b = r0["req_body"]
w("e2_request_headers.json", json.dumps(r0["req_headers"], indent=1))
w("e2_request_body_keys.json", json.dumps({k: v for k, v in b.items() if k not in ("messages", "tools")}, indent=1))
w("e2_system_prompt.txt", b["messages"][0]["content"])
w("e2_user_reminders.json", json.dumps(b["messages"][1:], indent=1))
w("e2_tool_names.txt", "\n".join(t["function"]["name"] for t in b["tools"]))
w("e2_tool_schemas_sample.json", json.dumps([t for t in b["tools"] if t["function"]["name"] in ("Bash", "Edit", "Read", "UpdateGoal", "TodoList")], indent=1))
w("e2_summary_kc.txt", summarize("e2-kc"))
nos = ex("e2-go-nosession", "go")[0]
w("e2_go_nosession_400.json", json.dumps({"path": nos["path"], "status": nos["status"], "body": nos["resp_body"], "req_keys": {k: v for k, v in nos["req_body"].items() if k not in ("messages", "tools")}, "n_tools": len(nos["req_body"]["tools"])}, indent=1))
g2 = ex("e2-go", "go")[0]
w("e2_go_429.json", json.dumps({"path": g2["path"], "status": g2["status"], "body": g2["resp_body"], "retry_after_s": g2["resp_headers"].get("retry-after"), "attempts_recorded": len(ex("e2-go", "go"))}, indent=1))

# E3 -------------------------------------------------------------------------
w("e3_fix_summary.txt", summarize("e3-fix"))
w("e3_fix_diff.txt", sh("diff", "-u", os.path.join(HERE, "..", "fixtures", "repo-failing", "calc.py"), os.path.join(RUNS, "e3-fix", "ws", "calc.py")))
for n in ("e3-repeat", "e3-repeat2"):
    rs = ex(n); msgs = rs[1]["req_body"]["messages"]
    a = [x for x in msgs if x["role"] == "assistant"][0]
    tools = [x for x in msgs if x["role"] == "tool"]
    w(f"{n}_tool_calls_and_results.json", json.dumps({"assistant_tool_calls": len(a["tool_calls"]), "first_call": a["tool_calls"][0]["function"], "tool_results": [t["content"] for t in tools]}, indent=1))
rs = ex("e3-hung")
w("e3_hung_tool_results.json", json.dumps([x["content"] for r in rs[1:2] + rs[-1:] for x in r["req_body"]["messages"] if x["role"] == "tool"], indent=1))
w("e3_hung_final_answer.txt", open(os.path.join(RUNS, "e3-hung", "kimi.out")).read())

w("e1_stream_json.txt", open(os.path.join(RUNS, "e1-json", "kimi.out")).read())

# E4 -------------------------------------------------------------------------
w("e4_hostile_answer.txt", open(os.path.join(RUNS, "e4-hostile", "kimi.out")).read())
w("e4_hostile_calls.txt", "\n".join(json.dumps(t["function"]) for r in ex("e4-hostile") for x in r["req_body"]["messages"] if x["role"] == "assistant" for t in (x.get("tool_calls") or [])))
w("e4_decoy_after.txt", sh("sh", "-c", f"ls -la {RUNS}/e4-hostile/decoy; ls {RUNS}/e4-hostile/ws"))

# E5 -------------------------------------------------------------------------
rs = ex("e5-compact")
tool1 = [x for x in rs[1]["req_body"]["messages"] if x["role"] == "tool"][0]["content"]
w("e5_large_result_stub.txt", f"(stub is {len(tool1)} characters)\n" + tool1)
w("e5_summary.txt", summarize("e5-compact"))
w("e5_compaction_request_tail.txt", rs[6]["req_body"]["messages"][-1]["content"])
txt = ""
for x in rs[6]["resp_body"].splitlines():
    if x.startswith("data: {"):
        j = json.loads(x[6:])
        if j.get("choices"): txt += j["choices"][0].get("delta", {}).get("content") or ""
w("e5_compaction_summary_output.txt", txt)
w("e5_post_compaction_messages.txt", "\n".join(f"{x['role']}: " + (x['content'] if isinstance(x['content'], str) else json.dumps(x['content']))[:300].replace("\n", " ") for x in rs[7]["req_body"]["messages"]))

# E6 / E7 --------------------------------------------------------------------
w("e6_goal_stderr.txt", open(os.path.join(RUNS, "e6-goal", "kimi.err")).read())
w("e6_goal_summary.txt", summarize("e6-goal"))
w("e6_todo_summary.txt", summarize("e6-todo"))
w("e7_resume_summary.txt", summarize("e7-resume"))
w("e7_repair_log.txt", sh("sh", "-c", f"grep -h 'corrupted wire' {RUNS}/e7-repair/kimi-home/sessions/*/*/logs/kimi-code.log; ls {RUNS}/e7-repair/kimi-home/sessions/*/*/agents/main/"))
wire = glob.glob(os.path.join(RUNS, "e3-fix", "kimi-home", "sessions", "wd_*", "session_*", "agents", "main", "wire.jsonl"))[0]
cnt = {}
lines = []
for l in open(wire):
    o = json.loads(l); cnt[o["type"]] = cnt.get(o["type"], 0) + 1
    if o["type"] in ("metadata", "environment.set_binding", "permission.set_mode", "turn.prompt", "llm.request", "usage.record", "file_history.tracked", "turn.ended") and cnt[o["type"]] == 1:
        if o["type"] == "turn.prompt": o["input"] = "[trimmed]"
        lines.append(json.dumps(o)[:700])
w("e7_wire_record_types.txt", json.dumps(cnt, indent=1) + "\n\nfirst record of selected types:\n" + "\n".join(lines))
sess = glob.glob(os.path.join(RUNS, "e3-fix", "kimi-home", "sessions", "wd_*", "session_*", "state.json"))[0]
w("e7_state_json.json", open(sess).read())

# E8 / E9 --------------------------------------------------------------------
for n in ("e8-probe", "e9-history", "e9-dynamic", "e9-guardian-thinking"):
    w(f"{n.replace('-', '_')}.json", open(os.path.join(RUNS, n, "probe.json")).read())
for n in ("e9-e2", "e9-e3", "e9-effort", "e9-resume", "e9-resume-same"):
    w(f"{n.replace('-', '_')}_summary.txt", summarize(n))
g0 = ex("e9-e2")[1]
w("e9_guardian_request_keys.json", json.dumps({k: v for k, v in g0["req_body"].items() if k not in ("messages", "tools")} | {"n_tools": len(g0["req_body"]["tools"]), "tool_choice": g0["req_body"]["tool_choice"]}, indent=1))
w("e9_guardian_response.json", json.dumps(g0["resp_body"], indent=1))
w("e9_effort_stderr.txt", open(os.path.join(RUNS, "e9-effort", "fontaine.err")).read())
for n in ("e9-go-chat", "e9-go-messages"):
    posts = [r for r in ex(n, "go") if r["method"] == "POST"]
    w(f"{n.replace('-', '_')}.json", json.dumps([{"path": r["path"], "status": r["status"], "error": (r["resp_body"] or {}).get("error"), "headers_sent": sorted(h for h in r["req_headers"] if h.startswith(("x-", "anthropic"))), "body_keys": {k: (v if k not in ("messages", "tools", "system") else f"<{len(v)} items>") for k, v in r["req_body"].items()}} for r in posts], indent=1))
for n in ("e10-big", "e10-hung"):
    p = os.path.join(RUNS, n, "claude.jsonl")
    if os.path.exists(p):
        out = []
        for l in open(p):
            o = json.loads(l)
            if o.get("type") == "user":
                for c in o["message"]["content"]:
                    if c.get("type") == "tool_result":
                        t = c["content"] if isinstance(c["content"], str) else json.dumps(c["content"])
                        out.append(t[:600] + (" [...]" if len(t) > 600 else ""))
            if o.get("type") == "result": out.append("FINAL: " + o.get("result", "")[:900])
        w(f"{n.replace('-', '_')}.txt", "\n---\n".join(out))
subprocess.run(["python3", "-I", os.path.join(HERE, "scrub.py"), CAP])  # literal device ids from runs/*/kimi-home
print("captures written:", len(os.listdir(CAP)))
