from claude_agent_sdk import ClaudeSDKClient
from lib import options, result_of, run, turn


async def body():
    async def one(prompt, **extra):
        opts, _ = options(**extra)
        async with ClaudeSDKClient(opts) as client:
            r = result_of(await turn(client, prompt))
        return {"session_id": r.session_id, "result": r.result, "subtype": r.subtype}

    first = await one("remember the word pelican and say ok")
    continued = await one("what word did I ask you to remember? one word", continue_conversation=True)
    resumed = await one("say the word again, one word", resume=first["session_id"])
    forked = await one("say the word once more, one word", resume=first["session_id"], fork_session=True)
    return {"first": first, "continued": continued, "resumed": resumed, "forked": forked}


run(body)
