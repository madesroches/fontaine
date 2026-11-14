import { open, inputStream, drain, results, guarded } from "./lib.mjs";

await guarded(async () => {
  const input = inputStream();
  const { q } = open(input, { permissionMode: "default" });
  input.send("say hi");
  const out = {};
  const attempt = async (name, fn) => {
    try {
      out[name] = { ok: (await fn()) ?? null };
    } catch (e) {
      out[name] = { error: String(e?.message ?? e) };
    }
  };
  let n = 0;
  await drain(q, async (m) => {
    if (m.type !== "result" || ++n > 1) return;
    await attempt("set_mode_accept_edits", () => q.setPermissionMode("acceptEdits"));
    await attempt("set_mode_plan", () => q.setPermissionMode("plan"));
    await attempt("set_mode_bypass", () => q.setPermissionMode("bypassPermissions"));
    await attempt("set_mode_afk", () => q.setPermissionMode(process.env.SCENARIO_AFK_MODE || "afk"));
    await attempt("set_model", () => q.setModel(process.env.SCENARIO_NEW_MODEL));
    await attempt("set_model_unknown", () => q.setModel("no-such-model-anywhere"));
    await attempt("mcp_status", () => q.mcpServerStatus());
    await attempt("context_usage", () => q.getContextUsage());
    input.end();
  });
  return out;
});
