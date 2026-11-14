import { open, drain, summarize, results, text, guarded } from "./lib.mjs";

await guarded(async () => {
  const { q } = open(process.env.SCENARIO_PROMPT || "say hello", {
    includePartialMessages: process.env.SCENARIO_PARTIAL !== "no",
  });
  const messages = await drain(q);
  const result = results(messages)[0];
  return {
    kinds: summarize(messages),
    init: messages.find((m) => m.type === "system" && m.subtype === "init") ?? null,
    assistant: messages.filter((m) => m.type === "assistant").map(text),
    stream_events: messages.filter((m) => m.type === "stream_event").map((m) => m.event.type),
    result: result ?? null,
  };
});
