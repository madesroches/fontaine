// Shared plumbing for the consumer programs: environment in, one JSON report out. The programs
// choose the executable only through `pathToClaudeCodeExecutable`.
import { query } from "@anthropic-ai/claude-agent-sdk";

export const bin = process.env.FONTAINE_BIN;
if (!bin) {
  console.error("FONTAINE_BIN is not set");
  process.exit(64);
}
export const model = process.env.SCENARIO_MODEL || undefined;
export const prompt = process.env.SCENARIO_PROMPT || "say hello";

export function baseOptions(extra = {}) {
  const stderr = [];
  const options = {
    pathToClaudeCodeExecutable: bin,
    cwd: process.cwd(),
    stderr: (line) => stderr.push(line),
    ...(model ? { model } : {}),
    ...extra,
  };
  return { options, stderr };
}

export function open(prompt, extra = {}) {
  const { options, stderr } = baseOptions(extra);
  const q = query({ prompt, options });
  return { q, stderr };
}

// A prompt stream the program feeds by hand, so a message can follow a running turn.
export function inputStream() {
  const queue = [];
  let wake = null;
  let ended = false;
  return {
    send(text, extra = {}) {
      queue.push({
        type: "user",
        message: { role: "user", content: text },
        parent_tool_use_id: null,
        ...extra,
      });
      wake?.();
    },
    end() {
      ended = true;
      wake?.();
    },
    async *[Symbol.asyncIterator]() {
      for (;;) {
        if (queue.length) {
          yield queue.shift();
        } else if (ended) {
          return;
        } else {
          await new Promise((r) => (wake = r));
          wake = null;
        }
      }
    },
  };
}

export const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

// Drains a query into a list of message summaries, calling `each` on every message.
export async function drain(q, each = () => {}) {
  const messages = [];
  for await (const m of q) {
    messages.push(m);
    await each(m);
  }
  return messages;
}

export function summarize(messages) {
  return messages.map((m) => ({ type: m.type, subtype: m.subtype ?? null }));
}

export function results(messages) {
  return messages.filter((m) => m.type === "result");
}

export function text(m) {
  return (m.message?.content ?? [])
    .filter((b) => b.type === "text")
    .map((b) => b.text)
    .join("");
}

export function toolUses(messages) {
  return messages
    .filter((m) => m.type === "assistant")
    .flatMap((m) => m.message.content.filter((b) => b.type === "tool_use"));
}

// The one JSON report a program prints, which is the whole of its stdout.
export function report(value) {
  process.stdout.write(JSON.stringify(value) + "\n");
}

// Runs `body`, reporting a thrown error instead of crashing, so a refusal is data.
export async function guarded(body) {
  try {
    report(await body());
  } catch (e) {
    report({ error: String(e?.message ?? e) });
  }
  process.exit(0);
}
