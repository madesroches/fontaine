import { open, inputStream, drain, results, sleep, guarded } from "./lib.mjs";

// Interrupts a turn that is running a long tool, then sends a further message.
await guarded(async () => {
  const input = inputStream();
  const { q } = open(input, { permissionMode: "acceptEdits", allowedTools: ["Bash"] });
  input.send(process.env.SCENARIO_PROMPT);
  const seen = [];
  let interrupted = false;
  let interruptReturned = false;
  await drain(q, async (m) => {
    if (m.type === "assistant" && !interrupted && m.message.content.some((b) => b.type === "tool_use")) {
      interrupted = true;
      await sleep(1000);
      await q.interrupt();
      interruptReturned = true;
      input.send(process.env.SCENARIO_AFTER || "say done");
    } else if (m.type === "result") {
      seen.push(m);
      if (seen.length === 2) input.end();
    }
  });
  return { interrupt_returned: interruptReturned, results: seen };
});
