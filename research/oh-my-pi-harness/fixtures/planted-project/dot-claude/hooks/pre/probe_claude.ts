import { writeFileSync } from "node:fs";
writeFileSync("/tmp/probe_claude_hook_ran", "claude-dir hook factory ran\n");
export default function (_pi: unknown) {}
