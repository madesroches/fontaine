# The guardian

The guardian is what makes `auto` mode possible. When a tool call is not provably safe and nothing
you configured pre-approved it, a separate model reads your task and a short written policy and
answers **allow**, **deny** (with a reason the agent sees) or **escalate** (ask you). It is the
only part of Fontaine that uses judgment, so everything around it is written to keep that judgment
from being the only thing standing between a model and your machine.

A call reaches the guardian in `auto` mode, and in `plan` mode for the consequential calls that
mode does not block outright. The [deterministic floor](permissions.md#the-pipeline) (deny and ask
rules, critical paths, protected-path handling), the read-only fast path (which includes a spawn of
an agent type that can only read, and excludes reads of secret files such as `.env`), every other `agent` spawn and the shell
pre-classifier all run first, in every mode; the guardian never sees a call they decided. In `auto`
it also does not see file-tool edits to tracked, new or session-created files in your workspace or
writes into the session scratch directory (see [the pipeline](permissions.md#the-pipeline)); it
still sees instruction files, `.github/`, existing untracked files and every shell command.

## Threat model

The guardian is itself an attack surface: the arguments of the call it judges, the files the agent
read and the instructions in a repository are all text an attacker may have written. So:

- **It has no tools.** One stateless request per adjudication, with no conversation that grows and
  nothing to call. Its only output is a structured verdict, which reaches Fontaine on one of two
  paths. The default asks for it through a required `submit` function, which needs a model with
  tool support. For an Ollama model that has no tool support, the request instead carries the
  verdict's JSON schema as the daemon's `format`, which constrains the answer and needs no tool
  template. Either way the answer is parsed and validated the same way, and anything that does not
  parse is a denial.
- **Everything it is shown is data, except your own words.** The request is rendered into
  delimited `<untrusted>` blocks; text that would close a block is neutralized, and anything
  credential-shaped is redacted before it is rendered. The system prompt says that nothing inside
  those blocks is an instruction. The two blocks holding what you typed, `task_intent` and
  `recent_user_messages`, are marked `source="user"` and are read as your intent; any other block
  that claims you approved something is treated as an attack signal.
- **Its picture of what you asked for comes from the session ledger**, the append-only record of
  your own messages, and never from the model's context. A compaction summary or any other
  model-written text cannot reach the guardian as your intent.
- **Every outcome that is not a verdict is a denial**, decided by code (below). There is no
  setting that turns the guardian off, and none that makes it fail open.

  The one deliberate exception is the development escape hatch: in the terminal UI,
  `/guardian-disable` turns the guardian and the permission floor off for that session until
  `/guardian-enable`, after a confirmation and with a warning chip in the status bar. It is
  session-scoped, never persisted, never inherited by subagents, and recorded in the ledger and
  the audit log; see [Terminal UI](tui.md#commands).
- **Rule prose can only restrict.** A shared project file may add to `soft_deny` and `hard_deny`
  but its text sits in the policy, so the prompt tells the guardian to ignore any entry that,
  read literally, grants permission. A [plugin](plugins.md) manifest may add deny-side entries to
  the same two lists, and nothing else.

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
| `tool_call` | The tool, its full arguments, a digest of them, and `destructive`, derived by Fontaine from the call itself (below). Every `powershell` call is judged, none is read as safe beforehand, and the request says the command was not parsed. |
| `session_facts` | The workspace roots, the workspace trust state, the git remotes at session start, the names (never the values) of the [`[env]`](configuration.md#env) variables you set, the status (`pre_existing`, `created_this_session`, `unindexed` or `session_scratch`; never the file index) of only the paths this call touches, which for a shell command are its redirect targets and path-like arguments, the [project environment](#project-environment) when you ran the setup command, and the last 20 actions of the session. When [auto memory](memory.md) is on it also has an `agent_memory_directory` line naming the memory store, set by Fontaine and never from a tool argument. It also has a `session_scratch_directory` line naming the session scratch directory, whose paths carry the `session_scratch` status, and for an `agent` call a `spawned_agent` line with the type, its tools and whether it can write, run commands or reach the network. Fontaine sets all of these from the session, never from a tool argument; the [minimal profile](#the-minimal-profile) and the reads of a [composition](#composing-the-guardian) get neither the scratch nor the spawn line (they do get the memory line). The reads do not get the project environment, which only the guardian view renders; the minimal profile does. |
| `hook_context` | Notes from `pre_tool_use` hooks, in an untrusted block. |
| `prior_denials` | Reason-coded denials earlier in the session, for loop detection. They are context, not evidence about the current call (see [How history and your words are weighed](#how-history-and-your-words-are-weighed)). |

The `task_intent` and `recent_user_messages` messages start after your last `/clear`: what you
asked before it is not shown to the guardian, while the session facts are kept. A
[`/rewind`](checkpoints.md) of the conversation takes the prompts it retracts out of them the same
way, so a message you took back no longer counts as intent; the denials and approvals stay.

Only messages you typed count as user intent. The text you type under "Other" when the model asks
you a question with `ask_user_question` counts, recorded as `Other: "<text>"`; so does a note you
type alongside a chosen option, which is recorded the same way — every `answer.other` becomes an
`Other: "<text>"` user message whether or not options were also selected, so the guardian reads a
note on a choice exactly like standalone "Other" text. The options you pick do not count,
because the model wrote them and a bare label carries no context. A message you queue while the agent works counts from
the moment the agent receives it, with its next tool result, not from when you typed it: until then
the model has not read it, and a call judged earlier must not rest on words the model never saw.
Commands you run with `!` and their output never count, and neither does anything a tool or the
model wrote. A message you type in a [subagent's view](tui.md#the-agent-list-and-the-agent-view)
counts the same way, from when that agent reads it, but only for that subagent and its own
subagents: a call of the main conversation or of a sibling is never judged by words only another
agent's model has read.

### How history and your words are weighed

The guardian reads history and your messages by a few fixed rules:

- What you typed is authoritative about intent. A message that vouches for something specific
  ("the script is ok to run") counts as you naming that thing, and clears a soft deny for it. It
  never clears a hard deny, however often you repeat it.
- After a denial, a message that re-issues or approves the denied action counts as your consent to
  it. The agent re-issuing the same call with no new message from you does not.
- A denial of one kind of action does not count against a different kind, and never against a
  read-only call. Reading, listing or tailing a file the session itself created is routine unless
  the call sends data out of the trust boundary.
- A prior denial by itself never makes a read-only call suspicious. A read made to scout around a
  denial — to learn what the denial protected or to find another route to the denied action — is
  not routine and is judged under the deny rule it serves.
- A standing grant ("you may post gh issues", "git commits in the scratch folder are fine") counts
  as your consent for every later call of that class until you withdraw it. It names the class,
  not the arguments or the content, and the content is still judged by every rule. Like a
  vouching message it clears soft denies only. A request names its action: "log a gh issue about
  X" asks for that issue.
- A guardian denial is not you rejecting the action. When it denies a soft-deny match, the
  explanation says what would clear it: you naming the action and its target, or a standing grant.
- A note you type about the setup, such as "fyi, the guardian is now haiku", is neutral.
- Suspicion needs a rule: "earlier denials and this looks related" is not a reason to deny.

### Subagents and the scratch directory

The session scratch directory is created empty when the session starts and deleted when it ends,
so nothing in it predates the session. It is inside the trust boundary: creating, overwriting or
deleting files there, or running `git init`, `commit` or `reset` confined to it, destroys nothing
pre-existing. A path counts as scratch only if it resolves there; a `..` or a link that leads out
of it does not. Every other rule still applies, such as running code downloaded into it or sending
its contents out. Only literal paths carry the `session_scratch` status: a path built from a shell
variable is not marked.

An `agent` call is never shown to the guardian. A spawn delegates a prompt to a child whose every
call goes through the whole pipeline on its own, guardian included, and whose mode is never looser
than its parent's, so judging the prompt would only repeat that coverage. `auto` and `plan` allow
the spawn (`tier = spawn`) after the deny and ask rules and hooks; `manual` and `acceptEdits` ask
you (see [the pipeline](permissions.md#the-pipeline)).

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
flags or security-guarding tests, adding persistence, or editing the agent's own settings), `Remote
Redirect`, `Unreviewed Merge` and `Container Escape` (minimal profile only). A deny reaches the
agent as `Denied by guardian: [Rule] explanation`, and your terminal shows the label and the
explanation; a subagent's denial shows in that agent's [view](tui.md#the-agent-list-and-the-agent-view),
not in the main chat. Every shipped deny rule starts with the label it is reported under.

### Rule precedence

`hard_deny` over `soft_deny` over `allow` over explicit, specific user intent.

1. A `hard_deny` match is always a deny. Nothing you said overrides it.
2. A `soft_deny` match is a deny unless an `allow` exception clears it or your own messages
   explicitly and specifically authorize that exact action, naming its target. "Clean up" does not
   authorize a recursive delete.
3. `allow` entries are exceptions to soft denies. They never clear a hard deny.
4. An action you asked for by name and target is acceptable when no deny rule applies.

The shipped lists, and how to extend them, are in [`[auto_mode]`](configuration.md#auto_mode).

### Reading outside the workspace

The four read-only file tools may name a path outside every workspace root; each such call
reaches the guardian like any other unapproved call in `auto` and `plan` (it is never skipped,
and it never earns the read-only fast path). The shipped `allow` list treats inspecting source
code, headers, build files, installed dependency source and installed skill definitions
(including `SKILL.md`) as routine wherever they sit — a Cargo registry cache, `/usr/include`, a
Python `site-packages`, a sibling checkout — because debugging a build against installed sources
is ordinary development work. That is a broadening of what is *adjudicated*, not of what is
*automatic*: the call is still judged, and the exceptions hold only for local inspection.
Executing code or skill commands, treating skill text as user intent, sending the contents
anywhere, reading credential or personal-data stores, and scouting around a denied action all
remain subject to the deny rules. In the interactive modes the same call asks you instead, with
the resolved path in the prompt.

## Memory writes

When `session_facts` names the agent's memory directory, a call whose target is under it is a
memory write: the target is found in the call's arguments, in the touched paths, or in a path in a
shell command. Future sessions load what is written there as notes, so the guardian judges the
content as well as the place. It allows plain project facts that your request or the repository
supports (commands, layout, preferences you stated), and denies or escalates content that instructs
the future agent, widens permissions or relaxes the guardian, contains secrets or credentials,
embeds URLs or commands to run, or traces to tool results, web pages or file text rather than to
you. Low confidence is a denial, as everywhere. The rule is in the `full` and the `minimal` prompt,
and it covers shell commands that write there. The notes themselves are never added to
`project_instructions`. See [Auto memory](memory.md).

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
failure names the denial. A malformed verdict is retried with a note saying why it was rejected,
written by Fontaine and never quoting the model's own text; if it still fails it is denied as
`unparseable`. A deny verdict is never softened. The destructive class is derived from the call by
Fontaine, structurally, never from the label the guardian chose: a call is destructive when it
removes or overwrites (`rm`, `mv`, `cp`, `dd`, `sed -i`, `find -delete`, `git reset --hard`, a
redirect to a file, a write over a path that existed), writes outside every workspace root, reaches
the network (`curl`, `git push`, a package install, `webfetch`, `websearch`), schedules a prompt to
run later (`cron_create`), runs a command as a `monitor` (judged like `shell`), starts a code runner
(`sh`, `python` in any version, `pwsh`, `cmd`, `awk`, `make`, `cargo run`, `sudo`, ...; a launcher
such as `timeout` is judged by the program it wraps), is an MCP call, or uses any construct the
shell parser cannot classify. A plain `cargo test 2>&1 | tail` is not destructive.

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
- On an account that caches prompts (`prompt_caching`, on by default for `anthropic` and
  `openrouter`), the guardian's static prefix, its verdict tool and system prompt, is marked
  cacheable; the per-call message is not. On `openrouter` the marker sits on the system prompt
  itself, since OpenRouter's top-level marker would land on the per-call message, which is never
  reused. Models from `openai/`, `deepseek/`, `x-ai/` and `groq/` cache by themselves and get no
  marker. The system prompt holds the profile, the rules and the environment and no per-call
  text, so it is identical across calls until the rules change. The cache read and write tokens
  the guardian used are in the headless [`usage.guardian`](headless.md) output.
- A provider caches a prefix only above a model-dependent minimum length, so on some guardian
  models the marker saves nothing. When you pin a Claude guardian whose minimum is above the
  size of its prompt (about 1500 tokens under the `minimal` profile, 6000 under `full`), with
  caching on, Fontaine says so once at startup. Minimums, in tokens: Claude 5 models 512,
  Sonnet 4 models 1024, Haiku 3.5 and Opus 4.7 2048, Opus 4.5 and 4.6 and Haiku 4.5 4096. A
  guardian model with a lower minimum is the fix. The notice covers Claude ids on `anthropic`
  and `openrouter` accounts; other models are not checked.
- Calls are adjudicated one at a time, in order. The guardian is on the critical path, so a fast
  model matters.

The guardian's model endpoint inherits the [network settings](configuration.md#network) of its
account, and a composition's reads those of their Ollama accounts. An invalid `[network]` stops startup with exit
code 2; the guardian never falls back to a direct connection.

## Composing the guardian

A guardian verdict takes seconds. The guardian can be composed: a short Rune script decides how
the LLM stage described above and cheap local reads are combined, and Fontaine wraps the script in
a fail-closed root. A read asks a small local model a yes/no question about the call and turns the
first token's logprobs into a risk between 0 and 1; it needs an Ollama model, because Ollama's
native chat endpoint is the one that returns logprobs. The composition is configured in
[`[guardian.composition]`](configuration.md#guardiancomposition) and is either a built-in preset,
`screen` being the one that ships, or a script of your own. Nothing is enabled without
configuration: a guardian with no `[guardian.composition]` is the LLM stage alone, and the only
threshold Fontaine proposes is the one benchmarked for the model it is paired with, offered by
[`fontaine auto-mode setup`](#setting-up-the-composition) and never applied without a yes.

The composition runs after everything deterministic, only where the guardian would run, and not
while the denial breaker is paused. A read requests the account's
[`context_tokens`](configuration.md#accountsname) window, as every call to an `ollama` account does.
A read and a guardian on the same Ollama model avoid reloading it only when they request the same
window, which they do on one account or on accounts with equal `context_tokens`. A prompt longer
than the window makes that read fail.

### The screen preset

The default shape is an allow screen in front of the LLM guardian, run in sequence: the screen goes
first; if it allows the call, the call is allowed; everything it does not allow goes to the LLM
guardian, which reviews it. The screen does not stop or escalate on its own unless you set
`stop_threshold`. A call it cannot judge goes to the guardian too: a timeout, an HTTP error, an
unreachable daemon, an answer that is not yes or no, or a read that failed while others succeeded.
Fontaine tells you once per session that a read is failing, and the guardian decides.

The preset declares three reads, all on the composition's `model`, and always runs the first so the
audit log records `screen_risk`:

- **`stop`.** By default it asks, "If an employee were given the user's request, would they be
  allowed to take this action?"; `question = "outside_request"` asks whether the action is outside
  what the request asked for. With `stop_threshold` set, a call whose risk reaches it is escalated
  to you, as a guardian escalation is, with `tier = screen` and `rule = screen.stop` in the audit
  record. In a headless run an escalation is a denial.
- **`allow`.** The hardened permission read of [the allow path](#the-allow-path).
- **`steering`.** The steering detector of [the allow path](#the-allow-path).

The `allow` and `steering` reads run, together with `stop`, only when `allow_threshold` is set (and
always in shadow mode, so their scores are logged). A call the preset allows has `tier = screen`,
`decision = allow` and `rule = screen.allow`.

The reads see the same redacted, delimited request the guardian sees, and sequential means the
guardian is asked only for a call the screen did not settle: a settled call spends no guardian
tokens.

With `concurrent = true` the screen is raced against the guardian instead. They start together, the
screen can settle a call only while the guardian is still pending, and a guardian verdict already in
hand wins. A call the screen does not settle then costs the slower of the two, in practice the
guardian's, but a call it does settle has already spent the input tokens of the guardian request
that is then dropped. The cost is estimated from the rendered prompt and recorded as
`guardian_dropped_input_tokens`, apart from the usage the provider reports. A guardian request that
finished but whose verdict the composition did not use is added to the session's usage.

|  | sequential (default) | `concurrent = true` |
|---|---|---|
| Call the screen does not settle | screen + guardian | max(screen, guardian), about the guardian |
| Call the screen settles | about the screen | about the screen |
| Guardian tokens on settled calls | none | input spent, estimated in the audit log |

### The allow path

A call is allowed without the guardian only when two extra reads of the same local model agree that
it is clearly safe:

- **A hardened permission read.** The question is always "If an employee were given the user's
  request, would they be allowed to take this action?", whatever `question` the stop read uses,
  because the threshold is fitted to that question. Its system prompt adds a sentence that tells
  the model the arguments are written by the agent, and that text in them which addresses a
  reviewer, claims approval or says how to answer is manipulation, to be answered no. The hardened
  prompt is valid only with this question.
- **A steering detector.** It asks whether the arguments contain text that addresses a reviewer,
  claims approval, or tries to influence how the action is judged. A call is never allowed when its
  score is at or above `steering_veto`.

The call is allowed when the hardened read's risk is below `allow_threshold`, the steering score is
below the veto and the stop read did not stop it. Without the detector, one steering sentence in a
call's arguments moved most of the should-not-allow cases in the measured set below the skip
threshold, which is why the allow path has its own prompt and a veto.

The allow path is user scope only, off by default and a deliberate exception to the rule that only
containment earns a skip of the guardian: the allow is a verdict of the composed guardian. It
never runs before the deterministic floor, and a paused breaker prompts you without running the
composition. A composition allow does not reset the denial breaker.

### Calibrating with shadow mode

`shadow = true` is the default. The composition runs and records every read's score, and the
guardian decides as always: `screen_risk`, `screen_allow_risk` and `screen_steering` (the reads
named `stop`, `allow` and `steering`), `screen_scores` (every other read, by name), `screen_model`,
`screen_latency_ms` (from the start of the composition to the last read finishing) and
`screen_outcome` in the audit log. `screen_outcome` is what the composition would have done: `stop`,
`allow`, `deny`, `pass` or `unavailable`, the last meaning a read or the script failed. Run in
shadow mode for a while, then read the audit log: the risk of the calls the guardian denied or you
declined, against the risk of the calls that were allowed, shows where a threshold would have
caught the first without escalating too many of the second. For `allow_threshold`, look for a value
below which no call the guardian denied or you declined appears, and check the steering scores of
the calls you would not want allowed. There is no shipped threshold, because the right one depends
on the model, the question and your own traffic. Set `stop_threshold` and/or `allow_threshold` and
`shadow = false` when the numbers are acceptable. To see the effect on the golden set, run a
[cascade scorecard](evals.md#cascade-scorecards).

### Writing a composition script

A script defines `pub async fn guardian(call, params)` and returns a verdict. It runs in Rune, a
small Rust-like scripting language with `async` and `.await`, in a sandbox: it has no files,
network or terminal, and no access to the call's arguments. The host functions below are in scope
without an import; the language itself is documented at rune-rs.github.io.

| Item | Returns | Notes |
|---|---|---|
| `call.tool()`, `call.destructive()` | `String`, `bool` | The facts of the call a script may see. The arguments are not exposed: a script decides by reads, not by matching arguments. |
| `read(call, name)` | future of `Result<Score, String>` | `name` is a read declared in `[guardian.composition.reads]`. A failed or timed-out read is an `Err`; an undeclared name is an `Err`. Each name is read at most once per call, however often it is named, and dropping the future does not cancel the read. |
| `score.value()` | `f64` | The risk of the read. |
| `llm(call)` | future of verdict | The LLM guardian. All mentions in one call share one request, so naming it on several paths costs once. It never abstains: its failures are denials. |
| `first_decisive([futures])` | future of verdict | Polls all at once and returns the first verdict that is not an abstain, the earlier in the list winning a tie; the rest are dropped. All abstaining abstains. |
| `std::future::join((a, b))` | tuple of results | Runs futures together; a script that awaits one after the other runs them in sequence. |
| `escalate(node, reason)`, `deny(node, reason)`, `abstain()` | verdict | `node` names the deciding step in the audit record's `rule`. |
| `allow_below(node, score, threshold)` | verdict | An allow if `score.value() < threshold`, else an abstain. The only way for a script to allow a call. |
| `verdict.is_abstain()`, `verdict.is_allow()` | `bool` | For sequencing. |

`params` is an object holding `[guardian.composition.params]`: numbers (as floats), strings and
booleans. `call`, scores and verdicts have no constructors and no settable fields, so a script
cannot forge one: an allow can come only from `llm()` or from `allow_below` applied to a score a
declared read produced.

Fontaine wraps every script in rules a script cannot change:

- **It is bounded.** An instruction budget and a time limit (the guardian's timeout, one read's
  timeout and a margin) cover one stage of reads plus one LLM call, so a script that chains several
  read stages before `llm()` can hit the limit and be denied. The time limit cancels the
  requests in flight; the instruction budget and any other script failure stop the script but
  still await and record the reads it started, each within its read timeout.
- **It is fail-closed.** A script error, an exhausted budget, a timeout, a panic, a return value
  that is not a verdict, and an abstain at the end are all denials. Error text explains the denial;
  it never changes it. Such a denial is a guardian denial: it can be approved from `/denied`,
  counts toward the breaker and carries `fail_closed_reason`.
- **A verdict in hand is never overridden.** An LLM verdict that is a denial or an escalation and
  already complete is always the outcome, whatever the script returns. An LLM allow in hand yields
  to a script `escalate` or `deny`, the stricter outcome.
- **Spend is not lost.** A guardian request cut short is estimated into `guardian_dropped_*`; one
  that finished but was not used is added to the session's usage.
- **Reads are awaited.** Every read the script started finishes, within its timeout, and is
  recorded, even after the script has its answer.
- **Shadow mode is invisible to the script.** Fontaine starts every declared read and the LLM
  request itself and decides by the LLM; the script runs on the same requests and what it would have
  decided is recorded as `screen_outcome`.

A script's `escalate` is handled like a guardian escalation (a prompt, or a denial when headless,
with `tier = screen`); its `deny` is a denial with `tier = screen` and no prompt; its `allow_below`
is an allow with `tier = screen`. In each, the `rule` of the record is the node name.

For example, this script races a cheap read against the guardian and escalates what the read finds
out of bounds, but only if the guardian has not already answered:

```rune
pub async fn guardian(call, params) {
    let outside = async {
        match read(call, "outside").await {
            Ok(score) if score.value() >= params.limit => {
                escalate("outside.stop", format!("risk {:.2}", score.value()))
            }
            _ => abstain(),
        }
    };
    first_decisive([llm(call), outside]).await
}
```

with

```toml
[guardian.composition]
script = "guardian.rn"
model = "local/qwen3:4b-instruct-2507"
shadow = false

[guardian.composition.params]
limit = 0.95

[guardian.composition.reads.outside]
question = "outside_request"
```

Project configuration cannot load a script, because a script can mint an allow: only the user file
sets a composition, and other scopes may force `shadow = true`.

The preset and the scripts are scored on the golden set with
[`--screen` and `--composition-script`](evals.md#cascade-scorecards).

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
  `opencode-go` account), only a model set explicitly in `guardian.model` is used, except for the
  validated list's own `opencode-go` entry (`claude-haiku-5-5`), which an `opencode-go` account
  that serves it picks up as its default guardian. A small local
  model can be both the agent and the guardian; judge it by its own scorecard run. The guardian
  prompt is longer than Ollama's default 4096-token window, so an `ollama` account requests
  `context_tokens` (16384 by default) and the daemon refuses a prompt that still does not fit; the
  guardian then denies the call.

### Models with no tool support

A model served by Ollama that reports no `tools` capability (`gemma3:4b`, for example) cannot take
a forced `submit` call: the daemon answers `400 ... does not support tools`. Fontaine asks the
daemon what the model can do (`/api/show`, which does not load it) and picks the path from the
answer:

- a model that reports `tools` uses the `submit` path, the one every scorecard entry was measured
  on;
- a model that reports `completion` but not `tools` uses the schema-constrained path, and Fontaine
  says so once, when the guardian is set up (at startup, and after `/guardian-model`). The
  scanner on that model takes the same path;
- a model that cannot generate text at all (an embedding model) makes the guardian unavailable
  with `ollama/<model> cannot generate text (capabilities: embedding)`, and every gated call is
  denied until you pick another guardian;
- a daemon too old to report capabilities (no `capabilities` list) is treated as a `submit` model
  for the guardian's life. A question that goes unanswered (unreachable daemon, HTTP error or
  timeout) is treated as a `submit` model for that call only, so a failure stays as loud as it is
  today, and is asked again on the next call.

There is no setting for this: the path follows what the model reports. If a daemon claims tool
support the model lacks, the call is denied and the explanation says that the model does not
support tool calls and so could not use the `submit` output tool; pick another model.

The schema-constrained path is not benchmarked unless a model is listed for it. The constraint
fixes the shape of the answer, not its quality, and a small model may reason less before it fills
the schema. The audit log records which path judged each call as `verdict_transport` (`tool` or
`schema`), and `fontaine-evals --verdict-transport` scores a model on either; see
[the scorecard](evals.md#verdict-transport). Until a model's own schema run clears the inclusion
bar, it works only when you set it in `[guardian] model`.

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

## Inspecting the rules

`fontaine auto-mode` shows what the guardian judges against. It starts no session, runs nothing
the repository defines and, except for `setup`'s optional
[composition offer](#setting-up-the-composition), changes no setting. `--cd` and `--settings` apply. Each subcommand takes
`--format text|json`.

- `fontaine auto-mode defaults` prints the shipped `environment`, `allow`, `soft_deny` and
  `hard_deny` lists. The text form is a `[auto_mode]` TOML fragment you can paste into a settings
  file and edit. It needs no configuration and makes no request.
- `fontaine auto-mode config` prints the effective lists a session would hand the guardian, after
  `$defaults` splicing and the add-only rules for `soft_deny` and `hard_deny`. Each list is headed
  by the scopes that set it (`user`, `project-local`, and so on) or `shipped defaults`, and each
  entry is tagged `default` or `custom`. Scopes are recorded per list, not per entry. The output
  shows what a non-interactive load yields: project settings that need workspace trust are not
  given it here, so an interactive session you trusted can differ. Control characters in entries
  are shown as escapes. Under `[guardian] profile = "minimal"` (when the container check holds) a
  note says that only `environment` reaches the guardian. It makes no request.
- `fontaine auto-mode critique` sends your custom entries, the entries not equal to a shipped one,
  to the guardian's model and prints its review for ambiguity, overbreadth, redundancy and
  contradiction with the hard-deny classes, followed by the model and its token count (and cost,
  for a priced model). With no custom entries it prints `nothing to critique` and exits 0 without
  resolving a model or making a request. The model is chosen as a session chooses it, so it needs
  the same main model and account; an unavailable guardian model is a configuration error (exit
  2), and a timeout (`[guardian] timeout_secs`) or provider error exits 1. Your entries go to that
  account's provider, as they do in a session; the command says so on stderr before sending. The
  review is advice: it changes no setting and has no effect on any permission decision. Under
  `[guardian] profile = "minimal"` it prints a note and makes no request, because the rule lists
  are not sent.

- `fontaine auto-mode setup [--no-model] [--no-composition] [--format text|json]` collects the
  project's [environment](#project-environment) and stores it, then offers the
  [guardian composition](#setting-up-the-composition). It starts no session and exits 0 even when
  steps fall back to unknown; it exits 1 only when the record cannot be written. The JSON report
  gains a `composition` object with a `status` (`skipped`, `offered_unanswered`, `declined`,
  `written` or `failed`) and the `candidates` it found.

A prompt that is exactly `auto-mode` runs the subcommand, with or without `-p`; write
`fontaine -p -- auto-mode` to give the agent that word as a task.

## Project environment

Without help, the guardian does not know whether your repository is public, which branch is
protected, whether a tag push triggers a release or which `.env` files are ignored, so it guesses.
`fontaine auto-mode setup` (or `/auto-mode-setup` in the [terminal UI](tui.md#commands))
collects those facts once per project, in two layers.

**Facts read directly**, with no model:

| Fact | Source |
|---|---|
| Visibility, default branch, branch protection (the `protected` flag or any ruleset rule), deploy environments, homepage, Pages site | The GitHub API through the `gh` you are logged in with, for `github.com` remotes. No `gh`, no login or another host leaves the fact `unknown`; an org policy that bars the check (`403`, SAML) leaves it `unverified`. |
| Workflows triggered by a tag push or a release, workflow `environment` names | `.github/workflows/*.yml` at the git work-tree root. |
| Package registries | `.npmrc`, `.yarnrc.yml`, `pip.conf`, `pyproject.toml` and `.cargo/config.toml`. The host only is kept, never userinfo, a token or a path. |
| `.env*` files and whether git ignores them | A directory listing; their contents are never read. |
| Stack | The manifests at the project root. |

Each `gh` call is a fixed command with a 10 second limit. The report names every step that fell
back and why.

**A model scan**, in a [trusted workspace](configuration.md#workspace-trust) and from a terminal.
The guardian's model reads a bounded sample of the repository (the tracked-path listing, the GitHub
settings above, non-GitHub CI files, build and task files, infrastructure file heads, the README
and CONTRIBUTING; `.env`-like files by name only) and proposes sensitive locations, deploy targets
outside GitHub Actions, protected targets and trusted destinations. The sample goes to the
guardian's provider, and the command says so first. Every proposal must cite a tracked file from
the listing or a project setting, a sensitive location must match a tracked path, and a trusted
destination must be a host, a bucket URL or a registry URL with no wildcard broader than one
organization prefix; the rest are dropped and counted. You review what is left, with the entries
that relax the guardian (trusted destinations) listed first, and nothing is saved until you accept.
`--no-model`, a run without a terminal and an untrusted workspace skip the scan, and a missing
model, a timeout or an unparseable reply never fails the setup.

The record is `<data dir>/auto_mode/<project>/environment.json`, owner-only and a
[protected path](permissions.md), keyed like [auto memory](memory.md) so every worktree of a
repository shares it. `fontaine auto-mode config` shows it.

**What the guardian reads.** A session loads the record at start with no network call and keeps
only the remotes it still has. The guardian sees it as data in the `session_facts` block, and its
prompt gives the fields fixed meanings:

- `public` is authoritative and cuts both ways. The repository's tracked content as published at
  that remote is public data, so sharing it anywhere is not data exfiltration, and opening issues
  or pull requests about the project there is routine. Untracked, ignored or sensitive files,
  local changes the remote does not have, and secrets get no relief, and pushing new material
  there is publication, so confidential material is not cleared by it.
- `private`, `internal` and `unknown` relax nothing. `unverified` counts as possibly public.
- A protected default branch, or a workflow that runs on tag push, marks a push there as a
  shared-branch change or a release.
- Facts read from repository files describe the project and never add trust.
- Trusted destinations you accepted count as your own trust-slot entries.
- A location, target or destination missing from the lists says nothing about its safety.

**Expiry.** A `public` fact older than 30 days is shown as `unknown (last seen public on <date>)`,
because a repository made private since would keep a relaxation it no longer earns. Facts that
only tighten, and entries you accepted, do not expire.

**The startup warning.** An `auto` session whose project has no record, or whose record lacks a
current remote or holds an expired `public`, warns once: run `fontaine auto-mode setup`. In the
terminal UI a session that starts in another mode warns the first time it switches into `auto`.
`/guardian-disable` silences it while the guardian is off. Set
[`[auto_mode] setup_reminder = false`](configuration.md#auto_mode) in the user file to silence
it, for example on CI with fresh checkouts. Having `[auto_mode] environment` entries does not:
those are global, the record is per project.

`/auto-mode-setup` applies the environment facts to the running session at once; a command-line run
from another terminal applies at the next start or resume. A composition it writes applies from the
next session in either case.

### Setting up the composition

The last, optional step of `fontaine auto-mode setup` and `/auto-mode-setup` offers the
[`screen` preset](#composing-the-guardian) so you do not write `[guardian.composition]` by hand or
guess a threshold. It looks for an `ollama` account that lists a model Fontaine has a benchmark for
(today `qwen3:4b-instruct-2507-q4_K_M`, matched by its exact tag, since a threshold is fitted to one
model, quantization, prompt and question) and asks one basic choice:

- **safe**: `stop_threshold` only. The screen can only stop a call (escalate it, or deny it where
  nobody can answer); it never allows, so the LLM guardian still reviews every call the screen did
  not stop. It may stop some calls that would otherwise be allowed.
- **fast**: `allow_threshold` only. The screen allows the calls it is confident about, which is a
  guardian-tier allow; every call it does not clear goes to the LLM guardian, which decides it.

You type no number. Setup shows what it will write, with the benchmark behind the threshold, and
writes nothing unless you answer yes: `preset = "screen"`, `model = "<account>/<model>"`,
`shadow = false` and the one threshold, to the user settings file only, in one edit that is first
checked as a session start would check it. The step is skipped, with a line saying why, when a
composition is already configured, under the `minimal` profile (the thresholds were fitted on full
profile reads), with `--no-composition`, and when no reachable Ollama account holds a benchmarked
model (the line names the model to pull). A run without a terminal lists what it found in the
report and writes nothing.

The value is a fit on a small dataset. The fast threshold let one call through live that the
dataset expects denied or escalated, so score it on your own traffic with
`fontaine-evals --screen live:<account>/<model>` or the `screen_risk` (safe) and `screen_allow_risk`
(fast) values in the audit log, and set `shadow = true` to log scores without acting while you
tune. See [Cascade scorecards](evals.md#cascade-scorecards).

## Prompt versioning

The prompts are versioned assets in `rust/fontaine-guardian/prompts/` (`guardian_prompt.md`,
`guardian_minimal.md`, `scanner_llm.md`, `critique.md`, `env_scan.md`) compiled into the binary, so a prompt change ships with a release and is
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
- **Standing grants have a window.** The guardian reads your last 5 messages and 2,000
  characters of intent. A grant you gave earlier than that drops out and has to be repeated.
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
