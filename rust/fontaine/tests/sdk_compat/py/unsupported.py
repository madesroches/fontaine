import json
import os

from claude_agent_sdk import ClaudeSDKClient
from lib import options, run, turn


async def body():
    extra = json.loads(os.environ.get("SCENARIO_OPTIONS", '{"fallback_model":"x"}'))
    opts, stderr = options(**extra)
    try:
        async with ClaudeSDKClient(opts) as client:
            await turn(client, "say hello")
    except Exception as e:
        return {"raised": f"{type(e).__name__}: {e}", "exit_code": getattr(e, "exit_code", None), "stderr": stderr}
    return {"raised": None, "stderr": stderr}


run(body)
