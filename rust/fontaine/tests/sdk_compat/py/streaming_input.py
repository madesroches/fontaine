from claude_agent_sdk import ClaudeSDKClient
from lib import options, result_of, run, turn


async def body():
    opts, _ = options()
    async with ClaudeSDKClient(opts) as client:
        first = result_of(await turn(client, "say one"))
        second = result_of(await turn(client, "say two"))
    return {"results": [first, second]}


run(body)
