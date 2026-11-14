import { open, inputStream, drain, results, guarded } from "./lib.mjs";

// A user message whose content is neither a string nor blocks.
await guarded(async () => {
  const input = inputStream();
  const { q, stderr } = open(input);
  input.send({ not: "a string" });
  input.end();
  const messages = await drain(q);
  return { results: results(messages), stderr };
});
