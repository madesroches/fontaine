import os

from claude_agent_sdk import AgentDefinition, ClaudeSDKClient, PermissionResultAllow
from lib import options, result_of, run, turn


async def body():
    asked = []

    async def can_use_tool(tool_name, tool_input, context):
        asked.append({"tool_name": tool_name, "agent_id": getattr(context, "agent_id", None)})
        return PermissionResultAllow(updated_input=tool_input)

    opts, _ = options(
        can_use_tool=can_use_tool,
        agents={
            "helper": AgentDefinition(
                description="Runs a shell command for the main agent",
                prompt="Run the command you are given with Bash and report its output.",
                tools=["Bash"],
            )
        },
    )
    async with ClaudeSDKClient(opts) as client:
        messages = await turn(client, os.environ["SCENARIO_PROMPT"])
    return {"asked": asked, "kinds": [type(m).__name__ for m in messages], "result": result_of(messages)}


run(body)
