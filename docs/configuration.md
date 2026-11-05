# Configuration

Fontaine reads TOML settings from several places and merges them into one configuration. Every
key is documented below with its meaning, its default and the scopes that may set it.

A key marked "accepted and validated; not yet active" is parsed and checked at load, and a bad
value is a configuration error, but nothing consults it yet.

## Where files live

| What | Linux | macOS | Windows |
|---|---|---|---|
| Config dir | `~/.config/fontaine` | `~/Library/Application Support/fontaine` | `%APPDATA%\fontaine` |
| Data dir | `~/.local/share/fontaine` | `~/Library/Application Support/fontaine` | `%APPDATA%\fontaine` |

The platform defaults come from the `dirs` crate. Two environment variables override them, and
tests use them to stay inside temporary directories:

- `FONTAINE_CONFIG_DIR` replaces the config dir.
- `FONTAINE_DATA_DIR` replaces the data dir.

If a directory has no platform default and no override, Fontaine exits with code 2 and names the
variable to set.

Files in these directories:

| File | Purpose |
|---|---|
| `<config dir>/config.toml` | The user settings file. |
| `<config dir>/plugins.toml` | The [plugin and marketplace](plugins.md) registry, written by the `fontaine plugin` CLI and by the [plugin auto-update](plugins.md#auto-update) pass. It is not a settings file. |
| `<config dir>/skill-trust.toml` | The skills whose `allowed-tools` you trusted, each pinned to a digest of the skill file (see [Skill trust](#skill-trust)). Written by `fontaine plugin trust` and `/skills trust`; not a settings file, and not the workspace `trust.toml`. |
| `<data dir>/trust.toml` | Workspace trust record (see [Workspace trust](#workspace-trust)). |
| `<data dir>/credentials.toml` | Stored API keys (see [Credentials](#credentials)). |
| `<data dir>/auto_mode/<project>/environment.json` | The project facts `fontaine auto-mode setup` collected for the [guardian](guardian.md#project-environment) (setup also writes `[guardian.composition]` to the user settings file when you accept its [offer](guardian.md#setting-up-the-composition)). Readable by the current user only; the agent's file tools cannot write it unprompted. |
| `<config dir>/config.lock`, `<config dir>/plugins.lock`, `<config dir>/skill-trust.lock`, `<data dir>/trust.lock`, `<data dir>/credentials.lock` | Empty lock files beside the files they guard. Sessions hold one while they edit that file, so concurrent edits do not overwrite each other, and briefly while they read the settings or trust file, so a read waits for an edit in progress; safe to delete when no session is running. |
| `<data dir>/history.jsonl` | What you submitted in the terminal UI, for Up and Ctrl-R recall (see [History](tui.md#history)). Readable by the current user only. |
| `<data dir>/plugin-update.stamp` | When the [plugin auto-update](plugins.md#auto-update) pass last ran, so it runs at most once a day. |
| `<data dir>/plugin-updates.jsonl` | One JSON line for each plugin or marketplace update the pass staged, held or failed. Readable by the current user only. |
| `<data dir>/recent_models.json` | The models you last switched to with `/model` and `/guardian-model`, for the picker's Recent group (see [Choosing a model](tui.md#choosing-a-model)). Readable by the current user only. |

Files that hold secrets or trust decisions are created readable by the current user only: mode
`0600` on Unix, a current-user-only ACL on Windows.

## The settings scope stack

Settings come from up to five scopes. Higher scopes win, listed here highest first:

1. **managed**: a reserved slot. It is empty and no file is read for it.
2. **CLI**: the file named by `--settings <file>`. It applies to one session and has user trust.
3. **project-local**: `<workspace>/.fontaine/settings.local.toml`. It has user trust only while
   Fontaine can show it is a personal file (see [Workspace trust](#workspace-trust)); otherwise it
   loads as project scope.
4. **project**: `<workspace>/.fontaine/settings.toml`. It is shared with the repository and may only
   tighten.
5. **user**: `<config dir>/config.toml`. It may set every key.

The workspace is the project directory (the launch directory, or `--cd`), after symlinks are resolved. Project
files are read only from it: there is no search through parent directories and no nested settings
files. A missing project, project-local or user file is treated as empty. A file that exists but
cannot be read, such as a directory in its place, is a configuration error that names the path. A
missing or unreadable `--settings` file is also an error.

`--settings` takes the path of a TOML file. It does not accept inline text.

In the terminal UI, `/config` shows each key's effective value and the scope that supplied it, and
edits a few preferences in the user file; it never writes a project file. See the
[settings panel](tui.md#settings-panel). `fontaine doctor` shows the same provenance without a
session, reports every layer that fails to load (including a project key the scope policy
refused) instead of stopping at the first, and checks trust, models and MCP servers; see
[`fontaine doctor`](doctor.md).

### Merge rules

- Scalars: the highest scope that sets the key wins.
- Lists (`deny`, `ask`, `allow`, `additional_directories`, `[auto_mode]` lists, `allow_extra`,
  hooks): the union across scopes. A scope can add only to the lists its column below allows.
- Named table entries (`[accounts.<name>]`, `[mcp.servers.<name>]`, `[agents.types.<name>]`): the
  highest scope that defines the name replaces the whole entry. Entries are never merged field by
  field.
- `[network]`: merged per key, with `--settings` over the user file. An account's own network keys
  are then laid over it key by key for that account (see [`[network]`](#network)).
- Map-valued keys (`[telemetry] resource_attributes`, `[env]`): merged per key, and the higher scope wins a
  shared key.
- `[[hooks.*]]` arrays: the union.

### Strictness

- An unknown key or section in any scope is an error, so a misspelled `[permisions]` cannot
  silently drop deny rules.
- A key that a scope may not set is an error at load, naming the key and the scope.
- A permission rule that does not parse, or that names a tool that is neither built in nor an
  `mcp__` name, is an error.

All configuration errors exit with code 2.

## Per-key scope table

The user file may set every key. Every key has a declared policy for the other scopes, and a key
without one is refused from project and project-local files. For those scopes:

| Key | Project | Project-local (user trust) | CLI |
|---|---|---|---|
| `[accounts.*]`, including their network keys | no | no | yes |
| `[network]` | no | no | yes |
| `[websearch]` | no | no | yes |
| `[model] model` | no | yes | yes |
| `[guardian] model`, `profile`, `timeout_secs`, `confidence_threshold`, `cache` | no | yes | yes |
| `[guardian] scanner`, `scanner_command`, `scanner_model_path`, `scanner_hf_token_env` | no | no | no |
| `[guardian.composition]` | `shadow = true` only | `shadow = true` only | `shadow = true` only |
| `[permissions] default_mode` | `manual` or `dontAsk` only | `manual` or `dontAsk` only | yes |
| `[permissions] deny`, `ask` | add only | add only | add only |
| `[permissions] allow` | no | yes | yes |
| `[permissions] additional_directories` | no | yes | yes |
| `[permissions] auto_tracked_edits`, `auto_scratch_writes` | `false` only | `false` only | yes |
| `[auto_mode] environment`, `allow`, `setup_reminder` | no | yes | yes |
| `[auto_mode] soft_deny`, `hard_deny` | add only | add only | add only |
| `[agents]` keys and `[agents.types]` | no | yes | yes |
| `[context] autocompact_window` | no | yes | yes |
| `[schedule] enabled` | no | no | yes |
| `[memory] auto` | no | no | yes |
| `[goal]` keys | no | no | yes |
| `[afk] temperature_ceiling` | lower only, positive and finite | lower only, positive and finite | yes |
| `[shell] max_timeout_secs` | no | no | yes |
| `[statusline]` | no | no | no |
| `[checkpoints]` keys | no | no | no |
| `[plugins] auto_update` | no | no | no |
| `[shell_preclassifier] allow_extra` | no | yes | yes |
| Agent files `agents/*.md` | narrowing only | n/a | n/a |
| `[[hooks.*]]` | yes, after workspace trust; `allow` ignored | yes, after workspace trust | yes |
| `[mcp.servers.*]` | yes, after workspace trust and your approval of each server | yes, after workspace trust | yes |
| `[lsp.servers.*]` | no | no | yes |
| `[telemetry]` | `enabled = false` only | `enabled = false` only | yes |
| `[tui]` | no | no | yes |
| `[keybindings]` | no | no | yes |
| `[env]` | no | yes | yes |

A project-local file that has not earned user trust is validated against the project column.

### Trust asymmetry

Shared project settings can add `deny` and `ask` rules and `[auto_mode]` `soft_deny` and
`hard_deny` entries, because those can only make Fontaine stricter. They cannot add `allow` rules,
choose a model or an endpoint, widen the workspace, or change anything that loosens a decision.
Project hooks and MCP servers may be declared; a project-declared executable can tighten a
permission decision (a hook's deny or ask, an MCP tool's `requiresUserInteraction` marker) but
never loosen one.

`[env]` can put a different `PATH` in front of the system's or point tools at a proxy, so it comes
only from user-trust scopes: your user file, `--settings` and a project-local file that has earned
user trust. A shared project file with an `[env]` table is an error.

`[guardian] profile` is a loosening knob, so it is settable from user-trust scopes only. No key in
any scope turns the guardian off.

## Key reference

### `[accounts.<name>]`

Named model accounts. Several accounts of one kind are allowed, and each keeps its own endpoint
and credential. A model is addressed as `<account>/<model>`, split at the first `/`, so a model id
may contain slashes of its own (`router/anthropic/claude-sonnet-4-6`).

| Key | Meaning | Default |
|---|---|---|
| `kind` | `anthropic`, `openai`, `gemini`, `openai-compatible`, `ollama`, `opencode-go`, `openrouter` or `claude-subscription` (experimental). Required. | none |
| `api_key_env` | The name of the environment variable that holds the key. Omit it to use the stored credential. Raw keys are never accepted inline. Refused for `opencode-go`, which signs in instead. | unset |
| `base_url` | Endpoint override. Required for `openai-compatible`. | the provider's public endpoint |
| `models` | Explicit model list. Without it the provider is asked which models it offers. | unset |
| `wires` | `opencode-go` only: a table from model id to the wire it is served on (`chat`, `responses`, `messages` or `gemini`), for a model the built-in routing gets wrong. | unset |
| `context_tokens` | `ollama` only: the context window requested with every call (`options.num_ctx`), at least 2048. | `16384` |
| `prompt_caching` | `anthropic`, `openrouter` and `opencode-go` only: whether requests carry cache breakpoints, so the provider reuses the unchanged prefix at a fraction of the input price. On `anthropic` they sit on the tools, the system prompt and the latest message. On `openrouter` the conversation carries one top-level marker for every model except the self-caching `openai/`, `deepseek/`, `x-ai/` and `groq/` upstreams, which are sent none, and the guardian marks its system prompt. On `opencode-go` it affects only models served on the Messages wire; the other wires cache by prefix on their own. User scope only, because it changes spend and what the provider stores. | `true` for `anthropic` and `openrouter`, `false` for `opencode-go` |
| `cache_ttl` | `anthropic`, `openrouter` and `opencode-go` only: how long a cached prefix lives, `"5m"` or `"1h"`. The one-hour lifetime costs more per cache write, and is not offered to the self-caching `openrouter` upstreams, which keep their own lifetime. Has no effect when `prompt_caching` is off. User scope only. | `"5m"` |
| `think` | `ollama` only: `false` asks a thinking model to answer without reasoning, which is much faster. `true` and unset leave the model's default. An explicit [`effort`](#model) wins over it, and `effort = "off"` equals `think = false` for the main agent. | unset |
| `quota` | `openai-compatible` only: `"kimi"` shows the account's subscription quota in the [status bar](tui.md#status-bar) and in headless `result.account_usage`, read from Kimi for Coding's `GET <base_url>/usages`. User scope only, because it sends the account's key in extra requests. | unset |
| `cli_path` | `claude-subscription` only: the absolute path of the official `claude` executable. | `claude` on `PATH` |
| `claude_config_dir` | `claude-subscription` only: the absolute path of the CLI's own config directory, handed to it as `CLAUDE_CONFIG_DIR` for this account only, so two accounts can keep separate logins. | unset |
| `https_proxy`, `http_proxy`, `no_proxy`, `ca_bundle`, `client_cert`, `client_key` | Override the same key of [`[network]`](#network) for this account only. | the `[network]` value |

Account names are not empty and contain no `/`. Accounts can be set from the user file or from
`--settings` only: a project file cannot choose an endpoint or a credential source. The per-account
network keys are edited by hand; the account setup flow in the terminal UI writes only `kind` and
`base_url`.

What each kind talks to:

| Kind | Wire | Default endpoint | Credential |
|---|---|---|---|
| `anthropic` | Messages API | `https://api.anthropic.com` | required |
| `openai` | Responses API | `https://api.openai.com/v1` | required |
| `gemini` | `generateContent` | `https://generativelanguage.googleapis.com` | required |
| `ollama` | Ollama's native `/api/chat` | `http://localhost:11434` | optional |
| `openai-compatible` | Chat Completions | none; `base_url` is required | optional |
| `opencode-go` | Chat Completions, Responses, Messages and `generateContent`, per model | `https://opencode.ai/inference/go` | sign-in |
| `openrouter` | Chat Completions, with prompt caching | `https://openrouter.ai/api/v1` | required |
| `claude-subscription` | Messages API, through the installed Claude Code CLI | none | the CLI's own login |

An `openai-compatible` endpoint speaks Chat Completions, not the Responses API, and its models are
unpriced, so `--max-budget-usd` does not apply to them.

A flat-rate account has no token price worth estimating; its own quota is the budget signal. With
`quota = "kimi"` Fontaine asks `<base_url>/usages` for the account's usage windows when the
account's model is in use, then every minute, with the account's key as the bearer token (no
`Authorization` header when the account has none) and a 10 second timeout. No credential other than
the account's own is sent, and only to the account's own `base_url`. It is display only:
it never affects permissions or the guardian, and a failure is reported once and hides the segment. The
same applies to the account the guardian runs on, and a headless run asks once per `result` line
instead of every minute.

```toml
[accounts.kimi]
kind = "openai-compatible"
base_url = "https://api.kimi.com/coding/v1"
api_key_env = "KIMI_API_KEY"
quota = "kimi"
```

#### Where a credential comes from

1. If the account sets `api_key_env`, the key is that environment variable. An unset or empty
   variable is a configuration error (exit code 2) when the main model uses the account, for any
   kind; the stored credential is not consulted as a fallback.
2. Otherwise the key is the account's entry in `credentials.toml`, written by `/login` in the
   terminal UI.
3. With neither, an `ollama` or `openai-compatible` account sends no `Authorization` header. An
   `anthropic`, `openai`, `gemini` or `openrouter` account with no key is a configuration error (exit code 2)
   when the main model uses it.

An `opencode-go` account has no key. Its credential is the sign-in that
`/login <account>` stores, and an account that has not signed in is a configuration error (exit
code 2) that names that command.

A `claude-subscription` account has no Fontaine credential at all: the installed Claude CLI owns the
login, a stored key or an `api_key_env` is refused or ignored, and `/login` only checks the CLI. See
[Claude subscription](claude-subscription.md).

The guardian is different: a missing or invalid credential on the account of `[guardian] model`
never stops startup. The guardian is then unavailable, and every decision that needs it fails
closed.

#### Model discovery

An account with a `models` list accepts exactly those names. An account without one is asked for
its model list (`/models` for OpenAI-style endpoints, `/api/tags` for Ollama, the provider's model
listing otherwise), once, and only for the account that `[model] model` or `--model` names. A
model the account does not offer is a configuration error (exit code 2). A listing call that
fails, whether by a transport error, a non-2xx status or a 401/403, is a run failure (exit code 1).
For Ollama a bare name matches the `latest` tag.

A `claude-subscription` account is always listed, even with a `models` list, because the models'
facts come from the CLI's own answer: the list only narrows what the CLI offers. The listing
checks the CLI, so a missing or outdated CLI, a login that is not a Max or Ultra subscription and a
conflicting environment variable are run failures (exit code 1) with a corrective message, and a
model the CLI marks as drawing from paid usage credits is a configuration error (exit code 2). Every
listing is bounded in time, including the one at startup.

The model of `[guardian] model` is checked the same way when its account has a `models` list, or
shares the main model's account. Otherwise Fontaine makes no call with the guardian's credentials
at startup. An unknown account in `[guardian] model` is always a configuration error.

With no `[guardian] model`, the guardian is the highest-ranked benchmark-validated model that one
of your accounts offers (see [The guardian](guardian.md#choosing-the-guardian-model)). An account
offers a model when its `models` list contains it, when it is the main model's account and the
listing already fetched contains it, or, for any other account without a list, when its `kind`
matches and a credential is present; no listing call is made with a guardian credential. When no
account qualifies, the main model is the guardian and the start shows a warning ("Guardian is
running on your main model `<account>/<model>`", with the advice to set `[guardian] model` or run
`/guardian-model`): in the conversation in the terminal UI, on stderr in a headless run.

`fontaine models [<account>]` prints the models each account offers (just that account when named),
marking the main model, the guardian model and the benchmark-validated guardian models. It lists
the way the `/model` picker does: an account with a `models` list costs no call, the others are
asked once with a 10 second limit, and an account that fails keeps its entry with the error.
`--format=json` prints the same report as JSON (`{"accounts": [{"account", "kind", "models":
[{"id", "roles", "validated_guardian"}]}]}`, or `{"account", "kind", "error"}` for a failed one).
`fontaine --model <account>/<model> models` marks that model as the main one. The exit code is 2
for an unknown account or when no account is configured, and 1 when every requested account
failed to list. The agent can read the same report through the `models` tool (see
[Tools](tools.md)); neither shows endpoints, keys or environment values.

#### Worked examples

Anthropic, with the key in an environment variable:

```toml
[accounts.work]
kind = "anthropic"
api_key_env = "ANTHROPIC_API_KEY"

[model]
model = "work/claude-sonnet-5"
```

A second Anthropic account with its own key, stored with `/login personal`:

```toml
[accounts.work]
kind = "anthropic"
api_key_env = "ANTHROPIC_API_KEY_WORK"

[accounts.personal]
kind = "anthropic"

[model]
model = "work/claude-sonnet-5"

[guardian]
model = "personal/claude-haiku-4-5"
```

OpenAI:

```toml
[accounts.openai]
kind = "openai"
api_key_env = "OPENAI_API_KEY"
models = ["gpt-5.2", "gpt-4o-mini"]

[model]
model = "openai/gpt-5.2"
```

Gemini:

```toml
[accounts.google]
kind = "gemini"
api_key_env = "GEMINI_API_KEY"

[model]
model = "google/gemini-2.5-pro"
```

A local Ollama daemon, which needs no key:

```toml
[accounts.local]
kind = "ollama"
base_url = "http://localhost:11434"
context_tokens = 16384

[model]
model = "local/qwen3:4b"
```

Every request to an `ollama` account names its context window, because the daemon otherwise loads
a model with its own default (4096 tokens unless the Modelfile or `OLLAMA_CONTEXT_LENGTH` says
otherwise), which cannot hold the guardian's rules. `context_tokens` replaces that setting, so a
user who raised it to 32768 in Ollama gets 16384 here until they set `context_tokens`. A larger
window costs GPU memory, since the KV cache grows linearly with it, and can push a model onto the
CPU on a small card. A prompt longer than the window is refused, not truncated: Fontaine asks the
daemon not to cut it, so the guardian denies the call with the daemon's message ("exceeds the
available context size ... try increasing it") and the main agent shows that error. Raise
`context_tokens` to fit the prompt. For a model in rig's catalog, compaction uses this window, starting
when the conversation fills three quarters of a window under about 57k tokens; a
model the catalog does not list never compacts by threshold, so a long session ends with that
error at the window.

OpenRouter, with prompt caching (an `openai-compatible` account pointed at OpenRouter
still works, but sends no cache markers):

```toml
[accounts.router]
kind = "openrouter"
api_key_env = "OPENROUTER_API_KEY"
models = ["anthropic/claude-sonnet-4-6"]

[model]
model = "router/anthropic/claude-sonnet-4-6"
```

LM Studio:

```toml
[accounts.lmstudio]
kind = "openai-compatible"
base_url = "http://localhost:1234/v1"

[model]
model = "lmstudio/qwen2.5-coder-7b-instruct"
```

vLLM, with an API key if the server was started with one:

```toml
[accounts.vllm]
kind = "openai-compatible"
base_url = "http://localhost:8000/v1"
api_key_env = "VLLM_API_KEY"

[model]
model = "vllm/Qwen/Qwen2.5-Coder-32B-Instruct"
```

LiteLLM proxy:

```toml
[accounts.litellm]
kind = "openai-compatible"
base_url = "http://localhost:4000"
api_key_env = "LITELLM_API_KEY"

[model]
model = "litellm/gpt-4o-mini"
```

OpenCode Go, one subscription serving about thirty models on four wire formats:

```toml
[accounts.go]
kind = "opencode-go"
```

Sign in once with `/login go`, which shows a URL and a code to approve in a browser, then pick a
model with `--model go/kimi-k3`. Each request goes to the wire its model is served on,
chosen from a built-in table by model id: `gpt-`, `grok-` and `muse-spark-` models use the Responses
API, `claude-`, `qwen` and `minimax-m3` use the Messages API, `gemini-` models use `generateContent`
and everything else uses Chat Completions. Every request carries a `fontaine/<version>`
`User-Agent` and the session id in `x-opencode-session`. The access token is refreshed silently.

A model the table gets wrong is routed with `wires`:

```toml
[accounts.go]
kind = "opencode-go"

[accounts.go.wires]
"some-new-model" = "messages"
```

Go usage is billed against the subscription's allowances, so its models are unpriced: the cost Go
reports, including Extra usage, is not shown or counted, and `--max-budget-usd` with a Go main
model is a usage error (exit code 2).

Claude Code CLI as a subscription transport (experimental; see
[Claude subscription](claude-subscription.md) for scope, checks and limits):

```toml
[accounts.claude]
kind = "claude-subscription"
```

Sign in with `claude auth login` outside Fontaine, then pick a model with `--model claude/sonnet`.
Usage is subscription usage: tokens are shown, a cost never is, and `--max-budget-usd` with such a
main model is a usage error (exit code 2).

### `[network]`

Proxy, extra CA roots and a client certificate for the HTTP clients Fontaine builds: model
accounts, the guardian and its screen, `webfetch`, the scanner-model download, the `opencode-go`
sign-in, `websearch` and HTTP MCP servers. User file and `--settings` only. Stdio MCP servers, shell commands
and [telemetry](#telemetry) exporters are not covered.

| Key | Meaning | Default |
|---|---|---|
| `https_proxy` | The proxy for `https` requests, such as `http://proxy.corp:8080`. It may carry credentials (`http://user:pass@host:port`). | unset |
| `http_proxy` | The proxy for plain `http` requests. | unset |
| `no_proxy` | Hosts that bypass the proxies, in `NO_PROXY` syntax (`localhost,.corp.internal`). Needs `https_proxy` or `http_proxy`. | unset |
| `ca_bundle` | A PEM file of one or more CA certificates, trusted in addition to the system roots. | unset |
| `client_cert` | A PEM certificate chain presented to servers that ask for a client certificate (mTLS). Needs `client_key`. | unset |
| `client_key` | The matching unencrypted PEM private key. A passphrase-protected key is an error. Needs `client_cert`. | unset |

With no proxy key set, Fontaine honors the `HTTPS_PROXY`, `HTTP_PROXY`, `ALL_PROXY` and `NO_PROXY`
environment variables (and the system proxy settings on macOS and Windows), as before. Setting
`https_proxy` or `http_proxy` turns all of that off for the clients concerned. With only
`https_proxy`, plain `http` requests go direct; with only `http_proxy`, `https` requests go direct.
An `ollama` account on `localhost` therefore goes direct under `https_proxy` alone, but through the
proxy under `http_proxy` unless `no_proxy` lists it. `SSL_CERT_FILE` and `SSL_CERT_DIR` replace the
system roots; `ca_bundle` adds to them.

Relative `ca_bundle`, `client_cert` and `client_key` paths resolve against the project directory
(the launch directory, or `--cd`), and `~` is not expanded.

Configuration checking is in two tiers. Reading a settings file checks only that the keys are known,
allowed in that scope and not empty, and never opens the files, so the example paths below pass
documentation checks. The files are read when the configuration is loaded at startup, where any
problem (a missing or non-PEM file, a certificate without a key, a `no_proxy` with no proxy, an
unparseable proxy URL) is a configuration error that names the key and exits with code 2. Nothing
falls back to a direct or default-trust connection.

Each account may repeat any of these keys to override the `[network]` value for itself. Overrides
apply per key, so an account that sets only `ca_bundle` keeps the proxy from `[network]`.

```toml
[network]
https_proxy = "http://proxy.corp.example:8080"
no_proxy    = "localhost,.corp.example"
ca_bundle   = "/etc/corp/ca.pem"

[accounts.work]
kind        = "anthropic"
api_key_env = "ANTHROPIC_API_KEY"
client_cert = "/etc/corp/client.pem"
client_key  = "/etc/corp/client.key"
```

### `[websearch]`

Where the `websearch` tool sends its queries, and whether the provider's own search is used. User
file and `--settings` only, since it decides where queries go, which key is used and which command
runs. See [Web search](tools.md#web-search) for how the two ways of searching are chosen.

| Key | Meaning | Default |
|---|---|---|
| `backend` | `tavily`, `searxng` or `off`. Unset: a SearXNG container Fontaine starts itself when `docker` or `podman` is installed (see [Default search](tools.md#default-search)). `off` disables the client-side `websearch` tool. | unset |
| `base_url` | The SearXNG instance, an `http` or `https` URL. Required for `searxng`; refused for `tavily`. | unset |
| `command` | `searxng` only: the argv (no shell) that starts the instance serving `base_url`, such as `["docker", "run", "--rm", "-p", "127.0.0.1:8080:8080", "searxng/searxng"]`. Started by the first search with a minimal environment and stopped with the session (SIGTERM to its process group, then SIGKILL after 10 seconds from a background task, so a session that ends sooner can leave a process that ignores SIGTERM running). | unset: Fontaine does not start it |
| `startup_timeout_secs` | How long to wait for `base_url` to answer after starting `command`. Needs `command`. | `60` |
| `native` | `auto` uses the provider's own search where supported (`anthropic` accounts on the Anthropic API); `on` also uses it for other hosts of a supported kind; `off` never does. | `auto` |
| `api_key_env` | `tavily` only: the environment variable holding the key. Omit it to use the stored credential. | unset |
| `max_results` | Results returned, 1 to 20. | `10` |
| `timeout_secs` | Request timeout. | `30` |

Queries are sent to the provider, or to the configured backend, in the clear. A `tavily` backend
needs a key, and a missing key is a configuration error (exit code 2) at startup, not at the first
search. The key is `api_key_env` when that is set; if the named variable is unset or empty the key
counts as missing. When it is omitted, the key is the `[websearch]` entry in `credentials.toml`.
Keys are never written inline in settings. With `backend` unset, the managed container gets the minimal child environment plus
`DOCKER_HOST`, `DOCKER_CONFIG`, `XDG_RUNTIME_DIR` and `CONTAINER_HOST` when they are set (the runtime needs them to
reach its daemon), a 300 second start timeout for the first image pull, and a command that is fixed by Fontaine and a generated container name (`fontaine-searxng-<random>`) removed by a detached `rm -f` when the session ends. `command` is an executable like an MCP server or a hook
command, but it can only start the instance; it never loosens a permission decision.

```toml
[websearch]
backend = "tavily"
api_key_env = "TAVILY_API_KEY"
max_results = 8
```

### `[model]`

| Key | Meaning | Default |
|---|---|---|
| `model` | The session model as `<account>/<model>`. `--model` overrides it. There is no default model: a run with neither is a configuration error (exit code 2) that names both. | unset |
| `effort` | The main agent's reasoning effort: `off`, `low`, `medium`, `high` or `max`. `--effort` overrides it and `/effort` changes it for a session. Fontaine passes it through rig's portable setting, which maps it onto the provider's own parameter. Unset sends no reasoning parameter, so the model thinks at its own default. A level the model or its endpoint cannot take (a non-reasoning model, a level the catalog does not list for it, or a generic `openai-compatible` gateway, which has no known mapping) is left out of the request with a one-time warning, and the run goes on. Subagents use it too, unless their type sets its own. The guardian is not affected: it always runs with thinking off. Not settable from the shared project file. | unset |

### `[guardian]`

Keys of the guardian that judges tool calls in `auto` mode and in the consequential branch of
`plan` mode. [The guardian](guardian.md) explains what they do.

| Key | Meaning | Default |
|---|---|---|
| `model` | The guardian model as `<account>/<model>`; it may use a different account than `[model]`. A model named here is pinned: it is never substituted when it is unavailable. | unset: the top-ranked validated model your accounts offer, else the main model |
| `profile` | `full` or `minimal`. `minimal` narrows the guardian for containers and only loads when `--in-container` is given and a runtime probe finds a container; otherwise `full` runs, with a notice. It never turns the guardian off. | `full` |
| `timeout_secs` | Time allowed for one verdict, retries included; at least 1. An attempt that stalls is dropped and retried within it, and on expiry the call is denied. The first attempt may use all but 10 seconds of it (for timeouts of 15 seconds or more; each retry gets at least 5 seconds when that much remains), so a slow model needs more than its worst answer time plus 10. | `30` |
| `confidence_threshold` | Between 0 and 1. An allow below it on a destructive call is turned into a deny. | `0.7` |
| `cache` | Whether verdicts are cached for 60 seconds. | `true` |
| `scanner` | Tool-result scanner: `off`, `pg2`, `llm` or `command`; it is the only switch. User file only. [Scanning tool results](guardian.md#scanning-tool-results) explains each. `pg2` needs a build with the `pg2` feature, which release binaries have; elsewhere it is a configuration error. | `off` |
| `scanner_command` | Program used by the `command` scanner: required with `scanner = "command"`. User file only. | unset |
| `scanner_model_path` | Directory holding `model.safetensors`, `tokenizer.json` and `config.json` for the `pg2` scanner. With it set, nothing is downloaded. User file only. | unset: downloaded into `<data dir>/models/promptguard2-86m/` |
| `scanner_hf_token_env` | Name of the environment variable holding the Hugging Face token the `pg2` download uses. The variable is always removed from `shell` and hook environments, whatever `scanner` is. User file only. | `HF_TOKEN` |

The `llm` scanner uses the guardian's model, so it follows `model`. `timeout_secs` bounds each scan
of every scanner type (the `llm` scanner retries a stalled attempt within it), and the `pg2` model load at startup.

#### `[guardian.composition]`

How the guardian is composed from the LLM guardian and cheap local reads in `auto` and `plan`
mode: a built-in preset or a Rune script. With the `screen` preset it is an allow screen in front
of the guardian: a small model, local (Ollama, yes/no questions) or hosted (Jev on OpenRouter), allows
a call that is clearly safe without waiting for the guardian, and escalates a call that is
confidently outside what you asked for (on a local model only if you set `stop_threshold`). Every
other call meets the guardian.
[Composing the guardian](guardian.md#composing-the-guardian) explains it, how to calibrate it and
how to write a script. The section is read from the user file only: other scopes may set
`shadow = true`, which holds over the user file's `shadow = false`, and nothing else, so a
repository cannot load a script. An invalid composition stops startup with exit code 2, naming the
key.

| Key | Meaning | Default |
|---|---|---|
| `preset` | A built-in preset: `screen`. Exactly one of `preset` and `script` is required. | unset |
| `script` | A Rune file. A relative path resolves against the user config directory. It must compile against the host functions and define `guardian` with two parameters. | unset |
| `model` | `<account>/<model>` of an `[accounts.*]` entry of kind `ollama` or `openrouter`. For `ollama`, its `base_url` is the daemon that is asked, without credentials, so the endpoint must not require authentication. For `openrouter` (`typesafe/jev-1.14`), the model is read through `<base_url>/systemone`, where `base_url` is the same one that serves chat (default `https://openrouter.ai/api/v1`), with the account's key (`api_key_env` or the one `/login` stored); the key is sent only over https or to a loopback host. A key that does not resolve stops startup with exit code 2, naming this key and the account. The default model of every read, and required with a preset: a hand-written section never starts sending guardian state to a hosted model by itself. The account's `context_tokens` is the window a read requests; Ollama reloads a model whenever the requested window changes, so a read and a guardian on the same model need the same value (the same account does). | unset |
| `shadow` | Run the composition and record every read, but let the guardian decide. | `true` |
| `timeout_ms` | Time allowed for each read; at least 1. On expiry the read fails. | `2000` |

`[guardian.composition.params]` holds the preset's parameters, or free-form numbers, strings and
booleans passed to a script as `params`. The `screen` preset takes:

| Param | Meaning | Default |
|---|---|---|
| `question` | Local model only (refused for a hosted one, which asks a single five-level question). The stop read's question: `employee_allowed` ("If an employee were given the user's request, would they be allowed to take this action?") or `outside_request` ("Is this action outside what the user's request asked for?"). A threshold is calibrated for one question. | `employee_allowed` |
| `stop_threshold` | The risk, greater than 0 and at most 1, at which a call is escalated. | local model: unset, the screen never escalates; hosted `typesafe/` model: `0.34`; another hosted model: unset |
| `allow_threshold` | Allow a call without the guardian when the allow read's risk is below this (and, on a local model, the steering score is below `steering_veto`). Greater than 0 and less than 1. On a local model or a hosted model that is not `typesafe/`, with `shadow = false`, at least one of `stop_threshold` and `allow_threshold` is required. | local model: unset, no allow path; hosted `typesafe/` model: `0.20`; another hosted model: unset |
| `allow` | Hosted model only: `false` switches the allow path off, and `allow_threshold` is then ignored. Refused on a local model. | `true` |
| `steering_veto` | Local model only: the steering score, greater than 0 and at most 1, at or above which the screen never allows a call. A hosted model has no steering read, and this is refused for it. | `0.001` |
| `concurrent` | Race the screen against the guardian instead of asking the guardian only for what the screen does not settle. A settled call then spends the guardian's input tokens. | `false` |

`[guardian.composition.reads.<name>]` declares a script's reads; a preset declares its own and
takes none. Each read has `model` (default: the composition's `model`), `prompt` (`plain` or
`hardened`) and `question` (`employee_allowed`, `outside_request` or `steering`). The hardened
prompt is valid only with `employee_allowed`, the one wording its threshold was fitted to.

```toml
[accounts.local]
kind = "ollama"

[guardian.composition]
preset = "screen"
model = "local/qwen3:4b-instruct-2507-q4_K_M"
shadow = false

[guardian.composition.params]
allow_threshold = 0.00000156
# stop_threshold = 0.0000599
# concurrent = true
```

The hosted Jev screen needs no threshold; its defaults apply (another hosted model needs its own). Where it is enabled, guardian state leaves
the machine for OpenRouter and Typesafe (see [the screen preset on a hosted
model](guardian.md#the-screen-preset-on-a-hosted-model)):

```toml
[accounts.openrouter]
kind = "openrouter"
api_key_env = "OPENROUTER_API_KEY"

[guardian.composition]
preset = "screen"
model = "openrouter/typesafe/jev-1.14"
shadow = false

# [guardian.composition.params]
# allow = false            # keep the stop path, switch the allow path off
```

With no `shadow` key a hand-written section stays in shadow mode; `fontaine auto-mode setup` is
what writes `shadow = false`.

The model's answers are nearly certain, so its risk scores are tiny or near 1, and a working
threshold is a small number. `0.0000599` and `0.00000156` are the ones measured in
[Cascade scorecards](evals.md#cascade-scorecards). Calibrate your own from the scores that shadow
mode records on your traffic; `allow_threshold` in particular is fitted to one model, one prompt
and one question, and a wrong value lets the screen allow calls the guardian would not have.

#### Bring your own scanner

`scanner = "command"` runs the program at `scanner_command` once per tool result, with no arguments
and no shell, and the same scrubbed environment as hooks, without `[env]`. It receives one JSON object on standard
input and answers with one on standard output:

```json
{"tool": "webfetch", "content": "the text of the tool result"}
```

```json
{"verdict": "flagged", "reason": "asks the agent to send ~/.ssh to a host"}
```

`verdict` is `clean` or `flagged`; `reason` is optional text shown to the agent. A non-zero exit, no
answer within `timeout_secs`, a program that cannot be started, or output that is not exactly this
shape counts as a failed scan: the result is marked unscanned and suspect.

```toml
[guardian]
model = "work/claude-haiku-5-5"
profile = "full"
timeout_secs = 30
confidence_threshold = 0.7
cache = true
scanner = "off"
```

```toml
[guardian]
scanner = "command"
scanner_command = "/usr/local/bin/injection-scan"
```

`/model` and `/guardian-model` in the terminal UI write `[model] model` and `[guardian] model` to
the user file for you. When a higher scope also sets the key, the choice is still saved to the user
file, and the notice names the scope that wins from the next launch.

### `[statusline]`

A user-defined command that draws the terminal UI status bar in place of the built-in hint
segment. It runs arbitrary code, so it is accepted from the user file only: project,
project-local and `--settings` files are refused.

| Key | Meaning | Default |
|---|---|---|
| `command` | The shell command; when absent or empty the feature is off. | unset |
| `refresh_interval_secs` | Re-run the command while idle, at least 1; requires `command`. | unset |

The command receives one JSON object on stdin and its first stdout line (ANSI escapes stripped)
is shown in the bar. It is killed after 5 seconds, and any non-zero exit, timeout or empty output
keeps the previous good line. See [Custom status line](tui.md#custom-status-line) for the full JSON
contract.

```toml
[statusline]
command = "jq -r '.mode'"
refresh_interval_secs = 10
```

### `[permissions]`

Rule strings use the `Tool` or `Tool(specifier)` form described in
[Permissions](permissions.md#rules). At load, Fontaine checks the outer shape, that the tool is one
of `shell`, `read`, `edit`, `write`, `notebook_edit`, `ls`, `glob`, `grep`, `webfetch`, `websearch`, `agent`, `skill`,
`monitor`, `lsp`, `powershell`, `job_output`, `job_cancel`, `send_message`, `ask_user_question`, `todo_write`, `models`, `cron_create`, `cron_jobs`, `cron_delete`, `schedule_wakeup` or an `mcp__<server>[__<tool>|__*]` name, and that the specifier is valid for the
tool (a `shell` specifier is literal command words, a path specifier a gitignore-style pattern, a
`webfetch` specifier `domain:<host>` or `domain:*.<host>`). A bad rule is a configuration error.
`deny` and `ask` rules apply in every mode; `allow` rules act after the floor.

| Key | Meaning | Default |
|---|---|---|
| `default_mode` | `manual`, `acceptEdits`, `plan`, `dontAsk`, `auto` or `afk`. Applies to both surfaces. | unset: the terminal UI starts in `auto`, a headless run in `manual` |
| `deny` | Rules that always deny. | `[]` |
| `ask` | Rules that always prompt. | `[]` |
| `allow` | Pre-approval rules. User-trust scopes only. | `[]` |
| `additional_directories` | Extra workspace roots. User-trust scopes only. See [Workspace roots](#workspace-roots). | `[]` |
| `auto_tracked_edits` | In `auto`, edits to git-tracked files, new files and files the session created inside the primary root skip the guardian (after the floor). Project scope may only set `false`. | `true` |
| `auto_scratch_writes` | In `auto`, file-tool writes into the session scratch directory skip the guardian (after the floor). Project scope may only set `false`. | `true` |

`default_mode = "auto"` and `"afk"` load only from the user file and `--settings`. It is rejected from the shared
project file and from `settings.local.toml`, trusted or not, because it loosens: those scopes accept
only `manual` and `dontAsk`.

```toml
[permissions]
default_mode = "manual"
deny = ["shell(rm *)", "read(.env)"]
ask = ["webfetch(domain:*.example.com)"]
allow = ["shell(cargo test)"]
additional_directories = ["/home/me/shared-libs"]
```

### Workspace roots

The workspace is an ordered set of roots: the primary root, then every `--add-dir <path>` and every
`additional_directories` entry. File tools *write* a path that lies inside any root, and `shell`
working directories stay inside them; every file tool — the four read-only ones (`read`, `ls`,
`glob`, `grep`) and the write tools (`write`, `edit`, `notebook_edit`) — may also name a path
outside the roots, each call adjudicated through the normal permission decision: it asks in the
asking modes, is judged by the guardian in `auto` (an un-asked external write is denied; see
[Guardian](guardian.md#writing-outside-the-workspace)) and is denied in `dontAsk` and headless
runs unless an allow rule or hook already covers it (see
[Permissions](permissions.md#the-pipeline)). A relative entry
is resolved against the project directory (the launch directory, or `--cd`). An entry that does not exist, is not a directory or
cannot be resolved is left out, with a warning naming the path and whether it came from
`--add-dir` or the settings, and the run continues. The terminal UI shows the warning in the
conversation; a headless run prints it on stderr.

Each session also gets a private scratch directory as a root of its own, with no setting to add or move
it (in `auto`, file-tool writes into it skip the guardian; see `auto_scratch_writes` above): `$XDG_RUNTIME_DIR/fontaine/<session id>/` where that is available, else a per-user directory
under the temp dir (`$TMPDIR` or `/tmp`; `%TMP%`/`%TEMP%` on Windows). It is removed when the session ends
and swept after a crash. For `shell` it is treated like any root: read-only commands whose operands
are inside it take the pre-classifier fast path, and other commands go through the guardian as usual. See
[the scratch directory](tools.md#the-scratch-directory). Tests and sandboxes redirect it by
setting `XDG_RUNTIME_DIR` (`TMP` on Windows).

Roots grant file access, not configuration: settings, hooks, MCP servers and extension files load
from the primary root only. A `.fontaine/settings.toml` in an added root is not read. See
[Tools](tools.md#workspace-roots).

### `[[hooks.pre_tool_use]]`, `[[hooks.post_tool_use]]`, `[[hooks.permission_denied]]`, `[[hooks.task_completed]]`, `[[hooks.teammate_idle]]`

[User hooks](hooks.md): programs that look at a tool call before it runs, after it ran and when it
was denied, and, with [task boards](agents.md#task-boards) on, before a task is completed
(`task_completed`) and when an agent's turn ends with a task in progress on its parent's board
(`teammate_idle`). The two board events take no
`matcher`: one that sets it is a config error.

| Key | Meaning | Default |
|---|---|---|
| `matcher` | A rule in the `Tool(specifier)` form; the hook runs for the calls it covers. A hook without one runs for every call and counts as broad. | none |
| `command` | The command line, run by the shell. Required. | none |
| `timeout_secs` | At least 1. | `30` |

```toml
[[hooks.pre_tool_use]]
matcher = "shell(git *)"
command = "/home/me/.config/fontaine/hooks/git-guard.sh"
timeout_secs = 10
```

Hooks the repository defines start only after [workspace trust](#workspace-trust) in the terminal
UI, and their `allow` is ignored. A repository's `teammate_idle` hook is never run. See [User hooks](hooks.md#hooks-a-repository-defines).

### `[mcp.servers.<name>]`

[MCP servers](mcp.md). Set exactly one of `command` (a stdio server) or `url` (streamable HTTP).
`args` and `env_vars` apply to stdio servers only. `env_vars` lists variable names to pass
through; values never appear in configuration, and the rest of Fontaine's environment is not passed.
Tools register as `mcp__<server>__<tool>`, always as `arbitrary` tools. A server a repository
defines starts only after workspace trust and your approval of that server in the terminal UI.

```toml
[mcp.servers.github]
command = "github-mcp-server"
args = ["stdio"]
env_vars = ["GITHUB_TOKEN"]

[mcp.servers.docs]
url = "https://mcp.example.com/docs"
```

### `[lsp.servers.<name>]`

A [language server](tools.md#language-servers) for the `lsp` tool and the diagnostics note on
edits. User file or `--settings` only: a server runs on your machine on every edit, so no
repository, project-local file or trust prompt can add one. Names use the MCP server alphabet.
An invalid entry is a configuration error that names it.

| Key | Meaning | Default |
|---|---|---|
| `command` | The program to start; required. | – |
| `args` | Its arguments. | `[]` |
| `extension_to_language` | Extension, with its dot, to the language id sent in `didOpen`; required, at least one entry. | – |
| `env_vars` | Parent variable names to pass through. | `[]` |
| `env` | Literal variables set on the server. | `{}` |
| `initialization_options` | Sent in `initialize`. | unset |
| `settings` | Sent by `workspace/didChangeConfiguration` and answered to `workspace/configuration`. | unset |
| `startup_timeout_ms` | How long the handshake may take. | `30000` |
| `shutdown_timeout_ms` | How long `shutdown` and `exit` may take before the server is killed. | `2000` |
| `request_timeout_ms` | How long one request may take. | `60000` |
| `restart_on_crash` | Restart a server that crashed after it was running. | `true` |
| `max_restarts` | Restarts allowed for the session. | `3` |
| `diagnostics` | Add the diagnostics note to edits of the files it handles. | `true` |

The server starts with the same environment as an MCP stdio server: a small base set of
variables, the ones `env_vars` names, the `[env]` table and its own `env`; its working directory
is the primary root, and it gets the workspace roots as workspace folders. `/add-dir` tells a
running server that supports it.

```toml
[lsp.servers.rust-analyzer]
command = "rust-analyzer"
extension_to_language = { ".rs" = "rust" }
env_vars = ["RUSTUP_HOME", "CARGO_HOME"]
```

### `[auto_mode]`

Prose lists for the guardian: `environment` (what is trusted and sensitive in your setup),
`allow` (exceptions that clear a soft-deny match), `soft_deny` and `hard_deny` (see
[rule precedence](guardian.md#rule-precedence)). The marker `$defaults` splices the shipped
defaults in where it stands.

- `environment` and `allow`: unset or empty gives the shipped defaults; a list with `$defaults`
  has them spliced in at that position; any other list replaces them.
- `soft_deny` and `hard_deny`: the shipped entries are always present, because a shared project
  file may add to these lists and must not be able to replace the defaults by adding. A list
  without `$defaults` has them placed first.

`environment` and `allow` loosen, so they come from user-trust scopes only; `soft_deny` and
`hard_deny` can be added to from any scope. Under the `minimal` profile only `environment` is used.

| Key | Meaning | Default |
|---|---|---|
| `setup_reminder` | Whether an `auto` session whose project has no stored [environment facts](guardian.md#project-environment) warns to run `fontaine auto-mode setup`. User-trust scopes only: a shared project file cannot set it. | `true` |

`fontaine auto-mode config` prints the lists a session would use and which scopes set them, and
the project's stored environment record, or `none — run fontaine auto-mode setup`;
`fontaine auto-mode setup` collects that record and can write
`[guardian.composition]` to the user file after a confirmation. See
[Inspecting the rules](guardian.md#inspecting-the-rules).

```toml
[auto_mode]
environment = ["$defaults", "Source control: github.com/myorg"]
allow = ["$defaults", "Deploying to the staging namespace is allowed: it resets nightly"]
hard_deny = ["$defaults", "Never send repo contents to third-party code-review APIs"]
```

### `[agents]`

[Subagent](agents.md) limits and agent types.

| Key | Meaning | Default |
|---|---|---|
| `max_parallel` | Subagents running at once across the whole session, at any depth; at least 1. A spawn past it is refused, not queued. | `4` |
| `subagent_token_budget` | Opt-in per-subagent token cap, the guardian's calls for it included. A subagent that passes it stops and returns a partial report. Unset means uncapped. | unset |
| `subagent_cost_budget_usd` | Opt-in per-subagent spend cap. Inactive, with a one-time notice, on a model with no known price; a token cap, if set, still binds. Unset means uncapped. | unset |
| `session_token_budget` | A token ceiling that every agent in the tree debits, when set. | unset |
| `session_cost_budget_usd` | The same in dollars, when set. | unset |
| `teams` | Turns on the [task boards](agents.md#task-boards): the `board_show` and `board_update` tools, the owner reminders and the `task_completed` and `teammate_idle` hooks. Set to `false`, the board tools are not registered. It does not control idling: every subagent goes idle when its turn ends and can be woken. Like the other `[agents]` keys it cannot be set from the repository's shared settings. | `true` |

`[agents.types.<name>]` defines an agent type with `model` (`$main`, `$guardian_default` or
`<account>/<model>`), `tools` (built-in or `mcp__` tool names), `effort` (`off`, `low`, `medium`,
`high` or `max`; unset inherits the session's level, and a subagent may run above or below its
parent because effort is not a permission) and `mode`. `mode` accepts only
`inherit` (the default: the parent's mode), `manual` or `dontAsk`; any other value is an error, so
a subagent is never looser than its parent. A type's toolset is cut down to what its parent has.
Agent files in `agents/` define types too; see [Extensions](extensions.md#agent-files).

```toml
[agents]
max_parallel = 4

[agents.types.reviewer]
model = "work/claude-sonnet-5"
tools = ["ls", "read", "glob", "grep"]
mode = "inherit"
```

### `[context]`

| Key | Meaning | Default |
|---|---|---|
| `autocompact_window` | A token count. Before a turn, the conversation is summarized once its messages pass it. The model's own limit still applies, so the effective trigger is the lower of this and the model's window less a 16,384-token reserve. At least 1. | unset |

Unset, compaction runs when the model's window less the reserve is passed, and only for a model
whose window the catalog knows. Set, it also runs for a catalog-listed model whose window is not
known. A model the catalog does not list (a gateway or local alias) is never compacted
automatically, with or without the setting; `/compact` cannot summarize with it either. A small
value also shrinks what a compaction keeps: the newest messages it leaves as they are and the
summary it asks for are each capped at a quarter of the value, so the conversation ends under it.
It applies to [headless runs](headless.md) too. See [Context](tui.md#context) for `/compact` and
`/context`.

```toml
[context]
autocompact_window = 150000
```

### `[schedule]`

| Key | Meaning | Default |
|---|---|---|
| `enabled` | Whether the [scheduling tools and `/loop`](tui.md#scheduled-prompts) are available in the terminal UI. When `false` the tools are not registered, `/loop` says scheduling is off, and a schedule saved by an earlier session is left on disk and never runs. Only the user file and `--settings` can set it. | `true` |

```toml
[schedule]
enabled = false
```

### `[memory]`

| Key | Meaning | Default |
|---|---|---|
| `auto` | Whether the agent may save notes to the [per-project memory store](memory.md) and the store's index loads into each session. A change applies at the next launch. Only the user file and `--settings` can set it. | `true` |

```toml
[memory]
auto = false
```

### `[goal]`

| Key | Meaning | Default |
|---|---|---|
| `max_turns` | Goal turns in one unattended stretch, 1 to 1000. Reaching it [pauses the goal](tui.md#goals); your next message starts a fresh stretch. | `50` |
| `max_cost_usd` | The stretch's spend (the main model, compaction, the guardian, the goal evaluator and subagents) that pauses the goal, zero or more. Inactive for a model with no known price. | unset |

A goal turn runs with nobody typing, so only the user file and `--settings` can set these. The
no-progress cap (8 verdicts in a row after turns that called no tool) is fixed.

```toml
[goal]
max_turns = 50
max_cost_usd = 5.0
```

### `[afk]`

| Key | Meaning | Default |
|---|---|---|
| `temperature_ceiling` | The [session temperature](afk.md#the-session-temperature) at which an AFK stretch is stopped. A finite number above zero. | `12.0` |

The user file and `--settings` set the value, and `--afk-temperature-ceiling` overrides both for one
session. A shared project file or a project-local file may only *lower* it: the lowest value any of
them sets caps whatever the user, the command line or the default gave, whichever file is read
first. A value that is zero, negative, infinite or not a number is a configuration error in every
scope, so no scope can switch the stop off. Nothing in a session can change it: the model has no
tool for it, and edits to settings files are a protected-path and Safety Bypass case.

```toml
[afk]
temperature_ceiling = 8.0
```

### `[shell]`

| Key | Meaning | Default |
|---|---|---|
| `max_timeout_secs` | The longest `timeout_secs` a model's [`shell`](tools.md#shell) call may ask for, 1 to 86400. In the main conversation it is how long a call blocks before the command is moved to a background job; for a subagent or a skill's shell injection it is how long the command runs before it is stopped. | `600` |

A repository must not stretch how long commands run, so only the user file and `--settings` can
set it. No tool argument lifts it. The `!` mode keeps its own 120 second default.

```toml
[shell]
max_timeout_secs = 1800
```

### `[checkpoints]`

| Key | Meaning | Default |
|---|---|---|
| `enabled` | Whether `write` and `edit` save a file's contents before changing it, for [`/rewind`](checkpoints.md). When `false` a rewind can restore the conversation but not the code. | `true` |
| `max_file_bytes` | A file larger than this is not saved; the edit goes ahead and the rewind lists the file as not restorable. At least 1. | `10485760` (10 MiB) |
| `max_session_bytes` | Once the saved copies of a session reach this size, later files are not saved. At least 1. | `268435456` (256 MiB) |

Only the user file can set these; neither a repository nor `--settings` can.

```toml
[checkpoints]
max_file_bytes = 5242880
```

### `[plugins]`

| Key | Meaning | Default |
|---|---|---|
| `auto_update` | Whether registered marketplaces and installed plugins update themselves in the background when the terminal UI starts (see [Auto-update](plugins.md#auto-update)). A plugin revision that changes its hooks, servers, skill `!` commands or scripts is held for `fontaine plugin update` either way. Headless runs never auto-update. | `true` |

Updating changes code you approved at install, so only the user file can set it; a repository and
`--settings` cannot. One marketplace or plugin can opt out on its own with `fontaine plugin
marketplace auto-update <name> no` or `fontaine plugin auto-update <name> no`.

```toml
[plugins]
auto_update = false
```

### `[shell_preclassifier]`

| Key | Meaning | Default |
|---|---|---|
| `allow_extra` | Extra read-only commands for the [shell pre-classifier](permissions.md#the-shell-pre-classifier). Each entry is a command name or a `command subcommand` pair with no flags; any flag on the call falls through, and positional words must be in-scope paths. User-trust scopes only. | `[]` |

### `[telemetry]`

Telemetry is off by default.

| Key | Meaning | Default |
|---|---|---|
| `enabled` | Turns export on. | `false` |
| `backend` | `micromegas` or `otlp`. | `micromegas` |
| `endpoint` | The ingestion URL. Required when enabled; there is no default. | unset |
| `api_key_env` | The name of the environment variable holding the ingestion key. Omit it to use the stored credential. | unset |
| `traces` | Export spans. | `true` |
| `log_user_prompts`, `log_assistant_responses`, `log_tool_details`, `log_tool_content` | Content opt-ins. | `false` |
| `content_max_bytes` | Truncation limit for opted-in content. | `61440` |
| `resource_attributes` | Static tags attached to everything exported. | `{}` |

Enabling telemetry requires both the ingestion URL and an ingestion key. Setting `enabled = true`
without an `endpoint`, or without a key, is a configuration error and Fontaine exits with code 2.
The key is `api_key_env` when that is set; if the named variable is unset or empty the key counts as
missing. When `api_key_env` is omitted, the key is the `[telemetry]` entry in `credentials.toml`.
Keys are never written inline in settings.

Project scopes can only set `enabled = false`. What is exported, the two backends and the privacy
defaults are described in [Telemetry](telemetry.md).

```toml
[telemetry]
enabled = true
backend = "micromegas"
endpoint = "https://telemetry.example.com"
api_key_env = "MICROMEGAS_INGESTION_API_KEY"
resource_attributes = { "team.id" = "platform" }
```

### `[env]`

Environment variables set on every process a tool or extension starts: the `shell` tool, `!`
commands, [hooks](hooks.md) and stdio [MCP servers](mcp.md). Each key is a variable name and each
value a string.

```toml
[env]
HTTPS_PROXY = "http://proxy.internal:3128"
PATH = "/home/me/.local/bin:/usr/local/bin:/usr/bin:/bin"
RUST_LOG = "warn"
```

- Values are literal: `$VAR` and `~` are not expanded, so `PATH = "/opt/bin:$PATH"` sets `PATH` to
  exactly that text. Write the whole value out.
- The entries are applied after the scrub of credential variables (see [Tools](tools.md#shell)),
  so an entry may deliberately set a scrubbed name. They are applied before the variables Fontaine
  sets itself.
- Names starting with `FONTAINE_` (in any case) are reserved and are an error. A name may not be
  empty or contain `=`, and neither names nor values may contain a NUL character. Values must be
  strings; a number or a boolean is an error.
- Scopes merge per key and the higher scope wins; on Windows names are compared without regard to
  case, so `Path` in `--settings` replaces `PATH` in the user file. There is no way to unset a
  variable.
- Values are never logged and never appear in a configuration error, and the settings debug output
  hides them. The [guardian](guardian.md) is told the names that are set, never the values.
- Fontaine's own process is not affected: provider, telemetry and scanner-command lookups keep
  using the variables they name, and neither the `scanner = "command"` program nor the `[websearch] command` SearXNG launch gets `[env]`. The
  SearXNG child's environment is deliberately the minimal one; pass it variables inline in the
  argv (for `docker run`, `-e NAME=value`).
- User-trust scopes only (see [Trust asymmetry](#trust-asymmetry)).

### `[tui]`

Terminal preferences. They are personal, so only your user file and `--settings` may set them; a
project or project-local file that has a `[tui]` table is an error.

| Key | Meaning | Default |
|---|---|---|
| `theme` | The colour theme: `dark-ansi`, `light-ansi`, `dark`, `light`, `dark-daltonized` or `light-daltonized`. `/theme` writes it. See [Terminal setup](terminal.md#colours). | `dark-ansi` |
| `screen_reader` | Turns on screen-reader mode. `--ax-screen-reader` turns it on and `FONTAINE_AX_SCREEN_READER=0` keeps it off over this setting. See [Terminal setup](terminal.md#screen-reader-mode). | `false` |
| `show_reasoning` | Shows the model's reasoning in the conversation: it streams live, then folds to a few rows once it settles, and `/expand thinking` prints it in full. `false` hides it. See [Terminal UI](tui.md#model-reasoning). | `true` |
| `notify` | How a prompt that waits for your answer (an approval, a question, an MCP server or skill prompt, the guardian-unavailable prompt) signals the terminal: `bell` (the default), `osc9` or `osc777` (a desktop notification, on terminals that support the sequence) or `off`. Screen-reader mode keeps its own bell. See [Notifications](terminal.md#notifications). | `bell` |
| `editor_mode` | How the input box edits text: `emacs` (readline-style, the default) or `vim` (modal INSERT/NORMAL; see [Vim input mode](tui.md#vim-input-mode)). `/vim` toggles it live and saves the choice. | `emacs` |

```toml
[tui]
theme = "dark"
screen_reader = false
show_reasoning = true
editor_mode = "vim"
notify = "osc9"
```

### `[keybindings]`

Maps keystrokes onto the TUI's global actions, so the keys above the input do what you want. It
is personal configuration: only your user file and `--settings` may set it, a `[keybindings]`
table in a project file is an error, and an unknown action, an unknown key or a rebind of a
reserved key fails to load with exit code 2 naming the entry.

Each entry is `"<keystroke>" = "<action>"`; the empty string unbinds the keystroke:

```toml
[keybindings]
"ctrl+g" = "todos"   # this key now opens the task list…
"ctrl+t" = ""       # …and this one does nothing
"f2"     = "cycle_mode"
```

A keystroke is modifiers (`ctrl`, `alt` — `meta` is an alias — and `shift`) then a key name:
`enter`, `tab`, `backtab`, `esc`, `up`, `down`, `left`, `right`, `home`, `end`, `pageup`,
`pagedown`, `backspace`, `delete`, `insert`, `space`, `plus` or a single character, `f1`–`f24`
included; names read case-insensitively (`Ctrl+Shift+T` and `ctrl+shift+t` are the same key).
One keystroke per entry — chords (space-separated sequences) are not supported. Binding a
keystroke that already triggers an action replaces that action's use of the key; several keys may
trigger one action. A bare printable key (such as `"a"`) bound to an action can no longer be
typed into the input. Changes apply on the next launch; `/keybindings` prints the effective table
and the file to edit.

`ctrl+c` (`interrupt`) and `ctrl+d` (`quit`) are reserved: they cannot be rebound or unbound, so
interrupting and quitting always work. Terminal-aliased keys are worth knowing about: many
terminals deliver Tab for Ctrl+I, Enter for Ctrl+M, Backspace for Ctrl+H and Esc for Ctrl+[.

The actions and their default keystrokes:

| Action | Default | What it does |
|---|---|---|
| `cycle_mode` | `shift+tab` | Cycle the permission mode; the cycle stays manual → plan → accept edits → auto → AFK → manual, no matter the key. |
| `interrupt` | `ctrl+c` (reserved) | Interrupt a running turn; quit when idle. |
| `quit` | `ctrl+d` (reserved) | Quit when the input is empty; with text it deletes the character under the cursor. |
| `agents_panel` | none | Open or close the agents panel; `/agents` opens it without a key. |
| `todos` | `ctrl+t` | Expand or collapse the task list. |
| `transcript` | `ctrl+o` | Open or close the transcript view. |
| `reprint` | `ctrl+l` | Redraw the screen from the transcript. |
| `attach_clipboard` | `ctrl+v` | Attach the clipboard's image. |
| `history_search` | `ctrl+r` | Search the prompt history. |
| `history_previous` | `up` | Recall the previous input on the input's first line. |
| `history_next` | `down` | Walk back forward while recalling; both also move the cursor inside a multi-line input. |
| `submit` | `enter` | Send the message. |
| `newline` | `shift+enter`, `alt+enter`, `ctrl+j` | Insert a newline. |

Only the global actions are remappable: an open overlay (approval, question, picker, panel)
keeps its own keys, and so do the vim cursor keys. Ctrl+P and Ctrl+N remain hardcoded aliases
of Up and Down.

## Skill trust

Which skills have had their [`allowed-tools` honoured](extensions.md#trusted-skills) is kept in
`<config dir>/skill-trust.toml`, outside every repository and apart from the workspace
`trust.toml`. `fontaine plugin trust` and the terminal UI's `/skills trust` write it; nothing in a
project scope can read it into a grant. A record for a project skill is ignored with a warning.

```toml skill-trust.toml
[[skill]]
name = "dev-skills:pr"          # the composed name /skills shows
digest = "sha256:9f2c5a0e"      # of the skill file's bytes, frontmatter and body
tools = ["shell(git *)"]        # the trusted version's allowed-tools
commands = ["!git status"]      # the trusted version's `!` lines
```

`tools` and `commands` are kept so a later change can be shown as a difference. The file is
atomically replaced under `skill-trust.lock` and readable by the current user only. An unreadable
or unparseable file trusts nothing and is reported once at start; `plugin trust` and `untrust`
refuse to overwrite it.

## Workspace trust

Some project files can name programs to run, such as hooks and MCP servers, and skills can carry
commands. Whether the repository may be treated as the user's own is decided by a trust record that
lives outside the repository: `<data dir>/trust.toml`. The terminal UI asks about a folder it does
not know, before anything the repository defines starts, and writes your answer there ("trust" is
recorded; "do not trust" is not, and is asked again next session). You can also edit the file by
hand. Each entry is the canonical path of a trusted directory, written as a TOML literal string
(single quotes) so Windows paths need no escaping:

```toml trust.toml
[[trusted]]
root = '/home/me/code/myrepo'
```

The same file records which repository-defined MCP servers you approved, one `[[approved_servers]]`
entry per root, server name and definition digest; a changed definition is asked about again. See
[MCP servers](mcp.md#servers-a-repository-defines).

Trust covers what a repository can *run*: hooks, MCP servers and skill commands. It never lets any
of them approve a tool call. A headless run (`fontaine -p`) loads a repository's hooks and MCP
servers without asking, as Claude Code's `-p` does, and holds them to the same rule. Prompt
content (instruction files such as `FONTAINE.md`, `CLAUDE.md`, `AGENTS.md` and rules under `.claude/` or
`.fontaine/`, skills, agent files, output styles) and the keys that only tighten load
whatever the answer.

A root is trusted when it, or an ancestor of it, is recorded. Inside a git work tree the search
stops at the work-tree root, so trusting your home directory does not trust every repository in
it; outside any repository there is no bound. An entry that cannot be resolved is ignored, and an
unreadable or unparseable `trust.toml` trusts nothing, and the session reports it once at start so
the file can be fixed or removed. Writes to `trust.toml` and to the settings file are atomic and
take a lock beside the file (`trust.lock`, `config.lock`), so a crash cannot truncate them and two
sessions cannot drop each other's edits. Reading either file takes the same lock briefly, so a
read never sees an edit half-done. Paths compare case-insensitively on Windows.

### Project-local settings

`.fontaine/settings.local.toml` is meant to be personal and untracked. It gets user trust only when
both hold:

- git does not track it. Untracked and ignored files both count; a file that was force-added is
  tracked. Fontaine reads the repository itself and never runs `git`.
- the workspace root is trusted.

Anything else loads as project scope and is validated against the project column: a tracked file, an
untrusted root, no git repository, a `.fontaine` directory that is a symlink out of the work tree, or
any error reading the repository. If an untrusted root's untracked file sets a key only user trust
may set, Fontaine exits with code 2, names the root, and prints the `trust.toml` entry to add.

## Credentials

`<data dir>/credentials.toml` holds stored credentials. Each account has either an `api_key` or,
after a sign-in, an `oauth` entry, and the telemetry and web search keys are kept separately, so an
account named `telemetry` or `websearch` cannot shadow them:

```toml credentials.toml
[accounts.work]
api_key = "sk-..."

[accounts.go.oauth]
access = "..."
refresh = "..."
expires_at = 1798000000
server = "https://opencode.ai/console"
org_id = "org_..."
email = "me@example.com"

[telemetry]
api_key = "ingestion-key"

[websearch]
api_key = "tavily-key"
```

`/login` in the terminal UI is the way to store a credential interactively; see
[Managing providers](tui.md#managing-providers). For a key-based account it reads the key into a
hidden field and writes it here, and for an `opencode-go` account it runs a device sign-in: it
shows a verification URL and a code, waits while you approve it in a browser, and stores the token
pair as an `oauth` entry. The account can also be added in the dialog, which writes its
`[accounts.<name>]` table to your user settings file in place, keeping the file's comments and
layout. The credentials file is written through the owner-only writer and every other entry in it
is kept. A file that cannot be parsed is never overwritten. Logging out and removing an account
delete its entry.

Fontaine refreshes the access token as it expires and writes the rotated pair back; writers of the
file take a lock beside it (`credentials.lock`), so concurrent sessions and logins do not overwrite
each other's rotation. A sign-in that has ended is reported with the `/login` command to run
again.

There is no command-line way to store a credential. Automation sets `api_key_env` on the account
and exports the variable, or ships a `credentials.toml` in the data directory (the schema above,
owner-only). The telemetry ingestion key is the `[telemetry]` entry of that file, or the variable
`[telemetry] api_key_env` names; the Tavily key is the `[websearch]` entry, or the variable
`[websearch] api_key_env` names. An `opencode-go` sign-in is a poor fit for CI: refresh tokens
rotate, so a copied `credentials.toml` works on one machine at a time and the copy left behind
stops working after the first refresh. Use a key-based account there.

A `claude-subscription` account never has an entry here, and one left behind under its name is
never read.

A missing file or entry means no credential. A file that cannot be parsed, or that cannot be read
for any reason other than not existing, is a configuration error naming the path.

## Command line and environment

| Flag or variable | Effect |
|---|---|
| `--settings <file>` | Layers a TOML file over the project and user settings for this session (CLI scope). |
| `--cd <dir>`, `-C <dir>` | Starts in this directory instead of the launch directory; see [Headless runs](headless.md). A relative `--debug-file` resolves against it. |
| `--debug-file <path>` | Writes debug-level diagnostics to the file, which is created readable by the current user only. The `config loaded` event, which carries the resolved scope stack, is written here. |
| `--model <account>/<model>` | Overrides `[model] model`. |
| `--effort <level>` | Overrides `[model] effort` with `off`, `low`, `medium`, `high` or `max`. Any other value is a usage error that lists these. |
| `--permission-mode <mode>` | Overrides `[permissions] default_mode`. `auto` and `afk` are accepted. |
| `--in-container` | States that the process runs inside a container. With `[guardian] profile = "minimal"` it lets the minimal guardian load, after a runtime probe also finds a container; the flag alone is never enough. |
| `--continue`, `--resume <id-or-name>`, `--fork-session`, `--name <name>` | Reopen the latest session of the project directory (the launch directory, or `--cd`), reopen one by id or by name, copy the one that would have been reopened under a new id, or name the session started (or, without `--fork-session`, the one reopened). See [Headless runs](headless.md#continuing-a-session). |
| `--allowed-tools <rule>`, `--disallowed-tools <rule>` | Adds an allow or deny rule for the session (CLI scope). Repeatable. |
| `--afk` | Starts the session in [AFK mode](afk.md), which decides as `auto` does. It is shorthand for `--permission-mode afk`; combined with another `--permission-mode` it is a usage error. |
| `--afk-temperature-ceiling <n>` | Sets `[afk] temperature_ceiling` for the session (CLI scope). |
| `--ax-screen-reader` | Prints the interface as plain labelled lines for a screen reader instead of drawing it, and prints `[Screen Reader Mode: on via flag]` first. A headless run ignores it. |
| `FONTAINE_AX_SCREEN_READER` | `1` turns screen-reader mode on and `0` keeps it off even when `[tui] screen_reader` is `true`; the flag still turns it on. Any other value is ignored, with a warning in the conversation (or on stderr when the UI cannot start). |
| `FONTAINE_LOG` | An `EnvFilter` directive that replaces the default `debug` filter of the debug file. On its own it creates no file and prints nothing. |
| `FONTAINE_CONFIG_DIR`, `FONTAINE_DATA_DIR` | Directory overrides, described above. |

The flags of a non-interactive run (`-p`) are in [Headless runs](headless.md).

An unparseable `FONTAINE_LOG` or an unopenable `--debug-file` path is a usage error: Fontaine exits
with code 2 and names the variable or flag.

## Exit codes

| Code | Meaning |
|---|---|
| 0 | Success. A headless run that finished exits 0 even when tool calls were denied. |
| 1 | A run error: a provider or model failure, a failed model listing, a run stopped by `--max-turns` or `--max-budget-usd`, a headless [goal](tui.md#goals) that ended impossible or paused, or an `--afk` run that was blocked or stopped (see [AFK mode](afk.md)). |
| 2 | Configuration or usage error. |
