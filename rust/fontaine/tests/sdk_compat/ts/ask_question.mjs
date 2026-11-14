import { open, drain, results, guarded } from "./lib.mjs";

await guarded(async () => {
  const asked = [];
  const canUseTool = async (toolName, input) => {
    if (toolName === "AskUserQuestion") {
      asked.push(input);
      const answers = {};
      for (const q of input.questions) answers[q.question] = q.options[0].label;
      return { behavior: "allow", updatedInput: { ...input, answers } };
    }
    return { behavior: "allow", updatedInput: input };
  };
  const { q } = open(process.env.SCENARIO_PROMPT, { canUseTool });
  const messages = await drain(q);
  return { asked, result: results(messages)[0] ?? null };
});
