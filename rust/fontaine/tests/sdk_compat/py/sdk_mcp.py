import os

from claude_agent_sdk import ClaudeSDKClient, PermissionResultAllow, create_sdk_mcp_server, tool
from lib import options, result_of, run, turn


async def body():
    calls = []

    @tool("add", "Adds two numbers", {"a": float, "b": float})
    async def add(args):
        calls.append(args)
        return {"content": [{"type": "text", "text": str(args["a"] + args["b"])}]}

    server = create_sdk_mcp_server(name="host", version="1.0.0", tools=[add])

    async def can_use_tool(tool_name, tool_input, context):
        return PermissionResultAllow(updated_input=tool_input)

    opts, _ = options(mcp_servers={"host": server}, can_use_tool=can_use_tool)
    async with ClaudeSDKClient(opts) as client:
        messages = await turn(client, os.environ["SCENARIO_PROMPT"])
    return {"calls": calls, "result": result_of(messages)}


run(body)
