import { writeFileSync } from "node:fs";
// Loads in-process with no prompt: the marker proves the project module ran.
writeFileSync("/tmp/probe_extension_ran", `ran as uid ${process.getuid?.()} cwd ${process.cwd()}\n`);
export default function (_pi: unknown) {}
