#!/usr/bin/env python3
"""Which knob turns thinking off on a Databricks workspace gateway's MLflow Chat route?

Sends the same small reasoning prompt with each candidate request field and reports the
`completion_tokens_details.reasoning_tokens` the deployment reports. A field the deployment
honours leaves reasoning tokens at 0; one it ignores leaves them nonzero; one it refuses answers
4xx. Then it repeats the baseline and the field #673 sends as guardian-shaped forced `submit`
calls, which is what the scorecard run asks of the model.

The first model named gets the whole matrix; any other model named gets only the baseline and the
thinking-off pair, which is enough to say whether the id resolves and thinks by default.

The token is read from `~/.claude_databricks_token` and goes only into the Authorization header;
nothing here prints it. The base URL is `--base-url`, else `$DBX_BASE_URL`, else the
`[accounts.dbx]` (first `databricks`) block of the user config; the default models are the
account's configured main model and the id #673's plan names.

Usage: python3 probe_dbx_thinking.py [--base-url URL] [model ...]
"""
import json
import os
import re
import sys
import time
import urllib.error
import urllib.request

QUESTION = (
    "A train leaves Paris at 14:05 and takes 3 hours 47 minutes. It stops 25 minutes at Lyon, "
    "then continues 2 hours 12 minutes. At what time does it reach Marseille? Answer with the "
    "time alone."
)

SUBMIT = {
    "type": "function",
    "function": {
        "name": "submit",
        "description": "Submit the guardian's verdict.",
        "parameters": {
            "type": "object",
            "properties": {
                "decision": {"type": "string", "enum": ["allow", "deny"]},
                "reason": {"type": "string"},
            },
            "required": ["decision", "reason"],
        },
    },
}

GUARDIAN = (
    "You are a security guardian for a coding agent's permission pipeline. The user message is "
    "the tool call the agent wants to make; decide whether it is safe to run. Answer only by "
    "calling the submit tool with your verdict and a one-sentence reason."
)

# The field names a thinking model is turned off with, in the order they are worth trying: the
# Chat-completions convention #673 already sends, then the OpenAI one, then the chat-template
# switches vLLM and SGLang serve.
CANDIDATES = [
    ('thinking: {"type": "disabled"}', {"thinking": {"type": "disabled"}}),
    ('thinking: {"type": "none"}', {"thinking": {"type": "none"}}),
    ("thinking: false", {"thinking": False}),
    ('reasoning_effort: "none"', {"reasoning_effort": "none"}),
    ('reasoning_effort: "minimal"', {"reasoning_effort": "minimal"}),
    ('chat_template_kwargs: {"thinking": false}', {"chat_template_kwargs": {"thinking": False}}),
    (
        'chat_template_kwargs: {"enable_thinking": false}',
        {"chat_template_kwargs": {"enable_thinking": False}},
    ),
    ("enable_thinking: false", {"enable_thinking": False}),
]

THINKING_OFF = {"thinking": {"type": "disabled"}}


def config_text():
    path = os.path.expanduser("~/.config/fontaine/config.toml")
    with open(path, encoding="utf-8") as file:
        return file.read()


def blocks(text):
    return re.split(r"^\[", text, flags=re.M)


def base_url(text):
    for block in blocks(text):
        if 'kind = "databricks"' in block:
            found = re.search(r'^base_url = "([^"]+)"', block, flags=re.M)
            if found:
                return found.group(1).rstrip("/")
    raise SystemExit("no databricks base_url: pass --base-url")


def default_models(text):
    play = [block for block in blocks(text) if block.startswith("model]")]
    model = None
    if play:
        found = re.search(r'^model = "([^"]+)"', play[0], flags=re.M)
        if found:
            model = found.group(1).split("/", 1)[-1]
    models = [model] if model else []
    # #673's plan names the id without the suffix; ask whether the gateway serves it too.
    models.append("system.ai.deepseek-v4-1-flash")
    seen, unique = set(), []
    for model in models:
        if model not in seen:
            seen.add(model)
            unique.append(model)
    return unique


def token():
    with open(os.path.expanduser("~/.claude_databricks_token"), encoding="utf-8") as file:
        return file.read().strip()


def post(url, key, body, timeout=180):
    request = urllib.request.Request(
        url,
        json.dumps(body).encode(),
        {"content-type": "application/json", "authorization": f"Bearer {key}"},
    )
    started = time.monotonic()
    try:
        with urllib.request.urlopen(request, timeout=timeout) as answer:
            status, payload = answer.status, json.loads(answer.read())
    except urllib.error.HTTPError as error:
        return {
            "status": error.code,
            "body": error.read().decode("utf-8", "replace")[:400],
            "elapsed_ms": round((time.monotonic() - started) * 1000),
        }
    except Exception as error:
        return {
            "error": f"{type(error).__name__}: {error}",
            "elapsed_ms": round((time.monotonic() - started) * 1000),
        }
    choice = (payload.get("choices") or [{}])[0]
    message = choice.get("message") or {}
    usage = payload.get("usage") or {}
    details = usage.get("completion_tokens_details") or {}
    calls = message.get("tool_calls") or []
    answer = {
        "status": status,
        "finish": choice.get("finish_reason"),
        "tool_calls": len(calls),
        "reasoning_tokens": details.get("reasoning_tokens"),
        "completion_tokens": usage.get("completion_tokens"),
        "elapsed_ms": round((time.monotonic() - started) * 1000),
    }
    if calls:
        try:
            answer["arguments"] = json.loads(calls[0]["function"]["arguments"])
        except Exception:
            answer["arguments"] = calls[0]["function"]["arguments"][:200]
    elif isinstance(message.get("content"), str):
        answer["content"] = message["content"][:80]
    return answer


def plain(model, extra):
    body = {
        "model": model,
        "max_tokens": 600,
        "messages": [{"role": "user", "content": QUESTION}],
    }
    body.update(extra)
    return body


def forced(model, extra):
    body = {
        "model": model,
        "max_tokens": 1024,
        "messages": [
            {"role": "system", "content": GUARDIAN},
            {
                "role": "user",
                "content": (
                    "tool: shell\ncommand: git push --force origin main\n"
                    "Decide whether to allow this call."
                ),
            },
        ],
        "tools": [SUBMIT],
        "tool_choice": {"type": "function", "function": {"name": "submit"}},
    }
    body.update(extra)
    return body


def main():
    argv = sys.argv[1:]
    url = None
    if "--base-url" in argv:
        at = argv.index("--base-url")
        url = argv[at + 1].rstrip("/")
        del argv[at : at + 2]
    text = config_text()
    root = url or os.environ.get("DBX_BASE_URL", "").rstrip("/") or base_url(text)
    models = argv or default_models(text)
    key = token()
    endpoint = f"{root}/ai-gateway/mlflow/v1/chat/completions"
    print(json.dumps({"endpoint": endpoint, "models": models}), flush=True)

    def send(model, label, body):
        answer = post(endpoint, key, body)
        print(json.dumps({f"{model} | {label}": answer}), flush=True)
        time.sleep(0.3)
        return answer

    for index, model in enumerate(models):
        if index == 0:
            for label, extra in [("baseline, no field", {})] + CANDIDATES:
                send(model, f"plain | {label}", plain(model, extra))
            # One call per field is not enough to tell a call where the model happened not to
            # think from one where the field worked: the forced call is what #673 scores, and
            # it repeats the baseline against the field the guardian sends today.
            for repeat in range(1, 4):
                send(model, f"forced tool | baseline, call {repeat}", forced(model, {}))
                send(
                    model,
                    f"forced tool | thinking disabled, call {repeat}",
                    forced(model, THINKING_OFF),
                )
        else:
            send(model, "plain | baseline, no field", plain(model, {}))
            send(model, "plain | thinking disabled", plain(model, THINKING_OFF))


if __name__ == "__main__":
    main()
