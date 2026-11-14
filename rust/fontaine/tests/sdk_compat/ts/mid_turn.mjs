import { open, inputStream, drain, results, text, toolUses, sleep, guarded } from "./lib.mjs";

// A message sent while the first turn is still running a tool.
await guarded(async () => {
  const input = inputStream();
  const { q } = open(input, { permissionMode: "acceptEdits", allowedTools: ["Bash"] });
  input.send(process.env.SCENARIO_FIRST);
  const order = [];
  let sent = false;
  await drain(q, async (m) => {
    if (m.type === "assistant") {
      for (const b of m.message.content) {
        if (b.type === "tool_use") {
          order.push({ at: "tool_use", name: b.name });
          if (!sent) {
            sent = true;
            await sleep(500);
            input.send(process.env.SCENARIO_SECOND);
            order.push({ at: "sent_second" });
          }
        } else if (b.type === "text") order.push({ at: "assistant_text", text: b.text });
      }
    } else if (m.type === "user") order.push({ at: "user", content: m.message.content });
    else if (m.type === "result") {
      order.push({ at: "result", result: m.result });
      // The message sent mid-turn is delivered at the next tool result, so one result answers
      // both; ending the input here also covers a run that queued it as its own turn.
      input.end();
    }
  });
  return { order };
});
