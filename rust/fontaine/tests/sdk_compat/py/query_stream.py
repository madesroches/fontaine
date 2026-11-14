from claude_agent_sdk import query
from lib import PROMPT, options, plain, run


async def body():
    opts, _ = options(include_partial_messages=True)
    messages = [m async for m in query(prompt=PROMPT, options=opts)]
    return {"kinds": [type(m).__name__ for m in messages], "messages": messages}


run(body)
