import asyncio
import os
import time

from claude_agent_sdk import ClaudeSDKClient
from lib import options, run


async def body():
    opts, _ = options(permission_mode="acceptEdits", allowed_tools=["Bash"])
    started = time.time()
    client = ClaudeSDKClient(opts)
    await client.connect()
    await client.query(os.environ["SCENARIO_PROMPT"])
    async for m in client.receive_response():
        if type(m).__name__ == "AssistantMessage":
            await asyncio.sleep(0.5)
            break
    await client.disconnect()
    return {"ms": int((time.time() - started) * 1000)}


run(body)
