import { createSdkMcpServer, tool } from "@anthropic-ai/claude-agent-sdk";
import { z } from "zod";
import { open, drain, results, toolUses, guarded } from "./lib.mjs";

await guarded(async () => {
  const calls = [];
  const server = createSdkMcpServer({
    name: "host",
    version: "1.0.0",
    tools: [
      tool("add", "Adds two numbers", { a: z.number(), b: z.number() }, async (args) => {
        calls.push(args);
        return { content: [{ type: "text", text: String(args.a + args.b) }] };
      }),
    ],
  });
  const asked = [];
  const { q } = open(process.env.SCENARIO_PROMPT, {
    mcpServers: { host: server },
    permissionMode: process.env.SCENARIO_PERMISSION_MODE || "default",
    canUseTool: async (toolName, input) => {
      asked.push(toolName);
      return { behavior: "allow", updatedInput: input };
    },
  });
  const messages = await drain(q);
  const init = messages.find((m) => m.type === "system" && m.subtype === "init");
  return {
    calls,
    asked,
    mcp_servers: init?.mcp_servers ?? null,
    tools: init?.tools ?? null,
    tool_uses: toolUses(messages).map((b) => b.name),
    result: results(messages)[0] ?? null,
  };
});
