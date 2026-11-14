from lib import options, run
from claude_agent_sdk import query


async def body():
    async def stream():
        yield {"type": "user", "message": {"role": "user", "content": {"not": "a string"}}, "parent_tool_use_id": None}

    opts, stderr = options()
    kinds = []
    try:
        async for m in query(prompt=stream(), options=opts):
            kinds.append(type(m).__name__)
    except Exception as e:
        return {"raised": f"{type(e).__name__}: {e}", "kinds": kinds, "stderr": stderr}
    return {"raised": None, "kinds": kinds, "stderr": stderr}


run(body)
