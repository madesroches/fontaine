import { open, drain, sleep, guarded } from "./lib.mjs";

// Closing a query mid-turn must end the child process promptly.
await guarded(async () => {
  const { q } = open(process.env.SCENARIO_PROMPT, { permissionMode: "acceptEdits", allowedTools: ["Bash"] });
  let closed = false;
  const started = Date.now();
  try {
    await drain(q, async (m) => {
      if (m.type === "assistant" && !closed && m.message.content.some((b) => b.type === "tool_use")) {
        closed = true;
        await sleep(500);
        q.close();
      }
    });
  } catch (_) {}
  return { closed, ms: Date.now() - started };
});
