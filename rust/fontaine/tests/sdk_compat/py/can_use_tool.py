import json
import os

from claude_agent_sdk import ClaudeSDKClient, PermissionResultAllow, PermissionResultDeny
from claude_agent_sdk.types import PermissionRuleValue, PermissionUpdate
from lib import options, result_of, run, turn

# SCENARIO_MODE: allow | deny | deny_interrupt | modify | update | error


async def body():
    mode = os.environ.get("SCENARIO_MODE", "allow")
    calls = []

    async def can_use_tool(tool_name, tool_input, context):
        calls.append(
            {
                "tool_name": tool_name,
                "input": tool_input,
                "agent_id": context.agent_id,
                "suggestions": [str(s) for s in context.suggestions],
            }
        )
        if mode == "deny":
            return PermissionResultDeny(message="not today")
        if mode == "deny_interrupt":
            return PermissionResultDeny(message="stop", interrupt=True)
        if mode == "modify":
            return PermissionResultAllow(updated_input=json.loads(os.environ["SCENARIO_UPDATED_INPUT"]))
        if mode == "update":
            return PermissionResultAllow(
                updated_input=tool_input,
                updated_permissions=[
                    PermissionUpdate(
                        type=u["type"],
                        rules=[PermissionRuleValue(tool_name=r["toolName"], rule_content=r.get("ruleContent")) for r in u.get("rules", [])],
                        behavior=u.get("behavior"),
                        destination=u.get("destination"),
                    )
                    for u in json.loads(os.environ["SCENARIO_UPDATED_PERMISSIONS"])
                ],
            )
        if mode == "error":
            raise RuntimeError("callback failed")
        return PermissionResultAllow(updated_input=tool_input)

    opts, _ = options(can_use_tool=can_use_tool, permission_mode=os.environ.get("SCENARIO_PERMISSION_MODE", "default"))
    async with ClaudeSDKClient(opts) as client:
        messages = await turn(client, os.environ["SCENARIO_PROMPT"])
    r = result_of(messages)
    return {"calls": calls, "result": r}


run(body)
