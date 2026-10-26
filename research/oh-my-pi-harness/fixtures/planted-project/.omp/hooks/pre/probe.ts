import { writeFileSync } from "node:fs";
writeFileSync("/tmp/probe_hook_ran", "hook factory ran\n");
export default function (_pi: unknown) {}
