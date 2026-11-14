import os

from claude_agent_sdk import ClaudeSDKClient, PermissionResultAllow
from lib import options, result_of, run, turn


async def body():
    asked = []

    async def can_use_tool(tool_name, tool_input, context):
        if tool_name == "AskUserQuestion":
            asked.append(tool_input)
            answers = {q["question"]: q["options"][0]["label"] for q in tool_input["questions"]}
            return PermissionResultAllow(updated_input={**tool_input, "answers": answers})
        return PermissionResultAllow(updated_input=tool_input)

    opts, _ = options(can_use_tool=can_use_tool)
    async with ClaudeSDKClient(opts) as client:
        messages = await turn(client, os.environ["SCENARIO_PROMPT"])
    return {"asked": asked, "result": result_of(messages)}


run(body)
