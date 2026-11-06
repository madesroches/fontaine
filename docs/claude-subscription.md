# Claude subscription (experimental)

The `claude-subscription` account kind runs your installed, unmodified Claude Code CLI as the
model transport. The CLI owns your Claude sign-in and its subscription billing; Fontaine keeps its
own agent loop, tools, permission pipeline, guardian, sessions, memory and compaction. It is an
experiment: selecting such an account as the main model prints a notice saying so.

## Scope

- Ordinary, individual, local use of your own **Max or Ultra** plan through the official `claude`
  binary. Pro, Team and Enterprise plans are refused.
- Not supported: sharing or pooling credentials, resale, hosted or multi-tenant use,
  `claude setup-token`, using `CLAUDE_CODE_OAUTH_TOKEN` directly, and importing anything from
  `~/.claude`. Fontaine never reads, copies or forwards a Claude credential, never presents
  itself as Claude Code, and never selects API-key, gateway, cloud-backend, paid-credit or
  fallback-model routing.
- Anthropic's terms for the Agent SDK and for signing in to Claude Code apply to you. Check them
  before relying on this account kind; it is not offered as a stable feature.

## Setup

Install the CLI, sign in outside Fontaine, and add the account:

```sh
claude auth login
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

`/login claude` in the terminal UI never asks for a key and never starts a login of its own. It
checks the CLI (installed, supported version, signed in on Max or Ultra, models listed) and opens the
model picker; when a check fails it says to run `claude auth login` outside Fontaine. Logging out of
or removing the Fontaine account never changes the CLI's own login.

## Checks before every request

Before the models are listed and before every model request Fontaine:

1. runs `claude --version` (once per connection) and refuses a CLI older than **2.1.318**;
2. refuses to start when the environment sets a variable that would change which account or
   backend bills: Anthropic API, auth and OAuth tokens, custom endpoints, headers, betas or extra
   body, Foundry, AWS and Vertex credentials and backend switches, Anthropic profiles, and model,
   effort or fallback overrides. The refusal names the variables, never their values; unset them
   and start again. The environment is looked up live, so a variable set later is seen too;
3. runs `claude auth status` for the account's profile and requires a signed-in `claude.ai`
   subscription on a Max or Ultra plan;
4. asks the CLI for its model list (an `initialize` request, no turn, no inference) and refuses the
   selected model if its current row says it draws from paid usage credits or resolves to another
   model than when it was listed.

A model whose canonical id this build of Fontaine has no model facts for (output limit, features)
is refused rather than guessed at.

Every CLI process is started with an empty environment rebuilt from the program, identity and temp
paths, locale, the profile directory, proxy and TLS settings and privacy opt-outs. No other
variable, including other providers' keys, reaches it.

## What a request is

Each model call starts a fresh CLI process in a private temporary directory and replays the
conversation Fontaine holds: history as stream-json frames (signed reasoning included), then the
final user or tool-result frame, which is the only one that asks for a response. Nothing is
resumed or persisted by the CLI; Fontaine's ledger and session store are the only record. The
request's tools reach the model through an inert tool inventory (a hidden mode of the `fontaine`
executable): it lists the schemas and refuses every call, so the CLI never executes anything.
Fontaine receives the model's tool call and runs it itself, through the same gate as for any other
account. The CLI reports each call it refused (a `permission_denied` event and an error tool
result); those reports are accepted only for a call the same response made, under the name it was
announced with, and once each.

The CLI is started with its own memory, instruction files, compaction, hooks, subagents, retries,
fallbacks, prompt suggestions, tool search and updates switched off, and refuses to start a
response that shows any of that is active. The CLI's own builtin plugins are accepted, since it
lists them even with every setting source off; any other plugin or skill is refused with an error
naming it. Instruction files stay off even though one accepted builtin is `cc-plugin-agents-md`:
each CLI process runs in an empty private scratch directory and `CLAUDE_CODE_DISABLE_CLAUDE_MDS=1`
is set. Each response is read until exactly one complete message
and one result have been validated, then the process (and its descendants) is stopped. A second
response, an unsolicited suggestion, a native tool or permission step, an unexpected model or a cut
stream is a failure, never a response. History the CLI cannot replay without loss (an assistant
prefill, a mid-history role, a hosted tool) is refused.

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
plan-limit or billing failure from the CLI is reported once, without a retry and without trying
another account or model.

## Failures

| Failure | Result |
|---|---|
| At startup: credit-billed model | exit code 2 with the corrective text; no turn runs |
| CLI missing, too old, not signed in, wrong plan, conflicting variable, malformed protocol, or a model that turns credit-billed at request time | exit code 1 with the corrective text; no turn runs and nothing is retried. A protocol failure names the rejected event, such as `(event: system/notification)` |
| Plan limit, allowance or billing error from the CLI | the same, after the run began |
| Overloaded or server error, a stream cut short, a CLI that is silent for 180 seconds | retried twice with backoff, like any provider |
| The CLI's version, login or model-listing probe taking too long | the same as a failed probe: not retried |

With `--debug-file`, a protocol failure also writes the rejected line (cut to 4 KiB) and the events
accepted before it to the debug file. A failed request is recorded in the session ledger as
`request_failed`.

A guardian on this account fails closed like any other: a verdict that cannot be read is a denial.
Pressing Esc or ending a headless run stops the CLI and its descendants and removes the temporary
directory.

## Supported CLI versions

The minimum is **2.1.318**. The gate is a minimum only: newer versions are accepted unqualified (no
version is live-qualified yet), and a protocol shape the adapter does not know still fails closed. The order 2.1.319 uses to report a
denied call, inside the response before its stop, is supported alongside the earlier order after
the stop. Windows runs the official executable with `taskkill /T /F` as the tree cleanup. Each request
costs a process start and a replay of the history, so latency and cache behavior differ from a
native session.
