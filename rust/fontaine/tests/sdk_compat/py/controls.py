import os

from claude_agent_sdk import ClaudeSDKClient
from lib import options, run, turn


async def body():
    out = {}

    async def attempt(name, fn):
        try:
            out[name] = {"ok": await fn()}
        except Exception as e:
            out[name] = {"error": str(e)}

    opts, _ = options(permission_mode="default")
    async with ClaudeSDKClient(opts) as client:
        await turn(client, "say hi")
        await attempt("set_mode_accept_edits", lambda: client.set_permission_mode("acceptEdits"))
        await attempt("set_mode_plan", lambda: client.set_permission_mode("plan"))
        await attempt("set_mode_bypass", lambda: client.set_permission_mode("bypassPermissions"))
        await attempt("set_model", lambda: client.set_model(os.environ["SCENARIO_NEW_MODEL"]))
        await attempt("set_model_unknown", lambda: client.set_model("no-such-model-anywhere"))
        await attempt("mcp_status", client.get_mcp_status)
        await attempt("context_usage", client.get_context_usage)
    return out


run(body)
