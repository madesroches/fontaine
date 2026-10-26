#!/usr/bin/env python3
"""Write the committed excerpts under captures/ from runs/ (which is gitignored). stdlib only; run with `python3 -I`.
Everything written here is then passed through scrub.py and scrub.py --check before commit."""
import json, os, re, shutil, subprocess, sys

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
RUNS = os.path.join(ROOT, "runs")
CAP = os.path.join(ROOT, "captures")
REPO = os.path.abspath(os.path.join(ROOT, "..", ".."))
os.makedirs(CAP, exist_ok=True)

def run(*argv):
    return subprocess.run([sys.executable, "-I", *argv], capture_output=True, text=True, check=False).stdout

def tidy(t):
    return t.replace(REPO, "<repo>").replace(RUNS, "<runs>").rstrip() + "\n"

def write(name, text):
    with open(os.path.join(CAP, name), "w") as f:
        f.write(tidy(text))

def trace(run_name, *flags):
    return run(os.path.join(HERE, "trace_run.py"), os.path.join(RUNS, run_name), *flags)

def exchanges(run_name):
    return [json.loads(l) for l in open(os.path.join(RUNS, run_name, "exchanges.jsonl"))]

def chat_ok(rs, listener=None):
    return [r for r in rs if r.get("status") == 200 and "completions" in r["path"] and (listener is None or r.get("listener") == listener)]

# E1
eg = "== e1-idle (interactive TUI started, no prompt, 25 s)\n" + run(os.path.join(HERE, "pcap_endpoints.py"), os.path.join(RUNS, "e1-idle/egress.pcap"))
eg += "\n== e1-ok (one headless prompt)\n" + run(os.path.join(HERE, "pcap_endpoints.py"), os.path.join(RUNS, "e1-ok/egress.pcap"))
write("e1_egress.txt", eg)
shutil.copy(os.path.join(RUNS, "e1-idle/pane_startup.txt"), os.path.join(CAP, "e1_pane_startup.txt"))
cl = open(os.path.join(RUNS, "e1-idle/config_list.txt")).read().splitlines()
write("e1_config_defaults.txt", "\n".join(l if len(l) < 300 else l[:300] + " ... [truncated]" for l in cl))
logs = subprocess.run(f"rg -N 'model discovery failed' {RUNS}/e1-ok/home/.omp/logs/*.log", shell=True, capture_output=True, text=True).stdout
write("e1_local_discovery_probes.txt", logs)

# E2
o = chat_ok(exchanges("e2-readonly"))[0]
f = chat_ok(exchanges("e2-readonly-fontaine"), "fontaine")[0]
json.dump(o["req_body"], open(os.path.join(CAP, "e2_omp_request.json"), "w"), indent=1)
json.dump(f["req_body"], open(os.path.join(CAP, "e2_fontaine_request.json"), "w"), indent=1)
cmp = []
for n, r in (("omp", o), ("fontaine", f)):
    b = r["req_body"]
    cmp.append(f"{n}: params={ {k: v for k, v in b.items() if k not in ('messages', 'tools')} }")
    cmp.append(f"  tools={[t['function']['name'] for t in b['tools']]} tools_bytes={len(json.dumps(b['tools']))}")
    cmp.append(f"  messages={[(m['role'], len(json.dumps(m))) for m in b['messages']]}")
    cmp.append(f"  headers={ {k: v for k, v in r['req_headers'].items() if k not in ('authorization', 'content-length')} }")
write("e2_framing_compare.txt", "\n".join(cmp))
write("e2_tool_schemas_omp.json", json.dumps(o["req_body"]["tools"], indent=1))

# E3
write("e3_fix_omp.txt", trace("e3-fix"))
write("e3_fix_fontaine.txt", trace("e3-fix-fontaine", "--listener", "fontaine"))
write("e3_loop_redirect.txt", trace("e3-repeat2", "--full"))
write("e3_repeat_unexpected_stop_judge.txt", trace("e3-repeat", "--full"))
write("e3_tool_timeout_bypass.txt", trace("e3-hang", "--full"))
write("e3_edit_auto_repair.txt", trace("e3-repair", "--full"))
write("e3_ttsr.txt", trace("e3-ttsr", "--full"))
write("e3_goal_in_print_mode.txt", trace("e3-goal-print") + "\nomp.out:\n" + open(os.path.join(RUNS, "e3-goal-print/omp.out")).read())
bb = os.path.join(RUNS, "e3-repair/home/.omp/agent/edit-blackbox.jsonl")
if os.path.exists(bb):
    write("e3_edit_blackbox_record.txt", "".join(l[:1500] + "\n" for l in open(bb)))

# E4
lines = []
for who in ("omp-hashline", "omp-default", "fontaine"):
    for k in ("multi", "tabs", "stale", "dup"):
        lines.append(f"######## {k} / {who}")
        lines.append(trace(f"e4-{k}-{who}", "--full", *(["--listener", "fontaine"] if who == "fontaine" else [])))
write("e4_edit_runs.txt", "\n".join(lines))
write("e4_usage.txt", run(os.path.join(HERE, "summarize_runs.py"), *[os.path.join(RUNS, d) for d in sorted(os.listdir(RUNS)) if d.startswith("e4-") and d not in ("e4-lsp", "e4-lsp2")]))
write("e4_lsp_setuptools_missing.txt", trace("e4-lsp2", "--full"))
write("e4_lsp_default_lazy.txt", trace("e4-lsp3", "--full"))
write("e4_lsp_eager_late_diagnostics.txt", trace("e4-lsp4", "--full"))

# E5
for m in ("yolo", "write", "always-ask"):
    write(f"e5_risky_{m}.txt", trace(f"e5-{m}", "--full"))
write("e5_patterns_write_mode.txt", trace("e5-patterns", "--full") + "\nfiles created by the refused commands (ls /tmp/pwn_*):\n" + open(os.path.join(RUNS, "e5-patterns/post.out")).read())
write("e5_interceptor.txt", trace("e5-interceptor", "--full"))

# E6
rs = exchanges("e6-compact")
ok = chat_ok(rs)
write("e6_trace.txt", trace("e6-compact"))
write("e6_usage.txt", run(os.path.join(HERE, "summarize_runs.py"), os.path.join(RUNS, "e6-compact")))
first = ok[2]["req_body"]
m = first["messages"][-1]["content"]
m = m if isinstance(m, str) else "".join(p.get("text", "") for p in m)
write("e6_compaction_request.txt", "SYSTEM:\n" + first["messages"][0]["content"] + "\n\nUSER (tail):\n" + m[-1800:])
after = ok[7]["req_body"]["messages"]
write("e6_after_compaction_messages.txt", "\n---\n".join(f"{x['role']}: " + (x['content'] if isinstance(x['content'], str) else json.dumps(x['content']))[:700] for x in after[2:]))
# E7
write("e7_todo_task.txt", trace("e7-todo-task", "--full"))
child = chat_ok(exchanges("e7-todo-task"))[2]["req_body"]
json.dump({"model": child["model"], "tools": [t["function"]["name"] for t in child["tools"]], "messages": [{"role": x["role"], "len": len(json.dumps(x))} for x in child["messages"]],
           "system_head": child["messages"][0]["content"][:1200]}, open(os.path.join(CAP, "e7_child_request_summary.json"), "w"), indent=1)
write("e7_child_mode_refused.txt", trace("e7-child-mode", "--full") )
write("e7_child_mode_allowed_task.txt", trace("e7-child-mode2", "--full") + "\n/tmp/child_marker after the run (parent bash was refused):\n" + open(os.path.join(RUNS, "e7-child-mode2/post.out")).read())
write("e7_plan_yolo.txt", trace("e7-plan", "--full"))

# E8
import glob, collections
sd = sorted(glob.glob(os.path.join(RUNS, "e8-resume/home/.omp/agent/sessions/*/*.jsonl")))
out = []
for fp in sd:
    rows = [json.loads(l) for l in open(fp)]
    out.append(f"== {os.path.basename(fp)}: {len(rows)} lines")
    out.append("types: " + json.dumps(dict(collections.Counter(r.get('type') for r in rows))))
    out.append("customType: " + json.dumps(dict(collections.Counter(r.get('customType') for r in rows if r.get('type') == 'custom'))))
    for r in rows[:4]:
        out.append(json.dumps(r)[:420])
    a = [r for r in rows if r.get("type") == "message" and r["message"]["role"] == "assistant"][0]
    out.append("first assistant: " + json.dumps(a)[:900])
out.append("\n" + open(os.path.join(RUNS, "e8-resume/omp.out")).read())
write("e8_session_log.txt", "\n".join(out))
# E9 / E10
write("e9_planted_project.txt", "markers created in /tmp by the planted project (ls -l):\n" + open(os.path.join(RUNS, "e9-planted/post.out")).read() + "\nomp.out:\n" + open(os.path.join(RUNS, "e9-planted/omp.out")).read() + "\nomp.err:\n" + open(os.path.join(RUNS, "e9-planted/omp.err")).read())
cj = []
for l in open(os.path.join(RUNS, "e10-claude-planted/claude.jsonl")):
    j = json.loads(l)
    if j.get("type") == "system" and j.get("subtype") == "init":
        cj.append("init mcp_servers: " + json.dumps([s for s in j.get("mcp_servers", []) if s.get("source") == "project"]) + " permissionMode=" + str(j.get("permissionMode")))
    if j.get("type") == "assistant":
        for c in j["message"]["content"]:
            if c["type"] == "tool_use": cj.append("CALL " + json.dumps({"name": c["name"], "input": c["input"]}))
            if c["type"] == "text": cj.append("TEXT " + c["text"][:200])
    if j.get("type") == "user": cj.append("RESULT " + json.dumps(j["message"]["content"])[:200])
cj.append("workspace listing after the run:\n" + open(os.path.join(RUNS, "e10-claude-planted/ws_listing.txt")).read())
write("e10_claude_planted_project.txt", "\n".join(cj))
# E11
for who, d in (("omp", "e11-omp-tui"), ("fontaine", "e11-fontaine-tui")):
    os.makedirs(os.path.join(CAP, f"e11_{who}"), exist_ok=True)
    for fn in sorted(os.listdir(os.path.join(RUNS, d, "frames"))):
        if re.match(r"(00|00b|01|02|03|05|07|90|91|92)_", fn) or fn.startswith("0") and "approval" in fn:
            t = open(os.path.join(RUNS, d, "frames", fn)).read()
            t = "\n".join(l.rstrip() for l in t.splitlines()) + "\n"
            open(os.path.join(CAP, f"e11_{who}", fn), "w").write(tidy(t))
print("captures written")
