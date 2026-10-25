#!/usr/bin/env python3
"""Write the excerpts the research doc cites from the gitignored runs/ into captures/ (then run scrub.py --check on captures/)."""
import json, os, re, subprocess, sys, collections
H = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..")
R, C = os.path.join(H, "runs"), os.path.join(H, "captures")
os.makedirs(C, exist_ok=True)

def w(name, text):
    open(os.path.join(C, name), "w").write(text if text.endswith("\n") else text + "\n")

def ex(run):
    return [json.loads(l) for l in open(os.path.join(R, run, "exchanges.jsonl"))]

def run_py(script, *args):
    return subprocess.run([sys.executable, "-I", os.path.join(H, "scripts", script), *args], capture_output=True, text=True).stdout

# E1
w("e1_endpoints.txt", run_py("pcap_endpoints.py", os.path.join(R, "e1-bash", "egress.pcap")))
ids = re.findall(r"^- id: (\S+)", open(os.path.join(R, "e1-dump", "config.yml")).read(), re.M)
w("e1_plugin_ids.txt", "composed headless tree, %d rows (dsh --profile headless --dump-config, with the study model patch):\n" % len(ids) + "\n".join(ids))
# E1/E2: one main request, structure only
e = ex("e1-bash")
main = e[1]["req_body"]
w("e1_request_main.json", json.dumps({
    "headers": e[1]["req_headers"],
    "body_keys": list(main.keys()), "thinking": main["thinking"], "output_config": main.get("output_config"), "max_tokens": main["max_tokens"],
    "tool_names": [t["name"] for t in main["tools"]],
    "bash_tool": next(t for t in main["tools"] if t["name"] == "bash"),
    "system_prompt": main["system"][0]["text"] if isinstance(main["system"], list) else main["system"],
    "messages_first_request": main["messages"],
    "dsh_plugin_packages_count": len(main["dsh_plugin_packages"]["packages"]),
    "dsh_session_log_keys": list(main["dsh_session_log"].keys()),
}, indent=1))
t = e[0]["req_body"]
w("e1_request_title.json", json.dumps({"thinking": t["thinking"], "max_tokens": t["max_tokens"], "system": t["system"], "tools": t.get("tools")}, indent=1))
# E2 summaries
out = []
for r in ("e2-default", "e2-nolog"):
    out.append("## " + r); out.append(run_py("summarize_run.py", os.path.join(R, r, "exchanges.jsonl")))
w("e2_session_log_on_off.txt", "\n".join(out))
ev = []
for r in ex("e2-default"): ev += r["req_body"]["dsh_session_log"]["events"]
w("e2_session_log_event_types.txt", "dsh_session_log events across the e2-default run (type: count)\n" + "\n".join(f"{k}: {v}" for k, v in collections.Counter(x["type"] for x in ev).most_common()))
# traces
for run, width in (("e3-fix", 230), ("e3-stall", 300), ("e3-promote", 300), ("e3-repeat", 260), ("e3-repeat2", 330), ("e3-goal", 260), ("e3-goal2", 260), ("e4-ask", 420), ("e4-never", 420), ("e4-ask2", 420), ("e4-never2", 420), ("e5-compact2", 260), ("e6-todo", 260), ("e6-subagent", 300), ("e7-resume", 300)):
    p = os.path.join(R, run, "session.jsonl")
    if not os.path.exists(p):
        subprocess.run([os.path.join(H, "scripts", "dump_session.sh"), run])
    w(f"trace_{run}.txt", run_py("trace_session.py", p, str(width)))
w("e5_usage.txt", "## e5-compact (thresholdRatio 0.05, no compaction)\n" + run_py("summarize_run.py", os.path.join(R, "e5-compact", "exchanges.jsonl")) + "\n## e5-compact2 (thresholdRatio 0.015, compaction fired)\n" + run_py("summarize_run.py", os.path.join(R, "e5-compact2", "exchanges.jsonl")))
w("e3_fix_usage.txt", run_py("summarize_run.py", os.path.join(R, "e3-fix", "exchanges.jsonl")))
w("e7_usage.txt", run_py("summarize_run.py", os.path.join(R, "e7-resume", "exchanges.jsonl")))
# compaction summary text
for l in open(os.path.join(R, "e5-compact2", "session.jsonl")):
    r = json.loads(l)
    if r["type"] == "compaction/summary": w("e5_compaction_summary.txt", "".join(c.get("text", "") for c in r["data"]["summary"]))
# E8
parts = []
for r in ("e8-read", "e8-fix"):
    parts.append("## " + r); parts.append(run_py("check_roundtrip.py", os.path.join(R, r, "exchanges.jsonl")))
w("e8_roundtrip.txt", "\n".join(parts))
w("e8_framing_compare.txt", "## Fontaine (Chat wire) e8-read\n" + run_py("summarize_run.py", os.path.join(R, "e8-read", "exchanges.jsonl")) + "\n## dsh (Messages wire) e2-default\n" + run_py("summarize_run.py", os.path.join(R, "e2-default", "exchanges.jsonl")))
fe = ex("e8-read")[1]["req_body"]
w("e8_fontaine_request.json", json.dumps({"keys": list(fe.keys()), "stream_options": fe.get("stream_options"), "tool_names": [x["function"]["name"] for x in fe["tools"]], "system_prompt": fe["messages"][0]["content"] if isinstance(fe["messages"][0]["content"], str) else fe["messages"][0]["content"]}, indent=1))
rc = ex("e8-resume-chat")
w("e8_resume_foreign_model.txt", "resume of a session built on go/glm-5.1 with --model go/deepseek-v4.1-flash, Chat wire:\n" + run_py("summarize_run.py", os.path.join(R, "e8-resume-chat", "exchanges.jsonl")) + "\nassistant turns in the replayed history:\n" + "\n".join(str({k: str(v)[:60] for k, v in m.items()}) for m in rc[1]["req_body"]["messages"] if m["role"] == "assistant"))
rm = ex("e8-resume-messages")[-1]
w("e8_resume_messages_wire.txt", "same resume on the Messages wire (proxy_messages.toml): status %s\nassistant block types per turn: %s" % (rm["status"], [[c["type"] for c in m["content"]] for m in rm["req_body"]["messages"] if m["role"] == "assistant"]))
# E9
for r in ("e9-repeat", "e9-timeout"):
    w(f"trace_{r}.txt", run_py("trace_claude.py", os.path.join(R, r, "claude.jsonl")))
