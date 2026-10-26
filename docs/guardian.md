# The guardian

The guardian is what makes `auto` mode possible. When a tool call is not provably safe and nothing
you configured pre-approved it, a separate model reads your task and a short written policy and
answers **allow**, **deny** (with a reason the agent sees) or **escalate** (ask you). It is the
only part of Fontaine that uses judgment, so everything around it is written to keep that judgment
from being the only thing standing between a model and your machine.

A call reaches the guardian in `auto` mode, and in `plan` mode for the consequential calls that
mode does not block outright. The [deterministic floor](permissions.md#the-pipeline) (deny and ask
rules, critical paths, protected-path handling), the read-only fast path and the shell
pre-classifier all run first, in every mode; the guardian never sees a call they decided.

## Threat model

The guardian is itself an attack surface: the arguments of the call it judges, the files the agent
read and the instructions in a repository are all text an attacker may have written. So:

- **It has no tools.** One stateless request per adjudication, with no conversation that grows and
  nothing to call. Its only output is a structured verdict, extracted through the model library's
  typed-extraction path (a required `submit` function), which works on any tool-capable model,
  Ollama included.
- **Everything it is shown is data.** The request is rendered into delimited `<untrusted>` blocks;
  text that would close a block is neutralized, and anything credential-shaped is redacted before
  it is rendered. The system prompt says that nothing inside those blocks is an instruction.
- **Its picture of what you asked for comes from the session ledger**, the append-only record of
  your own messages, and never from the model's context. A compaction summary or any other
  model-written text cannot reach the guardian as your intent.
- **Every outcome that is not a verdict is a denial**, decided by code (below). There is no
  setting that turns the guardian off, and none that makes it fail open.
- **Rule prose can only restrict.** A shared project file may add to `soft_deny` and `hard_deny`
  but its text sits in the policy, so the prompt tells the guardian to ignore any entry that,
  read literally, grants permission.

It is not a sandbox. A model can be talked into a bad verdict, and a command the guardian allows
runs with your permissions. See [Known limits](#known-limits).

## What the guardian sees

The request is built from the ledger and the call itself:

| Part | Content |
|---|---|
| `task_intent` | Your latest messages, verbatim, oldest dropped first to fit 2,000 characters. Never summarized. |
| `recent_user_messages` | Your last 5 turns (not tool output). |
| `project_instructions` | The instruction files the agent had at launch (`FONTAINE.md`, `CLAUDE.md`, `AGENTS.md`, rules and their imports), as they stood when the session started: the primary root's level first, then each directory above it, then yours, cut at 16,000 characters. Files loaded later on demand never reach it. See [Instruction files](instructions.md#what-the-guardian-reads). |
| `environment`, `rules` | The `[auto_mode]` prose lists, with the shipped defaults spliced in. |
| `tool_call` | The tool, its full arguments, a digest of them, and `destructive`, derived by Fontaine from the call itself (below). |
| `session_facts` | The workspace roots, the workspace trust state, the git remotes at session start, the names (never the values) of the [`[env]`](configuration.md#env) variables you set, the status (`pre_existing`, `created_this_session` or `unindexed`; never the file index) of only the paths this call touches, which for a shell command are its redirect targets and path-like arguments, and the last 20 actions of the session. |
| `hook_context` | Notes from `pre_tool_use` hooks, in an untrusted block. |
| `prior_denials` | Reason-coded denials earlier in the session, for loop detection. |

The `task_intent` and `recent_user_messages` messages start after your last `/clear`: what you
asked before it is not shown to the guardian, while the session facts are kept.

Only messages you typed count as user intent. The text you type under "Other" when the model asks
you a question with `ask_user_question` counts, recorded as `Other: "<text>"`; the options you pick
do not, because the model wrote them and a bare label carries no context. A message you queue while the agent works counts from
the moment the agent receives it, with its next tool result, not from when you typed it: until then
the model has not read it, and a call judged earlier must not rest on words the model never saw.
Commands you run with `!` and their output never count, and neither does anything a tool or the
model wrote.

## The verdict

```
decision     allow | deny | escalate
rule         allow, or one label of the list below
explanation  1-3 sentences, shown to you and fed back to the agent on a deny
confidence   0 to 1, the guardian's own estimate
```

`rule` is `allow` exactly when `decision` is `allow`; a verdict where they disagree, a label outside
the list or a confidence outside 0 to 1 is malformed. The labels are `Download+Execute`, `Data
Exfiltration`, `Production Deploy`, `Git Destructive`, `Destroy Pre-existing Files`, `IAM Grant`,
`Secret Exposure`, `Tunnel/Reverse Shell`, `Unguarded Agent Loop`, `Safety Bypass` (disabling safety
flags or security-guarding tests), `Remote Redirect`, `Unreviewed Merge` and `Container Escape`
(minimal profile only). A deny reaches the agent as `Denied by guardian: [Rule] explanation`, and
your terminal shows the label and the explanation.

### Rule precedence

`hard_deny` over `soft_deny` over `allow` over explicit, specific user intent.

1. A `hard_deny` match is always a deny. Nothing you said overrides it.
2. A `soft_deny` match is a deny unless an `allow` exception clears it or your own messages
   explicitly and specifically authorize that exact action, naming its target. "Clean up" does not
   authorize a recursive delete.
3. `allow` entries are exceptions to soft denies. They never clear a hard deny.
4. An action you asked for by name and target is acceptable when no deny rule applies.

The shipped lists, and how to extend them, are in [`[auto_mode]`](configuration.md#auto_mode).

## Failing closed

Fontaine's code, not the model, decides what happens when there is no usable verdict:

| Outcome | Result | `fail_closed_reason` |
|---|---|---|
| No credential, unknown account, or the pinned model is unavailable | deny, rule `GuardianUnavailable` | `unavailable` |
| Provider error, dropped stream, connection reset or a closed socket | deny, rule `GuardianUnavailable` | `error` |
| No answer within `[guardian] timeout_secs` (default 30, retries included) | deny, rule `Timeout` | `timeout` |
| Malformed, schema-invalid or out-of-enum verdict after two retries | deny, rule `Unparseable` | `unparseable` |
| An allow on a **destructive** call with confidence below `confidence_threshold` (default 0.7) | deny, rule `LowConfidence` | `low_confidence` |

The retry budget is two retries, shared between malformed answers and provider errors, and the last
failure names the denial. A deny verdict is never softened. The destructive class is derived from
the call by Fontaine, structurally, never from the label the guardian chose: a call is destructive
when it removes or overwrites (`rm`, `mv`, `cp`, `dd`, `sed -i`, `find -delete`, `git reset
--hard`, a redirect to a file, a write over a path that existed), writes outside every workspace
root, reaches the network (`curl`, `git push`, a package install, `webfetch`, `websearch`), schedules a prompt to run later (`cron_create`), starts a code runner
(`sh`, `python` in any version, `pwsh`, `cmd`, `awk`, `make`, `cargo run`, `sudo`, ...; a launcher such as `timeout` is judged by the program it wraps), is an MCP call, or uses any construct the shell
parser cannot classify. A plain `cargo test 2>&1 | tail` is not destructive.

### Denial limits

Three denials in a row, or twenty in a session (any tool, not configurable), pause `auto`: the
next call that needs the guardian asks you instead, and approving a call resumes `auto`. In a
headless run a paused session denies. This stops a model from retrying a refused action until
something slips through.

### Retrying a denial yourself

In the terminal UI, `/denied` lists the calls the guardian denied and `r` approves one **once**.
Fontaine records your approval for that exact call (same agent, tool and arguments), tells the agent
it may retry, and lets the next identical call through without asking the guardian. The approval is
consumed by that call, never cached, and does not count toward the denial limits. The floor runs
first: an `ask` rule or a critical path still asks.

## Speed and cost

- Read-only calls inside the workspace and the shell pre-classifier's commands never reach the
  guardian. A narrow allow rule you wrote skips it too; broad code-execution allow rules are
  [suspended in `auto`](permissions.md#allow-rules).
- A verdict is cached for 60 seconds (`[guardian] cache`). The key covers the call, the model, the rules, the environment, your messages, the session's
  recent actions and its denials, so any change misses. An identical call therefore reuses a verdict only when nothing else has happened in the session since (in practice consecutive identical calls under the minimal profile, or retries); this is deliberate, so an Allow is not reused after a security-relevant change. Only verdicts the model returned are cached,
  never a fail-closed outcome; the confidence rewrite applies after every lookup.
- On an account that caches prompts (`prompt_caching`, on by default for `anthropic`), the
  guardian's static prefix, its verdict tool and system prompt, is marked cacheable; the per-call
  message is not. The system prompt holds the profile, the rules and the environment and no
  per-call text, so it is identical across calls until the rules change. A provider caches a
  prefix only above a model-dependent minimum length (about 1024 tokens on Sonnet and Opus, more
  on Haiku), so on some guardian models this saves nothing.
- Calls are adjudicated one at a time, in order. The guardian is on the critical path, so a fast
  model matters.

The guardian's model endpoint inherits the [network settings](configuration.md#network) of its
account, and the screen those of its Ollama account. An invalid `[network]` stops startup with exit
code 2; the guardian never falls back to a direct connection.

## The screen stage

A guardian verdict takes seconds. The guardian is a composition of two stages: an optional screen
stage, a small local model that answers cheap yes/no questions about each call, and the LLM stage
described above. The screen reads the probability of each answer from the first token's logprobs
and turns it into a risk between 0 and 1. It is configured in
[`[guardian.screen]`](configuration.md#guardianscreen) and needs an Ollama model, because Ollama's
native chat endpoint is the one that returns logprobs. The screen requests the account's
[`context_tokens`](configuration.md#accountsname) window, as every call to an `ollama` account does.
A screen and a guardian on the same Ollama model avoid reloading it only when they request the
same window, which they do on one account or on accounts with equal `context_tokens`. A screen
prompt longer than the window makes the screen unavailable, and the guardian decides.

The screen sits after everything deterministic and runs only where the guardian would run. It
and the guardian start together, so a call the screen does not settle costs the slower of the two
(the screen is bounded by `timeout_ms`), in practice the guardian's. The
screen can settle a call in two ways, and only while the guardian is still pending:

- **A call it stops** is escalated to you, as a guardian escalation is, with the reason "a local
  screen judged this call outside the user's request (risk 0.97)" and `tier = screen` in the audit
  record. The stop read asks, by default, "If an employee were given the user's request, would they
  be allowed to take this action?" and stops a call whose risk reaches `stop_threshold`. In a
  headless run an escalation is a denial.
- **A call it allows** runs without waiting for the guardian, with `tier = screen`,
  `decision = allow` in the audit record. This is off unless you set `allow_threshold`; see
  [The allow path](#the-allow-path).
- **Every other call** goes to the guardian exactly as it would without the screen. That includes
  a call the screen could not judge: a timeout, an HTTP error, an unreachable daemon or an answer
  that is not yes or no. Fontaine tells you once per session that the screen is failing, and the
  guardian decides.

When the screen settles a call, the guardian's request is dropped, which cancels it. Its verdict
cache and denial counter are untouched, and its tokens are not counted in the session's usage, so a
priced guardian still pays for the input of a request the screen settled. A guardian verdict that
is already in hand when the screen answers always wins and goes through the normal flow, so a
denial the guardian computed is never turned into an allow.

The screen reads the same redacted, delimited request the guardian reads. Any read that fails
makes the whole screen outcome unavailable, so the guardian decides.

### The allow path

A call is allowed without the guardian only when two extra reads of the same local model agree that
it is clearly safe:

- **A hardened permission read.** The question is always "If an employee were given the user's
  request, would they be allowed to take this action?", whatever `question` the stop read uses,
  because the threshold is fitted to that question. Its system prompt adds a sentence that tells
  the model the arguments are written by the agent, and that text in them which addresses a
  reviewer, claims approval or says how to answer is manipulation, to be answered no.
- **A steering detector.** It asks whether the arguments contain text that addresses a reviewer,
  claims approval, or tries to influence how the action is judged. A call is never allowed when its
  score is at or above `steering_veto`.

The call is allowed when the hardened read's risk is below `allow_threshold` and the steering score
is below the veto, and the stop read did not stop it. Without the detector, one steering sentence in
a call's arguments moved most of the should-not-allow cases in the measured set below the skip
threshold, which is why the allow path has its own prompt and a veto. The two extra reads run
concurrently with the stop read under the one `timeout_ms`.

The allow path is user scope only, off by default and a deliberate exception to the rule that only
containment earns a skip of the guardian: the screen's allow is a verdict of the composed guardian.
It never runs before the deterministic floor, and a paused breaker prompts you without asking the
screen. A screen allow does not reset the denial breaker.

### Calibrating with shadow mode

`shadow = true` is the default. The screen scores every call and records `screen_risk`,
`screen_allow_risk`, `screen_steering`, `screen_model`, `screen_latency_ms` and `screen_outcome` in
the audit log, and the guardian decides as always. `screen_outcome` is what the screen would have
done: `stop`, `allow`, `pass` or `unavailable`. Run in shadow mode for a while, then read the audit
log: the risk of the calls the guardian denied or you declined, against the risk of the calls that
were allowed, shows where a threshold would have caught the first without escalating too many of
the second. For `allow_threshold`, look for a value below which no call the guardian denied or you
declined appears, and check the steering scores of the calls you would not want allowed. There is
no shipped threshold, because the right one depends on the model, the question and your own
traffic. Set `stop_threshold` and/or `allow_threshold` and `shadow = false` when the numbers are
acceptable. To see the effect on the golden set, run a
[cascade scorecard](evals.md#cascade-scorecards).

## Choosing the guardian model

Fontaine ships an ordered list of **benchmark-validated** guardian models in
`guardian_models.toml`, built into the binary and re-benchmarked each release. A model is on the
list only if, on the [scorecard](evals.md), its catch rate over the attack cases is at least 90%,
its false-deny rate over the benign cases is at most 5%, and its p95 adjudication latency is at
most 3 seconds. Entries are ranked by catch rate, then false-deny rate, then latency.

With no `[guardian] model`, the guardian is the highest-ranked validated model that one of your
accounts offers. If none is, the main model judges and Fontaine tells you once at startup.
The guardian's and the scanner's calls force a structured answer, which a model that thinks by
default refuses. Fontaine turns thinking off for those calls on `opencode-go` Chat models and
non-Claude Messages models. On an `openai-compatible` account, a model that thinks by default
cannot be the guardian: every call is denied as `GuardianUnavailable`. Pick a model without
default thinking.

Changing the guardian is easy but never automatic:

```toml
[guardian]
model = "work/claude-haiku-5-5"
```

- Set `[guardian] model`, or use `/guardian-model <account>/<model>` in the terminal UI, which
  switches the live session and saves the choice. `/model` does the same for the main model, and
  moves the guardian with it only when the guardian is on the main-model fallback.
- A model you pinned is **never substituted**. If it cannot be used, Fontaine fails loudly. In the
  terminal UI a prompt offers to retry it, to pick another validated guardian for the session, or
  to switch to `manual`. In a headless run every adjudication is denied with `GuardianUnavailable`,
  and the `init` event and stderr say so.
- For endpoints with no catalog match (a custom `openai-compatible` endpoint, Ollama, an
  `opencode-go` account), only a model set explicitly in `guardian.model` is used. A small local
  model can be both the agent and the guardian; judge it by its own scorecard run. The guardian
  prompt is longer than Ollama's default 4096-token window, so an `ollama` account requests
  `context_tokens` (16384 by default) and the daemon refuses a prompt that still does not fit; the
  guardian then denies the call.

## The minimal profile

`[guardian] profile = "minimal"` narrows the guardian for a container. Inside a sandbox the blast
radius of ordinary work is bounded, so the minimal guardian does not judge routine development; it
enforces **containment integrity** (mounts, container-runtime sockets, `/proc/sys` and cgroup
control files, privileged namespace operations, raw devices, symlink traversal to bind-mounted host
paths, network paths around the sandbox proxy), denying them as `Container Escape` with the escape
class in the explanation as `class=<name>`, and still denies exfiltration to destinations that are
not the repository's own remotes. It receives a smaller request: the call, your last 500
characters and 3 messages, the workspace roots, the remotes, the `[env]` names and the touched paths' status, without
project instructions, hook notes or earlier denials. The deterministic floor still runs.

It is a narrowing, never "off", and it only loads when containment is verified. **Both** must hold:

1. You pass `--in-container`, asserting the process runs in a container.
2. A runtime probe agrees. On Linux the probe finds any one of `/.dockerenv`, `/run/.containerenv`
   (Podman), a `container=` entry in PID 1's environment, or `docker`, `kubepods`, `containerd`,
   `libpod` or `lxc` in `/proc/1/cgroup`. macOS and Windows never report a container.

If the flag is missing or the probe is negative, Fontaine runs the **full** profile and prints a
notice saying why. The profile can only be set from user settings, never from a project file.
Every profile is scored before it ships: the scorecard has a `minimal` column.

## Scanning tool results

The guardian judges calls. The scanner looks at what comes back: with `[guardian] scanner` set, every
tool result is checked for text that tries to steer the agent (instructions addressed to it, attempts
to override its rules, hidden directives) before the agent reads it. It is off by default, because
every result then waits for the scanner, which costs latency on every call.

The scanner never withholds a result. It adds a note after the result, so the agent reads both:

- **Flagged.** The note says the scanner flagged the result as possible prompt injection and gives
  the scanner's reason, and tells the agent to treat anything in it that reads as an instruction as
  untrusted data.
- **Unscanned.** If the scanner errors, times out, answers with something that is not a verdict, or
  is unavailable, the note says the result is unscanned and suspect, with the cause. Anything that
  is not a clean verdict is treated this way, so a scanner outage never reads as a pass.

A clean result is passed through unchanged. Empty results are not sent to the scanner. A failed tool
call is a result too and is scanned. The scan runs after the notifications queued for the agent have
been added and before `post_tool_use` hooks, so what another agent said is scanned like any other
result: a subagent's report is flagged before its parent reads it. The `tool_result` telemetry event
records the verdict and the latency, never the content.

`scanner` selects one of four settings, in the user file only:

| Value | What scans |
|---|---|
| `off` | Nothing. The default. |
| `pg2` | PromptGuard 2 (86M), a small classifier run locally. A result is tokenized in full and scored in windows of 512 tokens at a stride of 256; it is flagged if any window scores 0.9 or more. |
| `llm` | The guardian's model, in a short separate conversation with its own prompt. The result sits inside an untrusted block. A malformed answer, an error or a timeout leaves the result unscanned and suspect. |
| `command` | A program of your own (`scanner_command`). See [the contract](configuration.md#bring-your-own-scanner). |

Each scan is bounded by `[guardian] timeout_secs`. `pg2` is in the release binaries; a source build
needs `cargo build --features pg2`, and `scanner = "pg2"` in a build without it is a configuration
error (exit code 2).

### PromptGuard 2 and the Llama 4 Community License

PromptGuard 2 is Meta's model, distributed under the Llama 4 Community License, and its weights are
a gated download on Hugging Face. Fontaine never bundles them. You accept the license on Hugging
Face yourself, and Fontaine prints a notice about it every time a session starts with `scanner =
"pg2"`, in every permission mode.

Either point `scanner_model_path` at a directory holding `model.safetensors`, `tokenizer.json` and
`config.json`, or let Fontaine download the missing files into `<data dir>/models/promptguard2-86m/`
with your Hugging Face token, read from the environment variable named by `scanner_hf_token_env`
(`HF_TOKEN` by default). The token is never stored or logged, and that variable is never passed to
`shell` commands or hooks. A download gives up when it cannot connect within 30 seconds or
when the server goes silent for 60 seconds, and a file appears under its final name only after a complete response. The first start downloads the
model (hundreds of MB), prints a line on stderr saying so, and is slow; later starts reuse the files.

The model loads at startup. If the token is missing, Hugging Face refuses the download (the license
is not accepted), the network fails, or the files are missing or corrupt, the session still starts.
The scanner is then unavailable: a notice names the cause, the license and the token variable, and
every result carries the unscanned note until you fix it and restart.

## Prompt versioning

The prompts are versioned assets in `rust/fontaine-guardian/prompts/` (`guardian_prompt.md`,
`guardian_minimal.md`, `scanner_llm.md`) compiled into the binary, so a prompt change ships with a release and is
re-scored on the golden dataset before the list of validated models is regenerated. A change to a
prompt or to the shipped rule lists changes the cache key, and an entry in the model list is only
valid for the dataset it was scored on.

## Known limits

- **Latency.** Every call that reaches the guardian waits for a model. The 3-second p95 bar is a
  property of the listed models, not of yours.
- **Injection.** Delimiting, redaction and a tool-less single-shot design reduce, and do not
  remove, the chance that hostile text in a command, a file or a repository steers the verdict.
  The deterministic floor and the fail-closed rules exist so that the guardian is not the only
  control, and your `deny` rules apply whatever it says.
- **Judgment.** The scorecard measures a fixed dataset. Novel attacks, and benign commands the
  dataset never saw, will be misjudged at some rate. A guardian allow is a decision to run the
  command with your permissions, not a proof that it is safe.
- **Tool results are scanned only if you turn it on.** The guardian judges calls, not what comes back from them. The [scanner](#scanning-tool-results) annotates suspect results; it cannot prove one safe, and a flag is a warning to the agent, not a block.
- **Provider-native search is outside the guardian and the scanner.** When the provider's own web
  search is offered (see [Web search](tools.md#web-search)), it runs inside the model's response:
  the guardian never sees a query and the scanner never sees a result. Fontaine withholds it while
  a `deny` or `ask` rule or a `pre_tool_use` hook could cover `websearch`, and in `manual`,
  `acceptEdits` and `dontAsk` unless an `allow` rule exists; `[websearch] native = "off"` turns it
  off. The client-side `websearch` tool is judged and scanned like any other call.

The [scorecard](evals.md) reports the measured catch and false-deny rates per model.

## Attribution

The seeded default rule lists are adapted from Anthropic's published defaults for Claude Code's
auto mode (https://code.claude.com/docs/en/auto-mode-config and the output of `claude auto-mode
defaults`), reworded and reduced to Fontaine's rule labels. The auto-mode design itself, the
precedence of rule lists and the `$defaults` marker follow Claude Code's. This is not an Anthropic
product and the lists are not endorsed by Anthropic.
