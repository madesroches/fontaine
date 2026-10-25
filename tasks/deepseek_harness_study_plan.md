# DeepSeek Harness Study Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/154

## Overview

Study DeepSeek's own agent harness, DeepSeek Harness (`dsh`), and record what Fontaine should
adopt, adapt or skip. The work covers how it frames prompts, tool calls and reasoning for DeepSeek
models, how it manages context, its safety model, tasks, session log and the feedback it gives the
model. Running DeepSeek models through Fontaine's rig provider path, as the main agent and as a
guardian or screen, is in scope too. This is a research spike. The deliverables are
`research/research-deepseek-harness.md`, the experiment scripts and captured data under
`research/deepseek-harness/`, GitHub issues for anything worth adopting, and dated `## Decisions`
lines only where a finding argues for departing from Claude Code. No product crate changes.

## Current State

**The subject.** `github.com/deepseek-ai/deepseek-harness`, TypeScript, MIT. It is a
"developer preview" that warns of breaking changes. The repo was last pushed 2026-10-23 and has no
release tags. The study pins commit `22ddee3792402c3e70ec5f86a63c3e3e895a2f60` (2026-10-23), and
every source citation in the doc is `path@22ddee37`. Known shape, from a first read:

- It is built on Cordis ("everything is a plugin"). The agent loop, tool registry, session log and
  model adapter are each a replaceable plugin, and a profile (`web`, `headless`, `sdk`,
  `sdk-minimal`, `acp`) is a stack of bundles plus YAML patches (`docs/architecture.md`).
- The core packages are `core/session` (an append-only `SessionEvent` log), `core/system-prompt`
  (prompt sections and tool schemas), `core/tools` (the guarded execution pipeline),
  `core/agent-loop` and `llm/llm`.
- The DeepSeek adapter (`packages/llm/llm-deepseek`) speaks only DeepSeek's
  **Anthropic-compatible Messages** endpoint (`https://api.deepseek.com/anthropic`). It
  "serializes reasoning history back when required" and maps cache-hit metrics. `llm-pi-ai` routes
  every other provider.
- `docs/deepseek-llm-api-wire-extensions.md` documents non-model-visible request additions:
  `x-deepseek-harness-user-id` (a stable anonymous UUID), `x-deepseek-harness-session-id`,
  `x-deepseek-harness-compact`, and `dsh_*` body fields such as `dsh_plugin_packages` and
  `dsh_session_log`. What `dsh_session_log` sends, and whether it is on by default, is a
  privacy finding the doc must settle.
- Packages map onto the issue's questions: `compaction/*` (basic, tool-result pruner, image
  offload), `guard/*` (repeat-tool-reminder, timeout-policy), `sandbox/*` (local, policy,
  Windows ACL), `plan/plan-mode`, `todo/tool-todo`, `goal/*`, `subagent`, `feedback/*`, `lsp`,
  `session`, `session-query`, `spill`, `context`. The docs include `tool-execution-pipeline.md`,
  `agent-lifecycle.md`, `session-format-status.md`, `persistence-catalog.md` and
  `docs/subsystems/*.md`.
- `SAFETY.md`: no security audit. Sandboxing and approvals "reduce risk but do not guarantee
  isolation". Run it in a disposable environment.

**Fontaine today.**

- DeepSeek reaches Fontaine only through the `opencode-go` account kind
  (`rust/fontaine-core/src/providers/opencode_go.rs`). `deepseek-v4.1-flash` is routed to the
  **Chat** wire (`wire()`, line 154), and the guardian turns thinking off with
  `{"thinking": {"type": "disabled"}}` (`thinking_off`, line 175; history in
  `tasks/completed/guardian_thinking_off_plan.md`). The local config has a `go` account and a
  `kimi` account, and no direct DeepSeek API key.
- DeepSeek's thinking-mode guide says that after a tool call, `reasoning_content` must be sent back
  in later turns, or the API returns 400. Whether rig's Chat path does this for the main agent, with
  thinking on, has not been tested. This is the most direct Fontaine risk the study can find.
- An account's `base_url` can be overridden (`providers/mod.rs:183`), so Fontaine's traffic can be
  routed through a recording proxy without code changes.
- Precedent: `research/research-llamafirewall.md` (format of a deep-dive) and the #98 spike
  (`research/analysis-typed-decision-screen.md` with `research/typed-decision-screen/scripts/`,
  for the layout of experiments and data). The master plan lists the research docs at
  `tasks/fontaine_harness_plan.md:53-59`, and `tasks/design/research-findings.md` has one
  subsection per piece of prior art.
- Related open issues that findings may feed rather than duplicate: #13 prompt caching, #53
  reasoning effort, #22 todo tools, #24 plan-mode exit, #10 session UX, #12 checkpoints/rewind,
  #17 /goal, #27 LSP, #59 ACP, #39 forked subagents, #149 and #97 (local guardian), #132
  (OpenRouter scoring).
- Tooling here: Docker is available. Node, npm and pnpm are not installed on the host. bwrap is
  installed.

## Design

### Method

There are two tracks: read the pinned source, and run the harness. The doc states every claim with
its evidence, which is a source citation, a captured request or log line, or a command and its
output. Negative results are recorded as findings ("X does not exist", "Y failed because Z").

### Environment and safety

- dsh runs **only in a Docker container** (`node:22-bookworm`, pinned by digest in the script)
  with a throwaway workspace volume, a throwaway `DSH_HOME`, and no host mounts besides
  `research/deepseek-harness/runs/<run>/`, as `SAFETY.md` asks. The container's network is
  needed for the model API and the npm install. Credentials enter the container only as the one
  gateway key, passed by env.
- It is built from the pinned checkout (`pnpm install && pnpm run build`), not with
  `npx @deepseek-ai/dsh@latest`, so the binary matches the cited source. If the build fails,
  fall back to the npm package version that matches the commit and record the mismatch.
- The `headless` profile is the driver, so experiments are scripted and repeatable. The Web UI
  is opened once, to check that headless behaviour matches the default surface, then it is
  dropped.

### Wire capture

`research/deepseek-harness/scripts/record_proxy.py` is a small stdlib-only (`http.server` plus
`urllib`) reverse proxy. It forwards to a configured upstream, streams SSE through unchanged, and
writes each exchange as JSONL (request headers, request body, response status, and the response
chunks joined into one body). Before writing, it **redacts** `authorization`, `x-api-key` and any
header or field whose name contains `key`, `token` or `secret`. Both harnesses point their base
URL at it:

- dsh: `llm-deepseek` `baseURL = http://host.docker.internal:<port>`, upstream
  `https://opencode.ai/inference/go/anthropic`, model `deepseek-v4.1-flash`. If the Go gateway
  does not serve DeepSeek on its Messages path, fall back to an `llm-pi-ai` OpenAI-compatible
  route to the Go Chat path, and record that the native adapter was not exercised.
- Fontaine: a temporary `go` account copy with `base_url` set to the proxy, so the same model
  runs on Fontaine's normal Chat path.

The captures answer the framing, reasoning, caching and telemetry questions from what is actually
sent, not from the docs. Before any capture is committed, a scrubber pass
(`scripts/scrub.py`) removes keys and the anonymous user id. Bulky full captures stay gitignored
under `runs/`. The specific excerpts the doc cites are committed under
`research/deepseek-harness/captures/`.

### Experiments

Each experiment has a script under `scripts/`, a task fixture under `fixtures/` (a tiny repo),
and a results section in the doc. Before an experiment runs, the plan states the question it
answers.

| # | Experiment | Answers |
|---|---|---|
| E1 | Build, boot `headless`, run `--dump-config`, and record the default plugin tree | architecture; which defaults are on (telemetry, `dsh_session_log`) |
| E2 | One read-only question with the wire captured | system-prompt sections, tool schemas, tool-call and result format, headers, `dsh_*` fields |
| E3 | Fix a failing test in a fixture repo (edit → run tests → iterate) | tool errors, the test-output feedback loop, repeat-tool-reminder, multi-turn reasoning round-trip |
| E4 | Risky actions in the fixture (write outside the workspace, `rm -rf` of a parent, `curl … \| sh`, reading `~/.ssh` in the container) | sandbox and approval policy, defaults, what a denial says to the model; compared with Fontaine's floor and guardian |
| E5 | A long session that crosses the compaction threshold (lower the configured window) plus a large tool result | compaction trigger, summary prompt, tool-result pruning and spill, cache-hit accounting across compaction |
| E6 | A multi-step task in plan mode, then with the todo tool, then with one subagent | task representation, todo state, plan handoff, subagent context |
| E7 | Resume the E3 session, then inspect the on-disk log and `session-query` | session log format, durability, resume and review |
| E8 | Run E2 and E3 through `fontaine -p` on the same model, with thinking on, through the proxy | whether rig sends `reasoning_content` back on the Chat wire; a side-by-side diff of framing against dsh |
| E9 | Live guardian scorecard: `cargo run -p fontaine-evals -- --dataset ../evals/dataset --guardian live:go/deepseek-v4.1-flash`, at the default timeout | DeepSeek as a guardian; compared with `evals/scorecards/kimi-for-coding.json` |
| E10 | Where dsh's behaviour differs from what Fontaine copies, capture the Claude Code baseline with `claude -p` on the same fixture task | the "different from Claude Code" column |

E9 is a full live bench, so it runs at the default timeout (memory: live bench timeouts). Its
scorecard goes to `evals/scorecards/deepseek-v4.1-flash.json`. It is a measurement, not a merge
gate.

Anything that cannot be run goes into a "Not tried" list with the reason (for example, no direct
DeepSeek API key, so `api.deepseek.com` Files and context caching are reached only through the
gateway, or the build fails). The user can then supply what is missing.

### The research doc

`research/research-deepseek-harness.md` follows the shape of `research-llamafirewall.md`:

1. Executive summary and bottom line for Fontaine.
2. Identity: repo, license, pinned commit, maturity, how it runs.
3. Architecture (Cordis, profiles, the turn flow).
4. One section per issue question: prompt and tool framing; reasoning output; context management
   (caching, compaction, truncation); permission and safety model; DeepSeek through rig, including
   as a guardian or screen; tasks; session log; model feedback. Each section has "What dsh does",
   "Claude Code", "Fontaine today", the evidence, and a verdict.
5. Telemetry and privacy: what leaves the machine by default (headers, `dsh_*` fields).
6. An **Adopt / Adapt / Skip** table. Each row links to the issue it filed, the existing issue
   it feeds, or the reason it is skipped.
7. Experiment log (E1–E10), with commands, results and failures.
8. Not tried, and why.

### Follow-ups and decisions

- For each Adopt or Adapt row, file an issue (`gh issue create`, with labels matching the area) or
  comment on the existing issue it feeds, linking the doc section. A bug found in Fontaine (E8 is
  the likely candidate) is filed as a `bug` with its reproduction.
- A finding that argues for departing from Claude Code semantics gets a dated line in the master
  plan's `## Decisions`. Adopting something Claude Code also does needs no Decisions line.
- The doc and the PR body both carry the full findings list, including the negative ones.

## Implementation Steps

1. **Scaffold** `research/deepseek-harness/` with `scripts/`, `fixtures/`, `captures/` and a
   `.gitignore` for `runs/`. Add `README.md` there, covering how to reproduce each experiment and
   the pinned commit and image digest.
2. **Source read.** Read the pinned checkout's docs and packages listed in Current State and take
   notes per question, with `path@22ddee37` citations. Use Explore subagents for the broad
   package sweep, and read the cited files directly before quoting them.
3. **Tooling.** Write `record_proxy.py` and `scrub.py` (stdlib only, run with `python3 -I`),
   `run_dsh.sh` (the docker invocation: build from the checkout, headless profile, proxy base
   URL, a run directory), and `run_fontaine.sh` (a temporary config with a proxied `go` account,
   then `fontaine -p`).
4. **E1–E7** on dsh, in order. Record each result in the doc as it lands. E4 runs only inside the
   container.
5. **E8** through Fontaine. If rig drops `reasoning_content`, reproduce it minimally, file the
   bug, and note whether the Messages wire avoids it.
6. **E9** live guardian bench, plus the scorecard file.
7. **E10** `claude -p` baselines, only for the rows where the Claude Code behaviour is in doubt.
8. **Write the doc.** Fill in the summary, the Adopt/Adapt/Skip table, the experiment log and the
   Not tried list.
9. **Follow-ups.** File and comment on issues, then link them from the table. Add a `## Decisions`
   line only where the rule above applies.
10. **Index.** Add the doc to the research list in `tasks/fontaine_harness_plan.md:53-59`, and
    update the count of "seven companion docs". Add a short "DeepSeek Harness" subsection to
    `tasks/design/research-findings.md` pointing to the deep-dive.

## Files to Modify

- New: `research/research-deepseek-harness.md`
- New: `research/deepseek-harness/{README.md,.gitignore,scripts/*,fixtures/*,captures/*}`
- New: `evals/scorecards/deepseek-v4.1-flash.json` (E9)
- `tasks/fontaine_harness_plan.md`: the research doc list, and `## Decisions` only if needed
- `tasks/design/research-findings.md`: a pointer subsection

## Trade-offs

- **Container rather than the host or bwrap.** Container isolation is what `SAFETY.md` asks
  for, and it supplies Node without installing it on the host. bwrap would need Node on the host
  anyway.
- **Build from the pinned source rather than `npx`.** Citations and behaviour then refer to the
  same code. It costs a pnpm build, and `npx` is the fallback.
- **Run through the opencode-go gateway rather than a direct DeepSeek key.** It is the only
  DeepSeek access available, and it is the path Fontaine users take. The cost is that
  gateway-specific behaviour (caching, Files API, wire coverage) may differ from
  `api.deepseek.com`. Each such gap is recorded in Not tried.
- **A stdlib recording proxy rather than mitmproxy.** Both harnesses accept a plain-HTTP base
  URL, so TLS interception is not needed and there are no new dependencies.

## Documentation

No `docs/` change. This is research, and `docs/` describes shipped behaviour. If E9 is published
as a scorecard, `docs/evals.md` is left to the release-scorecard issue (#104). The research doc and
the two index updates in Step 10 are the documentation deliverable.

## Testing Strategy

There is no product code, so nothing is added to `cargo test`. The scripts are research tooling,
in the same tier as `research/typed-decision-screen/scripts/`. They are checked by running them:
each experiment's recorded output is the check. One exception: `scrub.py` runs over every
committed capture, and a final `rg -i 'sk-|bearer|api[-_]?key'` over `research/deepseek-harness/`
must find nothing before commit, because a leaked key would fail silently.

## Open Questions

- Does the Go gateway serve `deepseek-v4.1-flash` on its `/anthropic` (Messages) path? This
  decides whether dsh's native adapter can be exercised at all. Answered by the first run of E2.
- Is `dsh_session_log` on by default, and what does it upload? Answered by E1 and E2.
