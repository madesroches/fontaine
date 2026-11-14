import asyncio
import os

from claude_agent_sdk import ClaudeSDKClient
from lib import options, result_of, run, turn


async def body():
    opts, _ = options(permission_mode="acceptEdits", allowed_tools=["Bash"])
    async with ClaudeSDKClient(opts) as client:
        await client.query(os.environ["SCENARIO_PROMPT"])
        interrupted = False
        first = []
        async for m in client.receive_response():
            first.append(m)
            if not interrupted and type(m).__name__ == "AssistantMessage":
                interrupted = True
                await asyncio.sleep(1)
                await client.interrupt()
        after = await turn(client, os.environ.get("SCENARIO_AFTER", "say done"))
    return {"first": result_of(first), "after": result_of(after)}


run(body)
