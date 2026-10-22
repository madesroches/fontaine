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
| `<data dir>/trust.toml` | Workspace trust record (see [Workspace trust](#workspace-trust)). |
| `<data dir>/credentials.toml` | Stored API keys (see [Credentials](#credentials)). |

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

The workspace is the directory Fontaine was launched from, after symlinks are resolved. Project
files are read only from it: there is no search through parent directories and no nested settings
files. A missing project, project-local or user file is treated as empty. A file that exists but
cannot be read, such as a directory in its place, is a configuration error that names the path. A
missing or unreadable `--settings` file is also an error.

`--settings` takes the path of a TOML file. It does not accept inline text.

### Merge rules

- Scalars: the highest scope that sets the key wins.
- Lists (`deny`, `ask`, `allow`, `additional_directories`, `[auto_mode]` lists, `allow_extra`,
  hooks): the union across scopes. A scope can add only to the lists its column below allows.
- Named table entries (`[accounts.<name>]`, `[mcp.servers.<name>]`, `[agents.types.<name>]`): the
  highest scope that defines the name replaces the whole entry. Entries are never merged field by
  field.
- Map-valued keys (`[telemetry] resource_attributes`): merged per key, and the higher scope wins a
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

The user file may set every key. For the other scopes:

| Key | Project | Project-local (user trust) | CLI |
|---|---|---|---|
| `[accounts.*]` | no | no | yes |
| `[model] model` | no | yes | yes |
| `[guardian] model`, `profile`, `timeout_secs`, `confidence_threshold`, `cache` | no | yes | yes |
| `[guardian] scanner`, `scanner_command`, `scanner_model_path`, `scanner_hf_token_env` | no | no | no |
| `[permissions] default_mode` | `manual` or `dontAsk` only | `manual` or `dontAsk` only | yes |
| `[permissions] deny`, `ask` | add only | add only | add only |
| `[permissions] allow` | no | yes | yes |
| `[permissions] additional_directories` | no | yes | yes |
| `[auto_mode] environment`, `allow` | no | yes | yes |
| `[auto_mode] soft_deny`, `hard_deny` | add only | add only | add only |
| `[agents]` keys and `[agents.types]` | no | yes | yes |
| `[shell_preclassifier] allow_extra` | no | yes | yes |
| `[[hooks.*]]` | yes | yes | yes |
| `[mcp.servers.*]` | yes | yes | yes |
| `[telemetry]` | `enabled = false` only | `enabled = false` only | yes |

A project-local file that has not earned user trust is validated against the project column.

### Trust asymmetry

Shared project settings can add `deny` and `ask` rules and `[auto_mode]` `soft_deny` and
`hard_deny` entries, because those can only make Fontaine stricter. They cannot add `allow` rules,
choose a model or an endpoint, widen the workspace, or change anything that loosens a decision.
Project hooks and MCP servers may be declared, but a project-declared executable can never change a
permission decision.

`[guardian] profile` is a loosening knob, so it is settable from user-trust scopes only. No key in
any scope turns the guardian off.

## Key reference

### `[accounts.<name>]`

Named model accounts. Several accounts of one kind are allowed, and each keeps its own endpoint
and credential. A model is addressed as `<account>/<model>`, split at the first `/`, so a model id
may contain slashes of its own (`router/anthropic/claude-sonnet-4-6`).

| Key | Meaning | Default |
|---|---|---|
| `kind` | `anthropic`, `openai`, `gemini`, `openai-compatible` or `ollama`. Required. | none |
| `api_key_env` | The name of the environment variable that holds the key. Omit it to use the stored credential. Raw keys are never accepted inline. | unset |
| `base_url` | Endpoint override. Required for `openai-compatible`. | the provider's public endpoint |
| `models` | Explicit model list. Without it the provider is asked which models it offers. | unset |

Account names are not empty and contain no `/`. Accounts can be set from the user file or from
`--settings` only: a project file cannot choose an endpoint or a credential source.

What each kind talks to:

| Kind | Wire | Default endpoint | Credential |
|---|---|---|---|
| `anthropic` | Messages API | `https://api.anthropic.com` | required |
| `openai` | Responses API | `https://api.openai.com/v1` | required |
| `gemini` | `generateContent` | `https://generativelanguage.googleapis.com` | required |
| `ollama` | Ollama's native `/api/chat` | `http://localhost:11434` | optional |
| `openai-compatible` | Chat Completions | none; `base_url` is required | optional |

An `openai-compatible` endpoint speaks Chat Completions, not the Responses API, and its models are
unpriced, so `--max-budget-usd` does not apply to them.

#### Where a credential comes from

1. If the account sets `api_key_env`, the key is that environment variable. An unset or empty
   variable is a configuration error (exit code 2) when the main model uses the account, for any
   kind; the stored credential is not consulted as a fallback.
2. Otherwise the key is the account's entry in `credentials.toml`, written by
   `fontaine auth login <account>`.
3. With neither, an `ollama` or `openai-compatible` account sends no `Authorization` header. An
   `anthropic`, `openai` or `gemini` account with no key is a configuration error (exit code 2)
   when the main model uses it.

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

The model of `[guardian] model` is checked the same way when its account has a `models` list, or
shares the main model's account. Otherwise Fontaine makes no call with the guardian's credentials
at startup. An unknown account in `[guardian] model` is always a configuration error.

With no `[guardian] model`, the guardian is the highest-ranked benchmark-validated model that one
of your accounts offers (see [The guardian](guardian.md#choosing-the-guardian-model)). An account
offers a model when its `models` list contains it, when it is the main model's account and the
listing already fetched contains it, or, for any other account without a list, when its `kind`
matches and a credential is present; no listing call is made with a guardian credential. When no
account qualifies, the main model is the guardian and the start prints a notice.

#### Worked examples

Anthropic, with the key in an environment variable:

```toml
[accounts.work]
kind = "anthropic"
api_key_env = "ANTHROPIC_API_KEY"

[model]
model = "work/claude-sonnet-5"
```

A second Anthropic account with its own key, stored with `fontaine auth login personal`:

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

[model]
model = "local/qwen3:4b"
```

OpenRouter, through its OpenAI-compatible endpoint:

```toml
[accounts.router]
kind = "openai-compatible"
base_url = "https://openrouter.ai/api/v1"
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

### `[model]`

| Key | Meaning | Default |
|---|---|---|
| `model` | The session model as `<account>/<model>`. `--model` overrides it. There is no default model: a run with neither is a configuration error (exit code 2) that names both. | unset |

### `[guardian]`

Keys of the guardian that judges tool calls in `auto` mode and in the consequential branch of
`plan` mode. [The guardian](guardian.md) explains what they do.

| Key | Meaning | Default |
|---|---|---|
| `model` | The guardian model as `<account>/<model>`; it may use a different account than `[model]`. A model named here is pinned: it is never substituted when it is unavailable. | unset: the top-ranked validated model your accounts offer, else the main model |
| `profile` | `full` or `minimal`. `minimal` narrows the guardian for containers and only loads when `--in-container` is given and a runtime probe finds a container; otherwise `full` runs, with a notice. It never turns the guardian off. | `full` |
| `timeout_secs` | Time allowed for one verdict, retries included; at least 1. On expiry the call is denied. | `30` |
| `confidence_threshold` | Between 0 and 1. An allow below it on a destructive call is turned into a deny. | `0.7` |
| `cache` | Whether verdicts are cached for 60 seconds. | `true` |
| `scanner` | Tool-result scanner: `off`, `pg2`, `llm` or `command`. User file only. | `off` |
| `scanner_command` | Program used by the `command` scanner. User file only. | unset |
| `scanner_model_path` | Model directory for the `pg2` scanner. User file only. | unset |
| `scanner_hf_token_env` | Name of the environment variable holding a Hugging Face token. User file only. | unset |

The scanner keys are accepted and validated; they take effect when the scanner ships.

```toml
[guardian]
model = "work/claude-haiku-5-5"
profile = "full"
timeout_secs = 30
confidence_threshold = 0.7
cache = true
scanner = "off"
```

`/model` and `/guardian-model` in the terminal UI write `[model] model` and `[guardian] model` to
the user file for you.

### `[permissions]`

Rule strings use the `Tool` or `Tool(specifier)` form described in
[Permissions](permissions.md#rules). At load, Fontaine checks the outer shape, that the tool is one
of `shell`, `read`, `edit`, `write`, `ls`, `glob`, `grep`, `webfetch`, `agent`, `skill`,
`send_message` or an `mcp__<server>[__<tool>|__*]` name, and that the specifier is valid for the
tool (a `shell` specifier is literal command words, a path specifier a gitignore-style pattern, a
`webfetch` specifier `domain:<host>` or `domain:*.<host>`). A bad rule is a configuration error.
`deny` and `ask` rules apply in every mode; `allow` rules act after the floor.

| Key | Meaning | Default |
|---|---|---|
| `default_mode` | `manual`, `acceptEdits`, `plan`, `dontAsk` or `auto`. Applies to both surfaces. | unset: the terminal UI starts in `auto`, a headless run in `manual` |
| `deny` | Rules that always deny. | `[]` |
| `ask` | Rules that always prompt. | `[]` |
| `allow` | Pre-approval rules. User-trust scopes only. | `[]` |
| `additional_directories` | Extra workspace roots. User-trust scopes only. See [Workspace roots](#workspace-roots). | `[]` |

`default_mode = "auto"` loads only from the user file and `--settings`. It is rejected from the shared
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
`additional_directories` entry. File tools reach a path that lies inside any root. A relative entry
is resolved against the launch directory. An entry that does not exist, is not a directory or
cannot be resolved is left out, with a warning on stderr naming the path and whether it came from
`--add-dir` or the settings, and the run continues.

Roots grant file access, not configuration: settings, hooks, MCP servers and extension files load
from the primary root only. A `.fontaine/settings.toml` in an added root is not read. See
[Tools](tools.md#workspace-roots).

### `[[hooks.pre_tool_use]]`, `[[hooks.post_tool_use]]`, `[[hooks.permission_denied]]`

User hooks. Accepted and validated; not yet active.

| Key | Meaning | Default |
|---|---|---|
| `matcher` | A rule in the `Tool(specifier)` form. Required. | none |
| `command` | The program to run. Required. | none |
| `timeout_secs` | At least 1. | unset |

```toml
[[hooks.pre_tool_use]]
matcher = "shell(git *)"
command = "/home/me/.config/fontaine/hooks/git-guard.sh"
timeout_secs = 10
```

### `[mcp.servers.<name>]`

MCP servers. Set exactly one of `command` (a stdio server) or `url`. `args` and `env_vars` apply to
stdio servers only. `env_vars` lists variable names to pass through; values never appear in
configuration. Accepted and validated; not yet active.

```toml
[mcp.servers.github]
command = "github-mcp-server"
args = ["stdio"]
env_vars = ["GITHUB_TOKEN"]

[mcp.servers.docs]
url = "https://mcp.example.com/docs"
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

```toml
[auto_mode]
environment = ["$defaults", "Source control: github.com/myorg"]
allow = ["$defaults", "Deploying to the staging namespace is allowed: it resets nightly"]
hard_deny = ["$defaults", "Never send repo contents to third-party code-review APIs"]
```

### `[agents]`

Accepted and validated; not yet active.

| Key | Meaning | Default |
|---|---|---|
| `max_parallel` | Concurrent subagents; at least 1. | `4` |
| `subagent_token_budget` | Per-subagent token cap. | `200000` |
| `subagent_cost_budget_usd` | Per-subagent spend cap. | `2.0` |
| `session_token_budget` | Whole-session token cap. | unset |
| `session_cost_budget_usd` | Whole-session spend cap. | unset |

`[agents.types.<name>]` defines an agent type with `model` (`$main`, `$guardian_default` or
`<account>/<model>`), `tools` (built-in or `mcp__` tool names) and `mode`. `mode` accepts only
`inherit`, `manual` or `dontAsk`; any other value is an error.

```toml
[agents]
max_parallel = 4

[agents.types.explore]
model = "$guardian_default"
tools = ["ls", "read", "glob", "grep"]
mode = "inherit"
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

## Workspace trust

Some project files can name programs to run, such as hooks and MCP servers. Whether the repository
may be treated as the user's own is decided by a trust record that lives outside the repository:
`<data dir>/trust.toml`. Nothing but trust is recorded there, and the way to trust a root is to edit
the file by hand. Each entry is the canonical path of a trusted directory, written as a TOML
literal string (single quotes) so Windows paths need no escaping:

```toml trust.toml
[[trusted]]
root = '/home/me/code/myrepo'
```

A root is trusted when it, or an ancestor of it, is recorded. Inside a git work tree the search
stops at the work-tree root, so trusting your home directory does not trust every repository in
it; outside any repository there is no bound. An entry that cannot be resolved is ignored, and an
unreadable or unparseable `trust.toml` trusts nothing. Paths compare case-insensitively on Windows.

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

`<data dir>/credentials.toml` holds stored API keys. Each account has an `api_key`, and the
telemetry key is kept separately, so an account named `telemetry` cannot shadow it:

```toml credentials.toml
[accounts.work]
api_key = "sk-..."

[telemetry]
api_key = "ingestion-key"
```

`fontaine auth login <account>` stores an account's key and `fontaine auth login --telemetry`
stores the telemetry ingestion key. The secret is read from a hidden prompt, or from stdin when
stdin is not a terminal (`printf %s "$KEY" | fontaine auth login work`). The account must exist in
the loaded configuration; an empty secret is a usage error. The file is written through the
owner-only writer and every other entry in it is kept. A file that cannot be parsed is never
overwritten.

A missing file or entry means no credential. A file that cannot be parsed, or that cannot be read
for any reason other than not existing, is a configuration error naming the path.

## Command line and environment

| Flag or variable | Effect |
|---|---|
| `--settings <file>` | Layers a TOML file over the project and user settings for this session (CLI scope). |
| `--debug-file <path>` | Writes debug-level diagnostics to the file, which is created readable by the current user only. The `config loaded` event, which carries the resolved scope stack, is written here. |
| `--model <account>/<model>` | Overrides `[model] model`. |
| `--permission-mode <mode>` | Overrides `[permissions] default_mode`. `auto` is accepted. |
| `--in-container` | States that the process runs inside a container. With `[guardian] profile = "minimal"` it lets the minimal guardian load, after a runtime probe also finds a container; the flag alone is never enough. |
| `--allowed-tools <rule>`, `--disallowed-tools <rule>` | Adds an allow or deny rule for the session (CLI scope). Repeatable. |
| `fontaine auth login` | Stores a credential; see [Credentials](#credentials). |
| `FONTAINE_LOG` | An `EnvFilter` directive that replaces the default `debug` filter of the debug file. On its own it creates no file and prints nothing. |
| `FONTAINE_CONFIG_DIR`, `FONTAINE_DATA_DIR` | Directory overrides, described above. |

The flags of a non-interactive run (`-p`) are in [Headless runs](headless.md).

An unparseable `FONTAINE_LOG` or an unopenable `--debug-file` path is a usage error: Fontaine exits
with code 2 and names the variable or flag.

## Exit codes

| Code | Meaning |
|---|---|
| 0 | Success. A headless run that finished exits 0 even when tool calls were denied. |
| 1 | A run error: a provider or model failure, a failed model listing, or a run stopped by `--max-turns` or `--max-budget-usd`. |
| 2 | Configuration or usage error. |
