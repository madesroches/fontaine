import { open, inputStream, drain, results, text, guarded } from "./lib.mjs";

// Two sequential messages on one streaming-input query: the second is sent after the first
// result, as a chat application does.
await guarded(async () => {
  const input = inputStream();
  const { q } = open(input);
  input.send(process.env.SCENARIO_FIRST || "say one");
  const seen = [];
  await drain(q, (m) => {
    if (m.type === "result") {
      seen.push({ subtype: m.subtype, result: m.result, session_id: m.session_id });
      if (seen.length === 1) input.send(process.env.SCENARIO_SECOND || "say two");
      else input.end();
    }
  });
  return { results: seen };
});
