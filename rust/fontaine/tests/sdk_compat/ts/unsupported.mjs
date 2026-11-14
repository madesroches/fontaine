import { open, drain, results, guarded } from "./lib.mjs";

// An option the executable does not implement: the raised error must name the flag.
await guarded(async () => {
  const { q, stderr } = open("say hello", JSON.parse(process.env.SCENARIO_OPTIONS || '{"fallbackModel":"x"}'));
  try {
    await drain(q);
  } catch (e) {
    return { raised: String(e?.message ?? e), stderr };
  }
  return { raised: null, stderr };
});
