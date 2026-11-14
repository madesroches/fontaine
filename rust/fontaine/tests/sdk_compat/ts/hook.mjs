import { open, drain, results, toolUses, guarded } from "./lib.mjs";

// A PreToolUse callback hook. SCENARIO_HOOK: deny | allow | rewrite | none
await guarded(async () => {
  const kind = process.env.SCENARIO_HOOK || "deny";
  const inputs = [];
  const hook = async (input, toolUseID) => {
    inputs.push({ input, toolUseID });
    if (kind === "deny") {
      return {
        hookSpecificOutput: {
          hookEventName: "PreToolUse",
          permissionDecision: "deny",
          permissionDecisionReason: "blocked by the host hook",
        },
      };
    }
    if (kind === "allow") {
      return {
        hookSpecificOutput: { hookEventName: "PreToolUse", permissionDecision: "allow" },
      };
    }
    if (kind === "rewrite") {
      return {
        hookSpecificOutput: {
          hookEventName: "PreToolUse",
          updatedInput: JSON.parse(process.env.SCENARIO_UPDATED_INPUT),
        },
      };
    }
    return {};
  };
  const { q } = open(process.env.SCENARIO_PROMPT, {
    hooks: { PreToolUse: [{ matcher: process.env.SCENARIO_MATCHER || "Bash", hooks: [hook] }] },
    permissionMode: process.env.SCENARIO_PERMISSION_MODE || "default",
  });
  const messages = await drain(q);
  return {
    hook_inputs: inputs,
    tool_uses: toolUses(messages).map((b) => ({ name: b.name, input: b.input })),
    result: results(messages)[0] ?? null,
  };
});
