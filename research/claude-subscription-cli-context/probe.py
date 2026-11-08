#!/usr/bin/env python3
"""Probe: per-step cache writes of the adapter's replay, today vs. a split final frame."""
import json, os, subprocess, sys, tempfile, time

SCRATCH = os.path.dirname(os.path.abspath(__file__))
CWD = os.path.join(SCRATCH, "cli-cwd")
os.makedirs(CWD, exist_ok=True)
MODEL = os.environ.get("PROBE_MODEL", "haiku")
MARK = {"type": "ephemeral", "ttl": "1h"}

ENV_FIXED = {
    "CLAUDE_CODE_ENTRYPOINT": "fontaine", "CLAUDE_CODE_DISABLE_AUTO_MEMORY": "1",
    "CLAUDE_CODE_DISABLE_CLAUDE_MDS": "1", "DISABLE_AUTO_COMPACT": "1", "DISABLE_COMPACT": "1",
    "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC": "1", "DISABLE_TELEMETRY": "1",
    "DISABLE_AUTOUPDATER": "1", "CLAUDE_CODE_MAX_RETRIES": "0", "ENABLE_TOOL_SEARCH": "false",
    "ENABLE_CLAUDEAI_MCP_SERVERS": "false", "CLAUDE_CODE_MESSAGES_PROMPT_CACHE_TTL": "5m",
    "GIT_CEILING_DIRECTORIES": SCRATCH,
}


def doc(k):
    return os.environ.get("SALT","") + "\n".join(f"Document {k}, line {i}: the {k} swift fox jumps over lazy dog number {i * 7 + k}."
                     for i in range(260))


def user(content, query):
    v = {"type": "user", "message": {"role": "user", "content": content}, "parent_tool_use_id": None}
    if not query:
        v["shouldQuery"] = False
    return v


def assistant(text):
    return {"type": "assistant", "message": {"role": "assistant", "content": [{"type": "text", "text": text}]},
            "parent_tool_use_id": None}


def run(frames):
    tmp = tempfile.mkdtemp(dir=SCRATCH)
    sp = os.path.join(tmp, "system.txt")
    open(sp, "w").write("You are a terse assistant. Reply with one word: OK.")
    st = os.path.join(tmp, "settings.json")
    open(st, "w").write(json.dumps({"autoMemoryEnabled": False, "disableAllHooks": True, "env": {}}))
    args = ["claude", "-p", "--model", MODEL, "--input-format", "stream-json", "--output-format",
            "stream-json", "--verbose", "--tools", "", "--system-prompt-file", sp, "--settings", st,
            "--setting-sources", "", "--strict-mcp-config", "--disable-slash-commands", "--max-turns", "1",
            "--permission-mode", "dontAsk", "--no-session-persistence"]
    env = dict(os.environ)
    env.update(ENV_FIXED)
    p = subprocess.Popen(args, cwd=CWD, env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                         stderr=subprocess.PIPE, text=True)
    usage = None
    acks = 0
    for f in frames:
        p.stdin.write(json.dumps(f) + "\n")
        p.stdin.flush()
        if f["type"] == "assistant":
            continue
        while True:
            line = p.stdout.readline()
            if not line:
                err = p.stderr.read()
                return {"error": "eof", "stderr": err[-2000:], "acks": acks}
            ev = json.loads(line)
            if ev.get("type") == "result":
                if ev.get("num_turns", 0) == 0 and f.get("shouldQuery") is False:
                    acks += 1
                    break
                usage = ev.get("usage")
                text = ev.get("result")
                p.stdin.close()
                p.wait(timeout=60)
                return {"usage": {k: usage.get(k) for k in ("input_tokens", "cache_read_input_tokens",
                                                              "cache_creation_input_tokens", "cache_creation")},
                        "result": text, "num_turns": ev.get("num_turns"), "subtype": ev.get("subtype"),
                        "acks": acks}
    return {"error": "no query frame"}


def conversation(steps, variant, trigger=None):
    """Request k replays k-1 exchanges and asks about document k."""
    out = []
    for k in range(1, steps + 1):
        frames = []
        for j in range(1, k + 1):
            last = j == k
            content = [{"type": "text", "text": doc(j)}]
            if variant == "final":
                if last:
                    content[-1]["cache_control"] = MARK
                frames.append(user(content, last))
            elif variant == "today":
                if not last and j == k - 1:
                    content[-1]["cache_control"] = MARK
                frames.append(user(content, last))
            else:  # split: content as history with the marker, then a trigger
                if last:
                    content[-1]["cache_control"] = MARK
                    frames.append(user(content, False))
                    frames.append(user(trigger, True))
                else:
                    frames.append(user(content, False))
            if not last:
                frames.append(assistant("OK."))
        r = run(frames)
        out.append(r)
        print(variant, json.dumps(trigger), k, json.dumps(r), flush=True)
    return out


if __name__ == "__main__":
    which = sys.argv[1]
    steps = int(sys.argv[2]) if len(sys.argv) > 2 else 3
    if which in ("today", "final"):
        conversation(steps, which)
    else:
        trig = json.loads(sys.argv[3])
        conversation(steps, "split", trig)
