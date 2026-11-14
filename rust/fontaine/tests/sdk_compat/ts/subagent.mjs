import { open, drain, results, toolUses, guarded } from "./lib.mjs";

await guarded(async () => {
  const asked = [];
  const { q } = open(process.env.SCENARIO_PROMPT, {
    agents: {
      helper: {
        description: "Runs a shell command for the main agent",
        prompt: "Run the command you are given with Bash and report its output.",
        tools: ["Bash"],
      },
    },
    canUseTool: async (toolName, input, ctx) => {
      asked.push({ tool_name: toolName, agent_id: ctx.agentID ?? null });
      return { behavior: "allow", updatedInput: input };
    },
  });
  const messages = await drain(q);
  const uses = toolUses(messages);
  const agentUse = uses.find((b) => b.name === "Agent" || b.name === "Task");
  const toolResults = messages
    .filter((m) => m.type === "user")
    .flatMap((m) => (Array.isArray(m.message.content) ? m.message.content : []))
    .filter((b) => b.type === "tool_result");
  return {
    asked,
    agent_use: agentUse ?? null,
    agent_result: toolResults.find((b) => b.tool_use_id === agentUse?.id) ?? null,
    parent_ids: messages.map((m) => m.parent_tool_use_id ?? null),
    result: results(messages)[0] ?? null,
  };
});
