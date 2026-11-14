import { open, drain, results, guarded } from "./lib.mjs";

// continue, resume and fork over the same working directory.
await guarded(async () => {
  const run = async (prompt, extra = {}) => {
    const { q } = open(prompt, extra);
    const messages = await drain(q);
    const r = results(messages)[0];
    return { session_id: r.session_id, result: r.result, subtype: r.subtype };
  };
  const first = await run("remember the word pelican and say ok");
  const continued = await run("what word did I ask you to remember? one word", { continue: true });
  const resumed = await run("say the word again, one word", { resume: first.session_id });
  const forked = await run("say the word once more, one word", {
    resume: first.session_id,
    forkSession: true,
  });
  return { first, continued, resumed, forked };
});
