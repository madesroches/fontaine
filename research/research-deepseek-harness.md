# Research — DeepSeek Harness (dsh): what Fontaine can learn from it

Date: 2026-10-24. Issue #154. Subject: `github.com/deepseek-ai/deepseek-harness`, pinned at commit
`22ddee3792402c3e70ec5f86a63c3e3e895a2f60` (2026-10-23); every source citation is `path@22ddee37`.
Method: read the pinned source, built it and ran it in Docker against the opencode Go gateway
(`deepseek-v4.1-flash`) with the wire captured, ran Fontaine on the same fixture through the same
recording proxy, and took a `claude -p` baseline where Claude Code's behaviour was in doubt.
Experiments, scripts and excerpts: `research/deepseek-harness/` (README there reproduces each one;
`captures/<file>` names below are relative to it). No benchmark and no live scorecard: this is a
feature study. Each claim carries its evidence: a source path, a capture file, or a command.

## 1. Executive summary

dsh is DeepSeek's own agent harness: TypeScript, MIT, an explicit "developer preview" with no
release tags, built as a tree of Cordis plugins (agent loop, tools, session log, model adapter are
each a plugin; a profile is a stack of YAML patches). It talks to DeepSeek only through the
Anthropic-compatible Messages endpoint.

**What lets the model iterate.** dsh's iteration is not magic and not a test-runner: its system
prompt contains no "verify your work" text at all. It comes from a handful of small mechanisms, all
measured here:

1. A **durable goal** continued in rounds by a driver (`create_goal`, a fixed `<goal_round>` prompt that
   says "make concrete progress and verify the result", a 256-round cap, `blocked` gated behind
   three consecutive rounds, a wrap-up message once the model completes it).
2. A **repeated-identical-call reminder** after the 3rd, 5th and 8th consecutive identical call.
   Measured: it made the model stop and diagnose even though the user had asked for nine repeats.
   Claude Code, same task: nine calls, no nudge.
3. **Tool results shaped for self-correction**: bash output ends in `[exit code: N]` and a non-zero
   exit is not a tool error; edit and write errors say what to do next ("read the file, then retry");
   a hung command is **promoted to a background job** rather than killed (Claude Code does the same,
   measured); a tool with a declared timeout returns `Error: tool call timed out after 5000ms`.
4. **Bounded context growth** (spill to file, prune old middles, then a structured summary).

**What the DeepSeek model needs.** The documented hard requirement (return `reasoning_content` or get
a 400) **did not reproduce** through the Go gateway on either wire, for either `deepseek-v4.1-flash` or
`deepseek-v4-pro`, in seven hand-built passback variants plus real sessions, including the case the plan flagged as
the most direct Fontaine risk (a tool-call turn with no reasoning after a resume from another model).
Fontaine round-trips reasoning on the Chat wire correctly. What is needed is cache-prefix hygiene
(measured), usage-field handling, and a direct-API check that this environment cannot do (#164).

**Bottom line for Fontaine.** Adopt, in order of value: goal rounds that survive headless (#17), the
repeat reminder (#160, a Claude Code departure, recorded in Decisions), background-job promotion on
timeout (#25), spill-and-prune for large results (#161), and a model-facing session search (#162).
Do not copy: the default upload of the whole session log to the model endpoint, the sandbox's
"permission denied means policy denial" heuristic, and prompt-only plan mode. Fontaine's existing
choices held up: its shell tool already returns the exit code and trims large output, its read-before-edit
tracking matches dsh's, and its guardian worked on `deepseek-v4.1-flash` with thinking off in all ten
calls observed (that is an observation, not a scorecard; #163 files the measurement).

## 2. Identity

- Repo `deepseek-ai/deepseek-harness`, TypeScript, MIT, package `@deepseek-ai/dsh` 0.2.1-rc.3
  (`package.json@22ddee37`), Node >= 24.9. Last push 2026-10-23, no tags. `SAFETY.md@22ddee37`: no
  security audit, sandbox and approvals "do not guarantee isolation", run in a disposable environment.
- Profiles: `web`, `headless`, `sdk`, `sdk-minimal`, `acp`. `dsh --profile headless "<task>"` runs one
  persisted session and exits; exit 0 only if the last turn completed (`packages/bundle/headless/README.md`).
- How it was run here: image `dsh-study:22ddee37` built from the checkout (`pnpm install`,
  `build:artifacts`, `build:native`; the build succeeded first time, 334 s), Node 24.22.1, non-root
  uid 1000, no internet. Source execution needs `TSX_TSCONFIG_PATH=/dsh/tsconfig.base.json` when the
  working directory is not the repo root, otherwise tsx resolves the built `@deepseek-ai/cordis` and
  fails with `does not provide an export named 'FiberState'` (a finding: a `const enum` import that
  only works through the repo's tsconfig paths).

## 3. Architecture and turn flow

- 99 plugin rows compose the headless tree (`captures/e1_plugin_ids.txt`, from `--dump-config`). The model
  patch (catalog entry copied from `deepseek-flash` onto the gateway id, default model set) landed
  (`runs/e1-dump/config.yml`, lines for `agent-default-model` and `llm-deepseek`).
- Turn flow (`docs/tool-execution-pipeline.md`): `tool/call` logged; `tools/pre-execute` waterfall (hooks,
  permission, sandbox); approval if `ask`; monotonic guards (repeat-tool-reminder, timeout-policy);
  `tools/execute` waterfall; tool body; `tools/post-execute`; `tool/result` logged. Every step is a plugin.
- Model-visible runtime state is **appended**, never edited into the system prompt: a "Current runtime
  context" user message carries the cwd, the sandbox policy and the approval policy, and is superseded by
  later snapshots. This is the cache discipline in one pattern.

## 4. One section per question

### 4.1 Prompt and tool framing

**What dsh does.** The system prompt is an ordered registry of sections contributed by the tools
themselves (`packages/core/system-prompt/src/index.ts`). The captured prompt (`captures/e1_request_main.json`)
is: "You are an AI agent powered by DeepSeek Harness." / "You are a coding agent powered by the
deepseek-v4.1-flash model." and then one or two sentences per tool: "Check the [exit code: N] marker on
every bash result; investigate failures before moving on."; "Use the read tool, not shell commands like
cat"; "Read a file before editing it ... unless you just created or edited it in this session"; the goal,
jobs, workflow and subagent paragraphs. There is no tone, plan-first, style or commit guidance. 24 tools
are sent (bash, create_goal, edit, get_goal, glob, grep, interrupt_agent, job_kill, job_list, job_output,
list_agents, read, read_image, send_message, skill, subagent, subagent_fork, todo_write, update_goal,
web_fetch, web_search, workflow, working_directory, write). The first request of a session carries no
tools: it is the title generator, `thinking: disabled`, `max_tokens 4096` (`captures/e1_request_title.json`).
Tool schemas are terse; parameter descriptions fix the argument order ("provide `description` before
`command`"). The first captured main request: 5,094 input tokens (512 cached) at 24 tools.

**Claude Code.** Not measured here.
**Fontaine today.** One narrative prompt ("You are Fontaine, a terminal coding agent ...") and 11 tools
(read, ls, glob, grep, write, edit, shell, webfetch, agent, skill, send_message); the first request is 3,386
input tokens (`captures/e8_framing_compare.txt`). Verdict: dsh's approach shows that a very thin prompt
works for this model when the tool results carry the guidance; it is not evidence that Fontaine's prompt
should shrink. No change proposed.

### 4.2 Reasoning output

**What dsh does.** Thinking on by default, `output_config.effort: high`, `max_tokens 256000`
(`captures/e1_request_main.json`). The reasoning of every turn is stored and replayed as a `thinking` block
(no `signature`: the gateway returns none, and it accepted the replay). Replay metadata is kept only when
the model matches; otherwise signatures are dropped and text, thinking and tool history stay
(`packages/llm/llm-deepseek/src/replay.ts`, `serialize.ts`). Headless streams reasoning to stderr under
`dsh: reasoning:`; `--json` projects it as `thinking` events.
**Fontaine today.** Chat wire, no thinking parameter on the main agent, so the model thinks by default
(P8 below: yes, on both models). Reasoning comes back as `reasoning_content` and rig replays it under the
same field. Measured round-trip (E8, `captures/e8_roundtrip.txt`): in the 15-request fix run, 12 of 12
reasoning-bearing turns were sent back with `reasoning_content`; 0 dropped. Verdict: correct as is.

### 4.3 Context management

**What dsh does** (all measured in E5/E2, sources `packages/compaction/*`, `packages/spill/*`):

- **Spill at result time.** A result over 12,500 estimated tokens is stored whole in a spill file; the model
  sees head and tail and "(Omitted N bytes. Full formatted result stored at: <path>. Use read with
  offset/limit, or grep this path to search within it.)" (`captures` do not hold it; the footer is in
  `runs/e5-compact/session.jsonl` seq 29). The `read` tool is exempt. bash additionally keeps a 64 KB tail
  and a separate full-output file: "[output truncated; full output: <path>]".
- **Prune at pressure.** Over the threshold, every tool result over 8,192 chars loses its middle (head 4,096,
  tail 1,024) before any model call; each prune is a logged `compaction/prune` that shadows the original.
- **Summarise last.** Threshold = floor(min(W x 0.8, W - O - 65,536)); with W = 1,048,576 and O = 256,000 that
  is min(838,860, 727,040) = 727,040 tokens (the study plan's 840k ignored the output reservation), so a normal session never compacts. The summary is one user message from a fixed
  8-section template (primary request, concepts, files, errors and fixes, pending, current work, next step,
  critical context), written by the same model on the same request prefix with `x-deepseek-harness-compact: 1`
  and `max_tokens 65536`. Real output: `captures/e5_compaction_summary.txt`.
- **First attempt did nothing.** With `thresholdRatio 0.05` (52,428 tokens) the 8-chunk dump session never
  reached the threshold: spill and bash truncation kept the whole context near 21k tokens
  (`captures/e5_usage.txt`). Lowering to 0.015 (15,728; retain 0.005) fired one compaction after the 4th
  chunk, preceded by four prunes of 12,491 tokens each (`captures/trace_e5-compact2.txt`).
- **Cache across compaction** (`captures/e5_usage.txt`): cache_read 12,928 on the last pre-compaction request,
  the summary request itself 11,520 (same prefix), the first post-compaction request 5,376 (system + tools
  only), then 10,496 and 11,904 as the new history re-cached. One request of cache loss per compaction.
- **Tool schema vs prefix.** E7 added five session-search tools to a resumed session: the first request reused
  only 4,096 of ~16k tokens (`captures/e7_usage.txt`), the next 15,872. Changing the tool array breaks the
  prefix from the first altered token, as `llm-deepseek/README.md` says.

**Claude Code.** Not measured here.
**Fontaine today.** `shell` returns first and last lines plus the lines with errors past 2000 lines / 50 KB
and names no file holding the full output (read from the shell tool description in the live E8 request). Verdict: adopt spill-to-file and the
pre-summary prune (#161); the summary template is a candidate for whatever rig-memory writes today.

### 4.4 Permission and safety model

**What dsh does.** No command parsing, allow/deny lists or protected paths. Two layers: a confinement
sandbox over **file writes only** (`read-only`, `workspace-write`, `danger-full-access`; Landlock or
bwrap on Linux, Seatbelt on macOS, a restricted-token ACL runner on Windows; no network or syscall policy)
and per-call approval used for sandbox escalation. Defaults in headless: `workspace-write` + `ask`.

Measured (E4, container only, `captures/trace_e4-*.txt`):

- **Linux runner.** bwrap is not in the image; the denial `bash: line 1: /home/node/outside.txt: Permission
  denied` followed by `[sandbox: file access denied under workspace-write mode]` shows Landlock confining the
  command. The reported `enforcement` value is not surfaced anywhere the model or logs show it.
- **The model-visible denial** is `[sandbox: file access denied under workspace-write mode]` then
  `[sandbox: escalation available - retry this exact command once with sandbox_permissions ... the approval
  prompt asks the user]` then `[exit code: 1]`.
- **Under `ask` with no answerer** the escalation request logs `approval/asked` then `approval/decided
  unavailable`, and the model sees `Error: sandbox escalation to "danger-full-access" requires approval, but no
  approval channel is available` (an error result, not a command result).
- **Under `--approval-policy never` the `rejected` text was never produced**: the runtime-context sentence
  "Approval prompts are disabled ... do not request sandbox escalation (do not set `sandbox_permissions`)" made
  the model refuse to set it even when the user told it to (`captures/trace_e4-never2.txt`). The rejected text
  ("the user rejected escalating this command ... stop and explain instead of working around it") is
  source-only (`packages/sandbox/sandbox/src/escalation.ts`).
- **Reads are not confined**, only writes. `cat /proc/1/environ` failed with plain EACCES (uid), yet dsh labelled
  it `[sandbox: file access denied ...]` and offered escalation. The attribution is a stderr substring match on
  "permission denied" (`packages/sandbox/sandbox-local/src/index.ts`). Do not copy: a wrong attribution invites the
  model to ask for danger-full-access for a problem the sandbox did not cause.
- `curl ... | sh` printed nothing (no route; the egress filter, not dsh, stopped it). `~/.ssh` did not exist.
  `env | grep -i key` inside the bash tool printed nothing although the dummy key is in the container's process
  environment, so the tool does not pass the parent environment through; the mechanism was not traced.

**Claude Code / Fontaine.** Fontaine's floor (`deny -> ask -> critical-path -> protected-path`), guardian and
workspace-containment fast path are stricter in every respect measured here. dsh has nothing comparable to the
guardian: a "permission mode" is a sandbox mode. Verdict: nothing to adopt. The one idea worth keeping is the
escalation round-trip being explicit and one-shot (the model re-issues the exact command with a justification),
which Fontaine's denial/retry UI already models.

### 4.5 DeepSeek through rig, including as a guardian or screen

Source and history only, plus the E8 observations below; no verdict-quality measurement (#163).

- Fontaine routes `deepseek-v4.1-flash` to the **Chat** wire (`opencode_go.rs wire()`); the **Messages** wire also
  works on the gateway with `x-api-key` (Fontaine probe and dsh both succeeded), so dsh's native adapter was
  exercised unmodified.
- Fontaine's guardian runs on the same model with `{"thinking":{"type":"disabled"}}`. In the E8 fix run all 10
  guardian exchanges returned 200 with a parseable `submit` verdict (all benign `allow`, confidence 0.80-0.95,
  1.5-2.5 s), and `prompt_tokens_details.cached_tokens` shows the guardian prefix cached from the second call
  (`captures/e8_guardian_calls.txt`). Thinking-disabled requests behave as documented on both wires (P7, P9).
  This says the route is usable, not that the verdicts are good (#163).
- A confound to know about: Fontaine runs on the host, which has no node, so in the E8 fix run the model
  could not run `npm test` and improvised a harness under another interpreter, while dsh ran in the container.
  The side-by-side is therefore about framing and wire behaviour, not task success.

### 4.6 Tasks (todo, plan, goal, subagents)

- **Todo.** `todo_write` replaces the whole list (`content`, `status` only); result "Updated todo list: 0
  pending, 1 in progress, 2 completed." The list is **never re-shown** to the model: the event is log-only UI
  state, so the model sees it only as its own earlier tool calls (`packages/todo/tool-todo/src/index.ts`).
  Measured (`captures/trace_e6-todo.txt`): the model kept the list current across a 5-step task, 4 updates.
  This does not argue for re-injecting the list; #22 is unaffected.
- **Plan mode** is prompt-level: a `plan:policy` system section while active, the same tools listed ("the tool
  catalog stays the same across modes for request-cache stability"), enforcement left to the sandbox, and
  `exit_plan_mode` asks the user to Approve or Keep planning. Not driven here (Web UI only; see Not tried).
- **Goal.** See 6.1. Verified end to end except rounds in the Web UI.
- **Subagents** are asynchronous: `subagent` returns "started subagent <id>", the child gets a fresh
  context (spawn) or the parent's log up to the last turn end (fork), runs with approval pinned to `never`,
  and reports with `send_message` before finishing; the parent receives it as a user-role relay message and a
  `subagent-settled` notice. Measured in `captures/trace_e6-subagent.txt`; the child called `send_message` itself
  and the settled notice said its closing message was omitted because it had already sent one. The parent
  also polled `list_agents` once before the result arrived. Fontaine's `agent` tool already has
  `background` and a `send_message`; #39 covers forks.

### 4.7 Session log

- Append-only JSONL, zstd frames, `session.v4.jsonl.zstd` under `$DSH_HOME/sessions/--<cwd>--/<id>/`, format version
  4 with frozen codecs v0-v3. Events: 40-odd types (turn, step, message, tool, approval, sandbox, goal, todo,
  compaction, request/header, request/context, job, subagent, ...). Compaction is a `surfaceOp: replace` over a
  seq range, so nothing is deleted. Durability: each batch fsynced, torn tails discarded, a single-writer lease,
  and a checkpoint policy that flushes before every model request and before every tool with external effects
  (source: `packages/session/session-persistence-jsonl/README.md`).
- **Resume.** `--session-id <id>` re-adopts it (E7): the same cwd is required, the model remembered the earlier
  diagnosis, and `captures/trace_e7-resume.txt` shows it using `session_search` (no hits, it only searches
  *prior* sessions) and then `session_event_search` and `session_event_read` for the current one.
- **No rewind.** `rewind`/checkpoint-as-a-feature does not exist; `fork` is an API used by forked subagents;
  `workspace/changes` snapshots a git diff for the Web UI, which is not a restore.
- Fontaine: resume exists, #10 and #12 track the rest. A model-facing search is new (#162).

### 4.8 Feedback to the model

Collected in 6.1 and 6.2. The negative findings: `packages/feedback/*` is human ratings that never reach the
model; there is **no LSP diagnostics after edits** (the `lsp` tool is read-only navigation, mounts nothing by
default, ships no servers, and `lsp-stdio` ignores diagnostics notifications at `src/connection.ts:248`); there is
no test-runner tool and no "run the tests" prompt text anywhere. Everything the model learns about its own work
comes from the shell result, which is why 6.1 item 3 matters.

## 5. Telemetry and privacy

Observed in E1 (`captures/e1_endpoints.txt`, `captures/e1_request_main.json`) with telemetry left on:

- **Every model request carries**: `user-agent: deepseek-harness/0.2.1-rc.3 (+url)`, `x-deepseek-harness-user-id`
  (a random UUID kept in `$DSH_HOME/.anonymous-user-id`), `x-deepseek-harness-session-id`, and the body fields
  `dsh_plugin_packages` (88 packages with versions) and `dsh_session_log`.
- **`dsh_session_log` is on by default and uploads the session.** It is incremental (`afterSeq`/`throughSeq`,
  at most 8 MiB per request), mounted in the base bundle, and the endpoint is whatever base URL is configured,
  so the Go gateway received it too. What was on the wire (E2, `captures/e2_session_log_event_types.txt`): the
  full system prompt, the user prompt, every tool call and result including **file contents the model read**
  (`replace(/ /g` from the fixture's source is in the payload), the cwd, titles and approvals. Reasoning is part
  of `assistant/message` events; not seen in this capture because that model turn had none. Cost: the first
  request grew from 7,166 to 34,895 bytes (title) and 29,839 to 57,568 bytes (main) with the log on
  (`captures/e2_session_log_on_off.txt`); `enabled: false` removes the field and nothing else changed.
- **Telemetry.** `session-telemetry-otel` defaults to `FEEDBACK_ONLY`: it exports only after an explicit
  `/feedback`, so with none given the pcap shows no contact with `dsh-otel-collector.deepseeksvc.com`.
- **Every endpoint the container contacted**: DNS for `fwd` only (5 queries), TCP to `fwd:18080`, mDNS
  multicast probes to 224.0.0.251:5353 (no answer possible on an internal network). `web_search` with
  `DEEPSEEK_SEARCH_BASE_URL` pointed at the gateway returned HTTP 400 "Request is missing x-opencode-session"
  (so dsh's search path cannot use the Go gateway as is).
- `web_fetch` reaches loopback (the stall server on 127.0.0.1 was fetched), so it has no SSRF guard
  (`packages/web/web-fetch-http/README.md`).

Verdict for Fontaine: do not adopt any default upload; if a feature needs the session it must be opt-in and
labelled. This also means anyone running dsh against a third-party gateway sends that gateway their sessions.

## 6. Desirable features

Ordered by how directly they let the model iterate on a solution. "CC" is Claude Code.

### 6.1 Iteration

1. **Goal rounds that outlive the turn.** *dsh:* `create_goal` stores `{objective, phase, roundsStarted,
   maxGoalRounds 256}`; when the agent goes idle with an armed goal the driver queues a follow-up
   `<goal_round>` message: "Continue working toward the objective in this same session. Treat the current
   workspace, tool results, and durable session state as authoritative; inspect them instead of assuming earlier
   narration is still current. Make concrete progress and verify the result. Before claiming completion, gather
   evidence that the whole objective is achieved, read the current goal, and mark it complete. If work remains,
   leave the goal active ..." (`packages/goal/goal-round-driver/src/prompt.ts:18`). The model ends it with a
   compare-and-set `update_goal complete|blocked`; `blocked` inside a goal round needs three consecutive rounds
   (`packages/goal/tool-goal/src/index.ts`), and completion injects a wrap-up message asking for outcome and
   verification. *Evidence:* E3 `captures/trace_e3-goal.txt`: asked to make `npm test` pass editing only `src/`,
   the model found the script itself was broken, and called `update_goal blocked` in its first turn
   (accepted at round 0: the three-round rule is evidently for automatic rounds only), which ended the loop. `captures/trace_e3-goal2.txt`:
   with an armed goal and no work, the headless run printed the answer and then **exited 1**, with `turn 1 completed`
   and `turn 2 aborted (disposed)`: the first goal round started and was killed by process exit, as the source
   predicted. So `-p` with an active goal is broken in dsh; rounds need the Web UI. *Fontaine:* feeds **#17**
   (comment filed). A Fontaine `-p /goal` must keep the process alive through rounds and make the exit code
   reflect the goal outcome. *CC:* has `/goal`; no Decisions line needed.
2. **Repeat-call reminder.** After the 3rd, 5th and 8th identical consecutive call (tool name + deep-sorted
   arguments; a user message resets; denied calls count) dsh appends a user-role notice: "You are repeating the
   exact same tool call with identical arguments. Carefully analyze the previous result before calling again: if
   the task is not complete, try a different approach or different arguments instead of repeating the call."
   (`packages/guard/repeat-tool-reminder/src/index.ts:71`), then a detailed one naming the tool, count and arguments.
   *Evidence:* E3 first trigger (`captures/trace_e3-repeat.txt`): the model reran a failing check inside a loop,
   so nothing fired (a model that batches avoids the guard); second trigger (`captures/trace_e3-repeat2.txt`):
   one call per turn, the notice arrived after call 3 (`src=repeat-tool-reminder`), and the model stopped at 3 of
   the 9 the user demanded, read `check.sh` and explained it can never pass. *CC baseline (E9,
   `captures/trace_e9-repeat.txt`):* all 9 calls, exit code 1 each, no message. *Fontaine:* **new #160**, advisory only;
   departs from CC, so a Decisions line was added.
3. **Shell results that carry the failure signal.** bash output is stdout, then `[stderr]\n...`, then
   `[exit code: N]` only when non-zero; `(no output)` when empty; a non-zero exit is not a tool error. One
   system-prompt line tells the model to check the marker (`packages/shell/tool-bash/src/render.ts:60`). *Evidence:* every
   E3 trace; in the fix run the model read the Node failure, probed four hypotheses about `node --test test/`
   (it was a Node 24 behaviour: a directory argument is treated as a module) and fixed both the code and the
   script in 13 steps (`captures/trace_e3-fix.txt`, `captures/e3_fix_usage.txt`: cache_read grew
   4,096 -> 13,312 as the history lengthened). A caveat: the model piped output through `| tail`, which hides
   the exit code; the harness cannot prevent that. *Fontaine:* `shell` already returns the exit code and trims
   (E8 request); nothing to add beyond the wording check in the existing tool description. *CC:* "Exit code 1"
   prefix (E9). Same idea.
4. **Hang handling: promote, do not kill.** With `promoteOnTimeout` (default) a foreground command that
   outlives `timeoutMs` keeps running as a job and the result says `[still running after 10000ms; moved to
   background job bash-...]`, with the output so far; `job_output(wait)`, `job_kill` and `job_list` follow, and a
   job completion wakes the agent with a notice. *Evidence:* `captures/trace_e3-promote.txt`: the result text
   above, then the model waited twice (`(no new output) [status: running]`), and killed the job. The other timeout:
   a tool that declares `timeoutMs` (`web_fetch`, `web_search`) returns `Error: tool call timed out after 5000ms`
   (`captures/trace_e3-stall.txt`; the model then diagnosed the stall server with curl). *CC baseline (E9,
   `captures/trace_e9-timeout.txt`):* "Command did not complete within its 10s timeout and was moved to the
   background (ID: ...)": **the same design**. *Fontaine:* `shell` kills at `timeout_secs`; **#25** (comment filed)
   is the home; no Decisions line.
5. **Edit that cannot silently clobber, and says what to do.** Read-before-edit is enforced by a version
   compare-and-set: `cannot modify "<path>": file has not been read - read the file, then retry`; stale:
   `cannot edit "<p>": file changed since it was read - re-read the file, then retry`; ambiguity: `old_string
   matched N times in "<path>"; provide a more specific old_string or set replace_all to true`. (Sources
   `packages/fs/fs-observation-policy/src/index.ts`, `packages/fs/tool-fs/src/error.ts`.) Not triggered by the runs here;
   source only. *Fontaine:* already has read-before-edit tracking (`builtins.rs`); check the error wording only.
   *CC:* same.
6. **Bounded context so long iteration survives**: spill, prune, summarise (4.3). *Fontaine:* **new #161**.
7. **Recoverable history** (4.7): the model can search what compaction hid. *Fontaine:* **new #162**.
8. **Async subagents with relay** (4.6): feeds **#39** (comment filed).

Not iteration mechanisms despite appearances: `packages/feedback/*` (human ratings), `todo_write` (state is
never re-injected), LSP (navigation only), `plan-mode` (prompt text).

### 6.2 Other features

- **Cache discipline as a design rule.** Append-only history; changed system prompt appended after the cached
  history (`systemPromptUpdate: in-history`); tool changes as `tool_addition`/`tool_removal` blocks under the beta
  `mid-conversation-tool-changes-2026-07-01`; clock readings at most every 10 minutes. The gateway accepted the
  beta header and blocks without error in every run. The probes (7.3) show why it matters. Feeds **#13**, **#34**.
- **Cost accounting that includes cache** (usage mapped to cache read/write): feeds **#56**.
- **Instructions with provenance**: AGENTS.md/CLAUDE.md chains inside `<system-reminder>` with a byte budget, and
  newly relevant nested files appended after successful fs operations ("Additional instructions from: <path>").
  Fontaine already loads project instructions; the incremental "discovered after a file operation" is the new part.
  Not filed (no measurement here).
- **Escalation as a one-shot retry with a justification** (4.4): already close to Fontaine's denial/retry UI.
- **ACP and an SDK profile** exist as profiles of the same tree: feeds **#59**.
- **Out of scope / not adopted**: default session upload (5), the sandbox attribution heuristic (4.4),
  prompt-only plan mode (4.6), the 24-tool surface (workflow, ralph, computer_use).

## 7. DeepSeek model support

Each entry: what, evidence, affected Fontaine path and issue, and **Correctness** (C) or **Optimisation** (O).
All probes: `scripts/probe_chat.py`, results `captures/probe_results.json` (flash) and
`captures/probe_results_deepseek-v4-pro.json`; both models behaved the same unless noted.

1. **Reasoning passback (C, unverified on the native API).** DeepSeek says tool-using requests must return
   `reasoning_content` or get a 400. *Measured through the Go gateway:* tool-call turn with the field (P1), absent
   (P2), empty string (P3), absent with no thinking parameter (P4), a non-tool assistant turn absent with tools
   sent (P5), without tools (P6), field present with thinking disabled (P7): **all 200**. Real sessions: Fontaine
   round-trips all 12 reasoning turns (4.2); a session built on `go/glm-5.1` and resumed on
   `go/deepseek-v4.1-flash` replayed three tool/assistant turns with no `reasoning_content` and got 200
   (`captures/e8_resume_foreign_model.txt`); the same resume on the Messages wire (assistant blocks `text`,
   `tool_use` only) also 200 (`captures/e8_resume_messages_wire.txt`). dsh's own natural tool-call turns without
   reasoning (e8-fix requests 12 and 14) were accepted. So **no bug is filed**: the gateway tolerates
   or fills it. Path: `opencode_go.rs`, rig `chat.rs reasoning_field`. What remains is the direct API: **#164**.
2. **Reasoning effort (O).** Chat `reasoning_effort` (+ `thinking`), Messages `output_config.effort`; dsh sends
   `high` by default and offers `off|low|high|max` (`packages/llm/llm-deepseek/src/serialize.ts`). The gateway
   accepted `low`, `medium`, `high`, `max`, `xhigh` and `none` on both wires (and `high` without a `thinking`
   field), i.e. it does not validate, and a one-line question gave no usable difference in reasoning tokens
   (16-51). Without a thinking parameter the model thinks (P8). Fontaine sends nothing on the main agent. Feeds
   **#53**: expose `off|low|high|max`, and do not rely on the gateway to reject a wrong value.
3. **KV-cache prefix stability (O, large).** Measured with a 5.4k-token prompt (C1-C5): same prompt again 5,376
   cached of 5,439; same prefix with the last question changed 5,376 cached; **a system message prepended 0 cached**;
   a system message appended after the history 5,376 cached. dsh's own runs: cache_read 5,376 of 5,612 on the
   second request of a session and 13,312 of ~13.9k by step 13 (`captures/e3_fix_usage.txt`). Compaction costs one
   request; changing the tool array costs the prefix (4.3). Fontaine rule: never edit anything before the
   latest message; append runtime changes. Feeds **#13**, **#34**.
4. **Cache accounting (O).** Chat: `usage.prompt_tokens_details.cached_tokens` (normalised by the gateway; the
   native `prompt_cache_hit_tokens` was never seen); Messages: `cache_read_input_tokens`; reasoning in
   `completion_tokens_details.reasoning_tokens`. The gateway adds `opencode.list_cost`, `allowance` and tokens in
   `usage` (billing-source `go`). Feeds **#56**; the native field names need #164.
5. **1,048,576-token window and compaction sizing (O).** dsh copies it into the catalog; its trigger is 80 % of W
   minus reserved output, so with `max_tokens 256000` compaction is not reached in normal use. The model-id is
   what keys per-model behaviour (`serialize.ts:64`). For Fontaine, `[context] autocompact_window` (#152, merged)
   applies to catalog-listed models only; a smaller window than the model's maximum is a cost/quality choice, not a
   requirement.
6. **Sampling parameters in thinking mode (O/C-lite).** `temperature=0.2` (P10) and `top_p=0.1` with
   `presence_penalty=1` (P11) were accepted with thinking on, and no error. Whether they are ignored cannot be
   told from one sample; Fontaine sets none on the main agent. No action.
7. **Images through the Files API (O).** dsh uploads images and references them by id, sized to a 14 px grid at
   most 1,024 tokens each (`llm-deepseek/README.md`); falls back to inline base64. Not reachable through the
   gateway here (Not tried). Feeds **#21**.
8. **Malformed historical tool arguments replayed as `{}` (C-lite).** `serialize.ts:20-28` parses stored
   arguments and falls back to `{}`. Never provoked live. Fontaine's analogue is unverified (#164).
9. **Thinking disabled for screens (C, already done).** The guardian already sends `thinking disabled`; both models
   honoured it (P7, P9), and the 10 guardian calls in E8 were accepted. `deepseek-v4-pro` also honoured it.
10. **Gateway quirks worth encoding (C for dsh-style clients).** A request without `x-opencode-session` is rejected
    with HTTP 400 on the search path (dsh `web_search`); Fontaine sends the header on its own requests. A
    non-browser `User-Agent` is mandatory at the edge (urllib's default gets Cloudflare error 1010). The Messages
    path accepts `x-api-key` only (Fontaine's decoration does that).

## 8. Experiment log

All runs: model `deepseek-v4.1-flash` via the Go gateway; `go` access token remaining at start ~690 h
(28.8 days, `expires_at`), no refresh needed and the proxy never wrote the credential file.
Single runs: no variance estimate. Anything not shown is in `runs/` (gitignored).

- **Open question (Messages on Go).** `fontaine -p --settings proxy_messages.toml` with `OK` returned `OK`; request
  `POST /anthropic/v1/messages`, `x-api-key` accepted, usage includes `opencode.billing_source: go`. dsh's native
  adapter then ran unmodified (no beta rejection). `llm-pi-ai` fallback not needed.
- **E1.** Build OK (334 s). `--dump-config`: 99 rows; patch landed. One bash call (`runs/e1-bash`): exit 0, `uname`/`id` output, a write inside the workspace succeeded. Egress:
  `captures/e1_endpoints.txt` (above). Sandbox runner: Landlock inferred (no bwrap in the image, denial
  attributed under `workspace-write`); `enforcement` is not shown to the model or in the log, so
  "full"/"partial" was **not observed**. Token lifetime: see the heading of this section.
- **E2.** Read-only question twice. Default: 3 requests, `dsh_session_log` + `dsh_plugin_packages` on every one.
  `enabled: false`: 4 requests, only `dsh_plugin_packages`. Sizes and cache in
  `captures/e2_session_log_on_off.txt`. Reasoning passback and effort field observed (`thinking:{type:enabled}`,
  `output_config.effort:high`).
- **E3.** Fix run: passed `npm test` 5/5 in 13 steps. Stall/promote/repeat/goal: stall fired
  (`tool call timed out after 5000ms`), promote fired, repeat did **not** fire on the first trigger (loop inside
  one shell call) and fired on the second, goal: created and blocked in one turn (first trigger), and with the
  no-work trigger the headless run exited 1 with a round aborted by disposal. `web_search` failed with a gateway
  400 (missing `x-opencode-session`). Failures to report: my fixture's test script was itself broken on Node 24
  (`node --test test/`), which turned the "easy" task into the iteration demo; the first goal trigger asked the
  model to stay within `src/` while the real fault was `package.json`, so it blocked itself at once.
- **E4.** Five commands under `ask` and `never`, plus a forced escalation. Results in 4.4. `rejected` text:
  not observed; `unavailable` text: observed. Nothing outside the container was touched.
- **E5.** Two runs (4.3). First threshold too high: no compaction. Second: one compaction.
- **E6.** Todo and subagent runs OK (`captures/trace_e6-*.txt`). The Web UI **booted** (`WEB=1`, bound to the
  container IP, published on 127.0.0.1:3080 through `fwd`) and the launch-token exchange answered HTTP 303 on the
  tokenized URL and 401 without it. Plan mode and Web UI goal rounds were **not driven**: they need a browser session.
- **E7.** Resume with `--session-id` worked; `session_search` found nothing prior, `session_event_search` found the
  earlier events; the changed tool array cost cache (4.3). The on-disk file is `session.v4.jsonl.zstd`.
- **E8.** Fontaine on Chat: the same read-only task and the fix task ran. Reasoning round-trip: 12/12
  (`captures/e8_roundtrip.txt`); guardian exchanges separated and counted (10 of 10 parseable,
  `captures/e8_guardian_calls.txt`); framing comparison (`captures/e8_framing_compare.txt`): Fontaine 3.4k
  input tokens at 11 tools, no `dsh_*` fields, `stream_options.include_usage`; dsh 5.1k at 24 tools. Resume of a
  foreign-model session: 200 on both wires. Hand-built probes: section 7.
- **E9.** `claude -p` (haiku, `--allowedTools`), two rows: repeat (no nudge) and 10 s timeout (moved to a
  background task). The baseline process was left to finish on its own; the 900 s `slow-test.sh` it backgrounded
  was not killed by me (the kill was blocked) and ends by itself.
- **Tooling failures and fixes** (the usual spike cost): tsx resolution (above); the recording proxy initially mislabelled a
  client hang-up after a complete stream as HTTP 502 (fixed to keep the real status); the first `PATCHES`
  handling resolved overlays against the workspace instead of the run dir; `pkill -f` matched my own shell twice.

## 9. Not tried, and why

- **A direct `api.deepseek.com` key.** Needed for the real 400 behaviour, native cache fields, the Files API and
  malformed-arguments replay: #164 (help wanted).
- **Plan mode and goal rounds in the Web UI**, and the Web UI vs headless parity check: need a browser or WebSocket
  client; the server was only proven to boot and authenticate.
- **dsh telemetry export** (needs a `/feedback` in the Web UI); the OTLP endpoint was never contacted without it.
- **Sandbox variants**: bwrap (not in the image), macOS Seatbelt and the Windows ACL runner (no hardware);
  Landlock's `enforcement: partial` downgrade on an older ABI (host kernel is current).
- **Fontaine in the same container as dsh**, to remove the missing-node confound (the host build is glibc-linked
  to a newer libc than `node:24-bookworm`).
- **Other dsh features**: the `workflow` and `ralph` tools, PTC mode, MCP, skills, LSP servers (none ship), the
  ACP and SDK profiles, `llm-pi-ai`.
- **Variance and scale**: every experiment is one run; no pass-rate, cost or latency comparison (not a goal).
- **E9 rows beyond two**: Claude Code's edit-staleness and compaction wording were not baselined.
- **The `rejected` approval text** and the three-round `blocked` gate inside a goal round: source only.
- **Mechanism of the empty `env | grep -i key`** inside dsh's bash tool.

## 10. Follow-ups

New issues: **#160** repeat reminder, **#161** spill and prune, **#162** session search, **#163** DeepSeek guardian
scorecard, **#164** direct-API checks. Comments on existing issues: #17, #25, #22, #24, #39, #13, #53, #56, #21, #10,
#27, #59, #97. One Decisions line was added to the master plan (the repeat reminder departs from Claude Code).
No bug was filed: the E8 hypothesis (a 400 for an empty-reasoning tool-call turn on the Go route) did not reproduce.
