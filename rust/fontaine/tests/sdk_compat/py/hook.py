import os

from claude_agent_sdk import ClaudeSDKClient, HookMatcher
from lib import options, result_of, run, turn


async def body():
    inputs = []

    async def hook(input_data, tool_use_id, context):
        inputs.append({"input": input_data, "tool_use_id": tool_use_id})
        return {
            "hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "permissionDecision": "deny",
                "permissionDecisionReason": "blocked by the host hook",
            }
        }

    opts, _ = options(hooks={"PreToolUse": [HookMatcher(matcher=os.environ.get("SCENARIO_MATCHER", "Bash"), hooks=[hook])]})
    async with ClaudeSDKClient(opts) as client:
        messages = await turn(client, os.environ["SCENARIO_PROMPT"])
    return {"hook_inputs": inputs, "result": result_of(messages)}


run(body)
