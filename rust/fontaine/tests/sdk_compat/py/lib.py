"""Shared plumbing for the consumer programs: environment in, one JSON report out. The programs
choose the executable only through `cli_path`."""
import asyncio
import dataclasses
import json
import os
import sys

from claude_agent_sdk import ClaudeAgentOptions, ClaudeSDKClient

BIN = os.environ.get("FONTAINE_BIN")
if not BIN:
    print("FONTAINE_BIN is not set", file=sys.stderr)
    sys.exit(64)
MODEL = os.environ.get("SCENARIO_MODEL") or None
PROMPT = os.environ.get("SCENARIO_PROMPT", "say hello")


def options(**extra):
    stderr = []
    opts = ClaudeAgentOptions(
        cli_path=BIN,
        cwd=os.getcwd(),
        stderr=stderr.append,
        **({"model": MODEL} if MODEL else {}),
        **extra,
    )
    return opts, stderr


def plain(value):
    if dataclasses.is_dataclass(value) and not isinstance(value, type):
        return {"_type": type(value).__name__, **{k: plain(v) for k, v in dataclasses.asdict(value).items()}}
    if isinstance(value, dict):
        return {k: plain(v) for k, v in value.items()}
    if isinstance(value, (list, tuple)):
        return [plain(v) for v in value]
    return value


async def turn(client, prompt):
    """One message and the messages up to and including its result."""
    await client.query(prompt)
    return [m async for m in client.receive_response()]


def result_of(messages):
    return next((m for m in messages if type(m).__name__ == "ResultMessage"), None)


def run(body):
    async def main():
        try:
            report = await body()
        except Exception as e:  # a refusal is data
            report = {"error": f"{type(e).__name__}: {e}"}
        sys.stdout.write(json.dumps(plain(report), default=str) + "\n")
        sys.stdout.flush()

    asyncio.run(main())
    os._exit(0)
