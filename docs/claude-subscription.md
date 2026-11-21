# Claude subscription

The `claude-subscription` account kind runs your installed, unmodified Claude Code CLI as the
model transport. The CLI owns your Claude sign-in and its subscription billing; Fontaine keeps its
own agent loop, tools, permission pipeline, guardian, sessions, memory and compaction.

## Scope

- Ordinary, individual, local use of your own **Max or Ultra** plan through the official `claude`
  binary. Pro, Team and Enterprise plans are refused.
- Not supported: sharing or pooling credentials, resale, hosted or multi-tenant use,
  `claude setup-token`, using `CLAUDE_CODE_OAUTH_TOKEN` directly, and importing anything from
  `~/.claude`. Fontaine never reads, copies or forwards a Claude credential (the one request
  that reaches Fontaine, the reminder probe described under
  [Supported CLI versions](#supported-cli-versions), goes to a listener on the loopback interface
  that reads only its body and drops its headers), never presents itself as Claude Code, and never selects API-key, gateway, cloud-backend, paid-credit or
  fallback-model routing. The CLI's own fallback is never used; a [model list](configuration.md#falling-through-to-the-next-entry)
  you write moves to its next entry in Fontaine, never inside the CLI.
- Anthropic's terms for the Agent SDK and for signing in to Claude Code apply to you. Check them
  before relying on this account kind.

## Setup

Install the CLI, sign in, and add the account. You can sign in beforehand, or let Fontaine run the
login for you the first time (see below):

```sh
claude auth login --claudeai
```

```toml
[accounts.claude]
kind = "claude-subscription"
# Optional:
#   cli_path = "/absolute/path/to/claude"          # default: `claude` on PATH
#   claude_config_dir = "/absolute/path/to/profile" # the CLI's own config directory
#   models = ["sonnet", "opus", "haiku"]            # narrows what the CLI offers
```

Then pick a model with `--model claude/sonnet`, `/model` or `[model] model`. The ids are the ones
the CLI lists (`sonnet`, `opus`, ...); each one is sent to the CLI as the canonical model id it
resolves to, so compaction and every other request address the same model.

`claude_config_dir` becomes the CLI's `CLAUDE_CONFIG_DIR` for this account only, so two accounts can
use two logins without Fontaine reading either. `[network]` does not configure the CLI; set its own
proxy and TLS environment (`HTTPS_PROXY`, `NODE_EXTRA_CA_CERTS`, ...) where Fontaine starts.

`/login claude` in the terminal UI never asks for a key. It checks the CLI (installed, supported
version, signed in on Max or Ultra, models listed) and opens the model picker. When the CLI is not
signed in, it offers to run `claude auth login --claudeai` for you (with the account's `cli_path`
and `claude_config_dir`): on `y` the terminal is handed to the CLI, which opens a browser and may ask
you to paste a code, and Fontaine checks again when it exits. The check, not the CLI's exit status,
decides: a login that fails or leaves nobody signed in ends with the failure and no second offer.
The same offer opens first when Fontaine starts on a main model whose account is signed out. Any
other failed check (a missing or old CLI, a plan that is not Max or Ultra) is shown as is.
Logging out of or removing the Fontaine account never changes the CLI's own login.

A headless run never offers anything: it exits 1 before any turn with the account name and the exact
login command to run, then retry. The same stop applies when a pinned
[guardian](guardian.md) is on a signed-out account in a mode that consults it. A login that is
revoked mid-session is not repaired mid-turn; the failure names the login command, and
`/login <account>` performs it.

## Checks before every request

Before the models are listed and before every model request Fontaine:

1. runs `claude --version` (once per connection) and refuses a CLI older than **2.1.318**;
2. refuses to start when the environment sets a variable that would change which account or
   backend bills (the reminder probe sets `ANTHROPIC_BASE_URL` to its own loopback listener for
   its one launch; that is not an inherited endpoint): Anthropic API, auth and OAuth tokens, custom endpoints, headers, betas or extra
   body, Foundry, AWS and Vertex credentials and backend switches, Anthropic profiles, and model,
   effort or fallback overrides. The refusal names the variables, never their values; unset them
   and start again. The environment is looked up live, so a variable set later is seen too;
3. runs `claude auth status` for the account's profile and requires a signed-in `claude.ai`
   subscription on a Max or Ultra plan. For a model request this runs alongside the start of the
   request's own CLI process and of the reminder probe, whose request reaches only the loopback
   listener, never Anthropic;
4. asks the CLI for its model list (an `initialize` request, no turn, no inference) and refuses the
   selected model if its current row says it draws from paid usage credits or resolves to another
   model than when it was listed. For a model request the question goes to that request's own
   process, before the frame that asks for a response.

No response is requested until every check has passed, so a refused request still runs no turn.

A model whose canonical id this build of Fontaine lists with an output limit uses those facts. One
it does not list, such as a point release newer than the build, gets facts derived from the newest
listed Anthropic model of the same name pattern (`claude-haiku-9-9` from the latest listed
`claude-haiku-*`), by the rule every account kind with a catalog vendor follows (see
[Configuration](configuration.md#model-discovery)), with the effort levels, adaptive-thinking and
thinking-off flags the CLI itself reports for the row laid over them, and no price. Fontaine warns
once whenever such a model is resolved, naming the model it took the facts from, and compaction and
`/context` treat it as listed. Update Fontaine for its exact facts. A model with no listed model of
its name pattern, or one the CLI marks as drawing from paid usage
credits, is still refused; `/model`, `/guardian-model` and `fontaine models` show it as
unavailable with the reason instead of hiding it.

Every CLI process is started with an empty environment rebuilt from the program, identity and temp
paths, locale, the profile directory, proxy and TLS settings and privacy opt-outs. No other
variable, including other providers' keys, reaches it. The reminder probe's launch drops the proxy
settings, so its loopback request cannot be routed to a proxy.

## What a request is

Each model call starts a fresh CLI process (the first call of a Fontaine process also starts one
extra, short-lived CLI for the reminder probe) and replays the
conversation Fontaine holds: history as stream-json frames (signed reasoning included), then the
final user or tool-result frame, which is the only one that asks for a response. Nothing is
resumed or persisted by the CLI; Fontaine's ledger and session store are the only record. The
request's tools reach the model through an inert tool inventory (a hidden mode of the `fontaine`
executable): it lists the schemas and refuses every call, so the CLI never executes anything.
Fontaine receives the model's tool call and runs it itself, through the same gate as for any other
account. The CLI reports each call it refused (a `permission_denied` event and an error tool
result); those reports are accepted only for a call the same response made, under the name it was
announced with, and once each.

Every CLI process runs in one stable, empty, owner-only directory in Fontaine's data directory
(`claude-cli-cwd`), because the CLI writes its working directory into the prompt and a path that
changes per request would change the prompt after its first lines. Fontaine checks the directory
is empty when it connects, and before every launch refuses one that holds `CLAUDE.md`, `AGENTS.md`
or `.claude`. Git discovery from it is capped at the data directory. The files of one request
(settings, tool inventory, system prompt) stay in a temporary directory removed with the process.
Fontaine marks one message for the prompt cache (the final message on CLI 2.1.319 and later with a
reminder it learned, the last replayed user message otherwise; see [Supported CLI versions](#supported-cli-versions)), and
the CLI's claude.ai connectors are not fetched.

The CLI is started with its own memory, instruction files, compaction, hooks, subagents, retries,
fallbacks, prompt suggestions, tool search and updates switched off, and refuses to start a
response that shows any of that is active. Fontaine's own [MCP tool search](mcp.md#tool-search)
is off whenever the main model is on these accounts, whatever `[mcp] tool_search` says: the CLI owns the tool list. The CLI's own builtin plugins are accepted, since it
lists them even with every setting source off; any other plugin or skill is refused with an error
naming it. Instruction files stay off even though one accepted builtin is `cc-plugin-agents-md`:
each CLI process runs in an empty private directory and `CLAUDE_CODE_DISABLE_CLAUDE_MDS=1` is
set. Each response is read until exactly one complete message
and one result have been validated, then the process (and its descendants) is stopped. A second
response, an unsolicited suggestion, a native tool or permission step, an unexpected model or a cut
stream is a failure, never a response. The one server tool accepted is the provider's web search
(see [Web search](#web-search)). History the CLI cannot replay without loss (an assistant
prefill, a mid-history role, a hosted tool other than web search) is refused.

A response that stops at `max_tokens` is the exception to waiting for the result: the CLI would
follow it with its own output-limit recovery (a synthetic user message and up to three more model
calls, billed to the plan), so the process is stopped at that response's `message_stop` and the
truncated response is returned as any provider's would be. The recovery never runs; only what the
CLI generated before the stop is spent. No CLI switch for the recovery was found on 2.1.319.

## Web search

The provider's own search is offered as it is for `anthropic` accounts, under the same
`[websearch] native` setting and the same permission and hook conditions (see
[Web search](tools.md#web-search)). The search runs inside the model's response: the CLI executes
nothing, the search is drawn from the plan, and the searches and their results are kept in the
conversation and replayed on later turns. With `native = "off"` or a rule or hook that withholds
it, the `websearch` tool serves searches instead.

## Usage, cost and the extra-usage setting

Usage is subscription usage: token counts are shown (`/cost`, headless `result.usage`,
[telemetry](telemetry.md)), a cost never is, and the list-price figures the CLI prints are never
presented as an invoice. `--max-budget-usd` with a `claude-subscription` main model is a usage error
(exit code 2); token budgets still apply.

The 5-hour and weekly plan usage shown in the status bar and in headless `result.account_usage`
comes from the rate-limit events the CLI streams with each reply. Fontaine makes no request for it,
and the figures never leave the machine. They appear after the first turn.

Fontaine never selects paid usage credits, but it cannot see your account's extra-usage setting: if
it is on, exhausting the included limit can switch a Max or Ultra session to paid credits.
Disable it at <https://claude.ai/settings/usage> if you need included-subscription-only use. A
billing failure from the CLI is reported once, without a retry. A plan limit that has used up the
window (the CLI's rate-limit event said `rejected`) is a
[usage window](headless.md#usage-windows) that resets by itself: the first request fails without a
retry, a [model list](configuration.md#falling-through-to-the-next-entry) moves on to its next entry,
and when none is left (a subagent first tries the
[`fallback` role](configuration.md#subagent-fallback)) the run pauses until the reset time the CLI
reported. A rate limit the CLI
reports without a rejected window, and a billing failure, are not waited for.

## Failures

| Failure | Result |
|---|---|
| At startup: credit-billed model | exit code 2 with the corrective text; no turn runs |
| CLI missing, too old, not signed in, wrong plan, conflicting variable, malformed protocol, or a model that turns credit-billed at request time | exit code 1 with the corrective text; no turn runs and nothing is retried. A protocol failure names the rejected event, such as `(event: system/notification)` |
| Billing error, or a rate limit with no spent window, from the CLI | the same, after the run began; it also moves a [model list](configuration.md#falling-through-to-the-next-entry) on to its next entry |
| Plan limit with the usage window spent | not retried; a model list moves on to its next entry, a subagent whose list is spent tries the `fallback` role, and a spent list pauses the run until the reset (see [usage windows](headless.md#usage-windows)) |
| Anthropic's safety classifier withholds the reply | not retried, since the same request would be refused again: the turn ends with an error saying the classifier stopped the response. Nothing ran, and the [guardian](guardian.md) cannot observe a call the classifier withholds |
| Overloaded or server error, a stream cut short, a CLI that is silent for 180 seconds | retried with backoff, like any provider (see [`[retry]`](configuration.md#retry)) |
| The CLI's version, login or model-listing probe taking too long | the same as a failed probe: not retried |
| The reminder probe taking too long, or failing | the request is not failed: it takes the earlier cache layout, and the probe is tried again after 5 minutes |

A protocol failure names what the CLI did wrong: the MCP servers and statuses it reported when the
inert tool inventory did not start (a `failed` inventory is one the CLI could not start or talk to),
or the tool it called or offered that the request did not define, alongside the tools the request
did define. An unprefixed name of a tool the request defines is accepted as that tool; any other
undefined name still fails the request. With `--debug-file`, a protocol failure also writes the
rejected line (cut to 4 KiB) and the events accepted before it to the debug file. A failed request
is recorded in the session ledger as `request_failed`, with the same text.

With `--debug-file`, every finished request of the native layout also writes one debug event,
`claude cache self-check`, with counts, hashes and tokens only (no message text): `session`, `cli`,
`wording` (first 8 hex digits), `layout`, `frames`, `added` (blocks added since the previous
request), `usage` and `previous_usage` (`read`, `input`, `write`, `write_1h`, `write_5m` tokens;
`write` is the total cache write, so a write in the 5-minute bucket is visible), `gap_secs`,
`thinking` (whether the conversation holds a thinking block) and `outcome`: `healthy`, `broken` or
`skipped:<reason>`. The reasons are `first_request`, `not_extended`, `merged_tail`,
`unreplayable_tail`, `settings_changed`, `cold_previous`, `gap_too_long` and `too_many_added`; for
`not_extended` the event adds `first_diff` (the first frame whose fingerprint differs), `prev_frames`
and `now_frames`. A `broken` request has `strike` set to `strike` (the first shortfall) or
`verdict` (the second, which turns the layout off). A request with no native layout, CLI version,
reminder wording or cache usage logs `outcome="no_record"` and `missing`.

A `user` event the adapter refuses fails with `the Claude CLI executed a native tool or permission
request` followed by the shape it refused (for example `content that is not a list of blocks`). To
capture the line for a report, rerun the same request with `--debug-file <path>`: the rejected
line is written there, cut to 4 KiB, and it may contain tool output, so read it before sharing.

A guardian on this account fails closed like any other: a verdict that cannot be read is a denial.
Pressing Esc or ending a headless run stops the CLI and its descendants and removes the temporary
directory.

## Supported CLI versions

The minimum is **2.1.318**. The gate is a minimum only: newer versions are accepted, and a
protocol shape the adapter does not know still fails closed. The cache layout below is a separate
matter, with its own threshold at 2.1.319. The order 2.1.319 uses to report a
denied call, inside the response before its stop, is supported alongside the earlier order after
the stop. The `system/informational` notice 2.1.329 emits in some turns is accepted and ignored.
Windows runs the official executable with `taskkill /T /F` as the tree cleanup.

Measured on 2.1.319: each request costs a CLI start (about 0.4 s) plus about 5 ms per replayed
user message, and the first request of a Fontaine process adds one more CLI start for the
reminder probe.

On versions from 2.1.319, replayed history is read from the prompt cache up to the previous
request's final message. The adapter repeats the CLI's own context reminder, as the CLI worded it,
on every replayed user message, where the CLI put it, so each replayed message matches the request that last wrote it
byte for byte, and it puts its cache marker on the final message. Each step then writes only its
new messages plus about 170 tokens of the reminder per user message, near a native Claude Code
session's write. The write that is never read back is the CLI's trailing date and token-budget
block; it has a 5-minute lifetime instead of the 1-hour one, which cuts its billed cost by about a
third, and the marker on the final message keeps the hour. A request of one message gets no marker,
so a one-shot caller such as the guardian keeps the CLI's 5-minute write, and the first message of a
conversation is written once more on its second step.

The adapter learns the reminder from the CLI instead of assuming its wording, which changes between
CLI releases. At the first request of a Fontaine process, per CLI version and account, it starts the
CLI once more with `ANTHROPIC_BASE_URL` pointing at a listener it opens on the loopback interface,
under a random path only that launch knows, and reads the reminder out of the request the CLI makes
there. The listener answers with a canned reply and forwards nothing, so the probe makes no model
call and uses no quota; it costs one CLI start (about 0.4 s), and requests that start meanwhile
share it. The CLI's request carries its bearer token and the account email; the listener reads only
the content-length header and the body, drops the rest of the headers, token included, without
storing or logging them, and the email is kept in memory like the one in the `initialize` answer.
The reminder is used only if it has the shape the replay reproduces: the email once, and no section
beyond `# userEmail` (a git or project section would depend on the directory). A reminder that
cannot be read in time, or is not of that shape, leaves the request on the earlier layout below; a
probe that could not tell is retried no sooner than 5 minutes later, and a shape that cannot be
replayed is kept for the process.

The adapter checks that the replay works. It compares a conversation's cache read with what the
previous request of that conversation wrote. A previous request that itself read nothing was a cold
start and gives no verdict. On a shortfall (the replayed bytes no longer match what the CLI wrote, for example
because a placement differs from the measured ones) twice in a row for one CLI version and one
reminder wording, with no healthy comparison in between, it turns the layout off for that pair and
shows one warning in the session naming the version, the first 8 hex digits of the wording's
SHA-256 and the numbers it judged on (read X of expected Y after Z s); `cache-bench` measures the
effect. The verdict is kept in `claude-cache-verdicts.json` in the data dir (readable by the current user
only), so a restart keeps the fallback. Each entry records `read` and `expected`, and the previous
request's `prev_read` and `prev_write_1h` and whether the conversation held a `thinking` block, so a
verdict can be judged afterwards without a debug file; an older file without those fields loads
unchanged. It expires after 7 days, when the version is measured
again, and is retired earlier when the CLI's reminder wording changes or a Fontaine update changes
where the adapter places the reminder. A new CLI version is judged afresh. The layout needs an
account email in the CLI's answer and a reminder the probe learned.
On versions below 2.1.319, without an email, when the reminder could not be learned, or after a
warning, the adapter keeps the earlier
layout: the marker on the last replayed user message, read back up to that message, and each request
writing the last two messages, about twice a native session's write per step. The never-read
5-minute write there is the CLI's breakpoint on the final message.

Reuse holds while a step adds no more than about 20 blocks; a wider parallel step (about ten or
more tool calls) re-writes the history once.

### What the CLI still adds

Even with Fontaine's own system prompt, the CLI puts four things of its own into every request,
and no switch in 2.1.319 removes them without also breaking sign-in:

- the line "You are a Claude agent, built on Anthropic's Claude Agent SDK." ahead of the system
  prompt;
- an `# Environment` block (working directory, whether it is a git repository, platform, shell, OS
  version and the model name), where the working directory is a stable empty directory in the data
  directory;
- a `<system-reminder>` with the signed-in account's email address, on the message that asks for a
  response;
- the current date and a token-budget line after it.

These come from the CLI, not from Fontaine, which still does not present itself as Claude Code.
Measured on 2.1.319 over a 5-step tool loop of about 8200-token tool results: a native session
writes about one tool result per step; with the earlier layout the adapter writes about 25300 per
step (the previous message again, without the reminder, plus the new one); with the current layout
it writes about 8600 (the new result, the reminder and the date block), and reads the rest from the
cache.

The adapter repeats the CLI's reminder on replayed messages so they match byte for byte. The email
it holds is the one the CLI already sends on every request (the probe's listener sees it, on the
loopback interface only): it goes to the same endpoint, and the
adapter keeps it in memory for the request only, never in a log, the ledger or an error. The cost
is about 170 tokens of context per user message, about 17000 tokens at 100 tool calls.

### Capturing traffic

To see why a replayed request missed the cache, set `FONTAINE_CLAUDE_CAPTURE` in the environment
of the process that runs Fontaine. It is for development and is off unless set:

| Value | Records |
|---|---|
| unset, empty, `0` | nothing |
| `1`, `full` | each request's complete frame list as sent, cache marker and reminder included |
| `delta` | only the messages that are new since the conversation's previous request |

Any other value is off, with one warning per connection that names the accepted ones. There is no config key at
any scope, and `--debug-file` and `FONTAINE_LOG` never enable it, so a shared project file cannot
turn it on.

Nothing is created until the first request. Then Fontaine creates
`<data dir>/claude-capture/<session>-<token>.jsonl` (the directory 0700, the file 0600), one file
per connection, shared by every conversation of that connection (the main model's connection,
which the guardian on the same account and subagents share), and warns once per file with its path.
A subagent model, a `/model` switch or a guardian account that connects separately gets its own
file and warning, and request indices restart in each file. **The file is not redacted**: it holds every prompt, file
content and tool result in the frames, plus the CLI's result events. Nothing is exported: it never
reaches telemetry, the ledger or a remote device. If the files cannot be created or written, one
warning is shown, the capture stops and the request goes on. Delete the whole
`<data dir>/claude-capture/` directory when you are done.

Each line is a `request` record (the conversation's session, a request index, a timestamp, the
mode, a hash per frame taken before the cache layout, `diverged_at` and the frames) or a `result`
record (the CLI's final `result` event, verbatim, with its usage). The hashes name the messages and
not their layout, which moves the cache marker between requests, so `diverged_at` is `null` while a
request extends the previous one, the index of the first changed message when history was edited,
and the request's length when it is shorter. A `delta` record shows messages only, not where the
marker or reminder sat.

The capture also sets `OTEL_LOG_RAW_API_BODIES` for the CLI of each request, pointing at a
`<name>.bodies/` directory (0700) beside the file, so the CLI's own raw-body capture lands there.
That is an undocumented CLI feature that may write nothing and that redacts thinking text; an empty
directory is not an error. Discovery, login probes and the reminder probe never get the variable.

`reassemble` and `diff` in `fontaine_sdk::core::providers::claude_subscription::capture` rebuild a
request's frames and compare two. The example prints the diff between request `index - 1` and
`index` (the divergence point first, then the first differing frame and the JSON paths that changed
in it):

```sh
cargo run -p fontaine-sdk --example claude_capture_diff -- <file> <session> <index>
```

### Checking cache usage

A cache miss changes no output, only usage, so the adapter's cache write is measured against
`claude -p` with a bench in `fontaine-evals`. It needs a signed-in subscription and the real CLI, so
it runs by hand or nightly, never in CI:

```
cargo run -p fontaine-evals -- cache-bench --model <account>/<model> \
  --out ../evals/scorecards/claude-subscription-cache-<model>.json
```

It reads six fixture files of about 2000 tokens one at a time through `fontaine -p` and through
`claude -p`, and takes each model call's cache write from the `model_call` events of
[headless output](headless.md#output) and from `claude -p`'s `assistant` events. The scorecard holds
every step, the totals, the CLI version and the models each side reports serving. Read the `ratio`:
the mean cache write per step of Fontaine over Claude Code, leaving out the first step (it carries
the base context), the second (Fontaine's writes only one message; left out on both sides so the
means cover the same steps) and the last. The run exits 1 when the ratio is above `--threshold`
(default 1.25, above the ratio the current layout is expected to reach; a CLI version on the earlier
layout, described under [What the CLI still adds](#what-the-cli-still-adds), will exceed it) or a
side reported fewer than five model calls. Counts are tokens only: the cheaper 1-hour write rate is
not part of them.
