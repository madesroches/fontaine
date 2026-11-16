#!/usr/bin/env python3
"""Which of the Databricks AI Gateway's four routes can turn DeepSeek's thinking off?

The MLflow Chat route accepts every thinking-off field and ignores all of them
(`probe_dbx_thinking.py`). The gateway serves the same model on the Anthropic Messages route, the
OpenAI Responses route and the OpenAI Chat route, and each is a different proxy with its own
parameter mapping, so the field may be honoured on one of them. This asks the same small reasoning
prompt on each, with and without the disable its wire spells, and reports the tokens the deployment
says it reasoned with. It also lists the workspace's `system.ai` model services, in case a
non-thinking variant of the model is served under another id.

The token is read from `~/.claude_databricks_token` and goes only into request headers; nothing
here prints it. The base URL is `--base-url`, else `$DBX_BASE_URL`, else the first `databricks`
account of the user config. The model is the account's configured main model unless one is named.

Usage: python3 probe_dbx_thinking_routes.py [--base-url URL] [model]
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

TOOL_SCHEMA = {
    "type": "object",
    "properties": {
        "decision": {"type": "string", "enum": ["allow", "deny"]},
        "reason": {"type": "string"},
    },
    "required": ["decision", "reason"],
}

GUARDIAN = (
    "You are a security guardian for a coding agent's permission pipeline. The user message is "
    "the tool call the agent wants to make; decide whether it is safe to run. Answer only by "
    "calling the submit tool with your verdict and a one-sentence reason."
)

CALL = "tool: shell\ncommand: git push --force origin main\nDecide whether to allow this call."


def config_text():
    with open(os.path.expanduser("~/.config/fontaine/config.toml"), encoding="utf-8") as file:
        return file.read()


def field_of(text, name):
    found = re.search(rf'^{name} = "([^"]+)"', text, flags=re.M)
    return found.group(1) if found else None


def databricks_block(text):
    for block in re.split(r"^\[", text, flags=re.M):
        if 'kind = "databricks"' in block:
            return block
    raise SystemExit("no databricks account in the user config: pass --base-url")


def main_model(text):
    for block in re.split(r"^\[", text, flags=re.M):
        if block.startswith("model]"):
            model = field_of(block, "model")
            if model:
                return model.split("/", 1)[-1]
    return "system.ai.deepseek-v4-1-flash-1017"


def token():
    with open(os.path.expanduser("~/.claude_databricks_token"), encoding="utf-8") as file:
        return file.read().strip()


def call(url, key, body, headers=None, timeout=180):
    sent = {"content-type": "application/json", "authorization": f"Bearer {key}"}
    sent.update(headers or {})
    request = urllib.request.Request(url, json.dumps(body).encode(), sent)
    started = time.monotonic()
    try:
        with urllib.request.urlopen(request, timeout=timeout) as answer:
            status, payload = answer.status, json.loads(answer.read())
    except urllib.error.HTTPError as error:
        return {
            "status": error.code,
            "body": error.read().decode("utf-8", "replace")[:300],
            "elapsed_ms": round((time.monotonic() - started) * 1000),
        }
    except Exception as error:
        return {
            "error": f"{type(error).__name__}: {error}",
            "elapsed_ms": round((time.monotonic() - started) * 1000),
        }
    return {"status": status, "elapsed_ms": round((time.monotonic() - started) * 1000), "payload": payload}


def chat_report(answer):
    payload = answer.pop("payload", None)
    if payload is None:
        return answer
    choice = (payload.get("choices") or [{}])[0]
    message = choice.get("message") or {}
    usage = payload.get("usage") or {}
    details = usage.get("completion_tokens_details") or {}
    text = message.get("content")
    answer.update(
        finish=choice.get("finish_reason"),
        tool_calls=len(message.get("tool_calls") or []),
        reasoning_tokens=details.get("reasoning_tokens"),
        completion_tokens=usage.get("completion_tokens"),
        content=text[:60] if isinstance(text, str) else None,
    )
    return answer


def messages_report(answer):
    payload = answer.pop("payload", None)
    if payload is None:
        return answer
    kinds = [block.get("type") for block in payload.get("content") or []]
    usage = payload.get("usage") or {}
    answer.update(
        stop=payload.get("stop_reason"),
        blocks=kinds,
        tool_use=kinds.count("tool_use"),
        thinking=bool(payload.get("content") and any(b.get("type") == "thinking" for b in payload["content"])),
        output_tokens=usage.get("output_tokens"),
    )
    return answer


def responses_report(answer):
    payload = answer.pop("payload", None)
    if payload is None:
        return answer
    usage = payload.get("usage") or {}
    details = usage.get("output_tokens_details") or {}
    kinds = [item.get("type") for item in payload.get("output") or []]
    answer.update(
        status_field=payload.get("status"),
        incomplete=(payload.get("incomplete_details") or {}).get("reason"),
        items=kinds,
        reasoning_tokens=details.get("reasoning_tokens"),
        output_tokens=usage.get("output_tokens"),
    )
    return answer


def main():
    argv = sys.argv[1:]
    url_override = None
    if "--base-url" in argv:
        at = argv.index("--base-url")
        url_override = argv[at + 1].rstrip("/")
        del argv[at : at + 2]
    text = config_text()
    block = databricks_block(text)
    root = url_override or os.environ.get("DBX_BASE_URL", "").rstrip("/") or field_of(block, "base_url").rstrip("/")
    model = argv[0] if argv else main_model(text)
    key = token()

    def show(label, answer):
        print(json.dumps({label: answer}), flush=True)
        time.sleep(0.3)

    # The workspace's own list: is a non-thinking variant of the model served under another id?
    try:
        request = urllib.request.Request(
            f"{root}/api/2.1/unity-catalog/model-services?parent=schemas/system.ai&page_size=100",
            headers={"authorization": f"Bearer {key}"},
        )
        with urllib.request.urlopen(request, timeout=60) as answer:
            services = json.loads(answer.read())
    except Exception as error:
        show("model services", {"error": f"{type(error).__name__}: {error}"})
        services = {}
    names = [
        service.get("name", "").removeprefix("model-services/")
        for service in services.get("model_services") or []
        if "deepseek" in service.get("name", "").lower()
    ]
    show("model services | deepseek", {"names": names})

    chat = f"{root}/ai-gateway/mlflow/v1/chat/completions"
    openai_chat = f"{root}/ai-gateway/openai/v1/chat/completions"
    messages = f"{root}/ai-gateway/anthropic/v1/messages"
    responses = f"{root}/ai-gateway/openai/v1/responses"

    def plain_chat(extra):
        body = {"model": model, "max_tokens": 600, "messages": [{"role": "user", "content": QUESTION}]}
        body.update(extra)
        return body

    # 1. MLflow Chat: repeat the one field that changed the usage report, three times.
    for repeat in range(1, 4):
        show(f"mlflow chat | no field, call {repeat}", chat_report(call(chat, key, plain_chat({}))))
        show(
            f'mlflow chat | reasoning_effort "none", call {repeat}',
            chat_report(call(chat, key, plain_chat({"reasoning_effort": "none"}))),
        )

    # 2. OpenAI Chat route: the same proxy family, but a different mapping.
    show("openai chat | no field", chat_report(call(openai_chat, key, plain_chat({}))))
    show(
        'openai chat | reasoning_effort "none"',
        chat_report(call(openai_chat, key, plain_chat({"reasoning_effort": "none"}))),
    )
    show(
        "openai chat | thinking disabled",
        chat_report(call(openai_chat, key, plain_chat({"thinking": {"type": "disabled"}}))),
    )
    show(
        'openai chat | max_completion_tokens 600, reasoning_effort "none"',
        chat_report(call(openai_chat, key, plain_chat({"max_completion_tokens": 600, "reasoning_effort": "none"}))),
    )

    # 3. Anthropic Messages route: thinking is a first-class block there.
    def plain_messages(extra):
        body = {
            "model": model,
            "max_tokens": 600,
            "messages": [{"role": "user", "content": QUESTION}],
        }
        body.update(extra)
        return body

    def forced_messages(extra):
        body = {
            "model": model,
            "max_tokens": 1024,
            "system": GUARDIAN,
            "messages": [{"role": "user", "content": CALL}],
            "tools": [{"name": "submit", "description": "Submit the verdict.", "input_schema": TOOL_SCHEMA}],
            "tool_choice": {"type": "tool", "name": "submit"},
        }
        body.update(extra)
        return body

    version = {"anthropic-version": "2023-06-01", "x-api-key": key}
    show("messages | no field", messages_report(call(messages, key, plain_messages({}), version)))
    show(
        "messages | thinking disabled",
        messages_report(call(messages, key, plain_messages({"thinking": {"type": "disabled"}}), version)),
    )
    show(
        "messages | forced tool, no field",
        messages_report(call(messages, key, forced_messages({}), version)),
    )
    show(
        "messages | forced tool, thinking disabled",
        messages_report(call(messages, key, forced_messages({"thinking": {"type": "disabled"}}), version)),
    )

    # 4. OpenAI Responses route.
    def plain_responses(extra):
        body = {"model": model, "max_output_tokens": 600, "input": QUESTION}
        body.update(extra)
        return body

    show("responses | no field", responses_report(call(responses, key, plain_responses({}))))
    show(
        'responses | reasoning effort "none"',
        responses_report(call(responses, key, plain_responses({"reasoning": {"effort": "none"}}))),
    )


if __name__ == "__main__":
    main()
