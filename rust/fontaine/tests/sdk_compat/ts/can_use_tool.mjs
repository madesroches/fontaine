import { open, drain, results, toolUses, guarded } from "./lib.mjs";

// SCENARIO_MODE: allow | deny | deny_interrupt | modify | update | disconnect | error
await guarded(async () => {
  const mode = process.env.SCENARIO_MODE || "allow";
  const calls = [];
  const canUseTool = async (toolName, input, ctx) => {
    calls.push({
      tool_name: toolName,
      input,
      tool_use_id: ctx.toolUseID,
      agent_id: ctx.agentID ?? null,
      suggestions: ctx.suggestions ?? null,
      decisionReason: ctx.decisionReason ?? null,
      description: ctx.description ?? null,
    });
    switch (mode) {
      case "deny":
        return { behavior: "deny", message: "not today" };
      case "deny_interrupt":
        return { behavior: "deny", message: "stop", interrupt: true };
      case "modify":
        return { behavior: "allow", updatedInput: JSON.parse(process.env.SCENARIO_UPDATED_INPUT) };
      case "update":
        return {
          behavior: "allow",
          updatedInput: input,
          updatedPermissions: JSON.parse(process.env.SCENARIO_UPDATED_PERMISSIONS),
        };
      case "error":
        throw new Error("callback failed");
      case "disconnect":
        // Never answers; the program is killed by its caller.
        await new Promise(() => {});
        return undefined;
      default:
        return { behavior: "allow", updatedInput: input };
    }
  };
  const { q } = open(process.env.SCENARIO_PROMPT, {
    canUseTool,
    permissionMode: process.env.SCENARIO_PERMISSION_MODE || "default",
  });
  if (mode === "disconnect") {
    setTimeout(() => process.exit(0), Number(process.env.SCENARIO_DISCONNECT_MS || 4000));
  }
  const messages = await drain(q);
  return {
    calls,
    tool_uses: toolUses(messages).map((b) => ({ name: b.name, input: b.input })),
    denials: results(messages)[0]?.permission_denials ?? null,
    result: results(messages)[0] ?? null,
  };
});
