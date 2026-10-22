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

Named model accounts. Several accounts of one kind are allowed. A model is addressed as
`<account>/<model>`. Accepted and validated; not yet active.

| Key | Meaning | Default |
|---|---|---|
| `kind` | `anthropic`, `openai`, `gemini`, `openai-compatible` or `ollama`. Required. | none |
| `api_key_env` | The name of the environment variable that holds the key. Omit it to use the stored credential. Raw keys are never accepted inline. | unset |
| `base_url` | Endpoint override. Required for `openai-compatible`. | unset |
| `models` | Explicit model list. | unset |

```toml
[accounts.work]
kind = "anthropic"
api_key_env = "ANTHROPIC_API_KEY_WORK"

[accounts.local]
kind = "ollama"
base_url = "http://localhost:11434"

[accounts.router]
kind = "openai-compatible"
base_url = "https://openrouter.ai/api/v1"
models = ["anthropic/claude-sonnet-4-6"]
```

Account names are not empty and contain no `/`.

### `[model]`

| Key | Meaning | Default |
|---|---|---|
| `model` | The session model as `<account>/<model>`. Accepted and validated; not yet active. | unset |

### `[guardian]`

Accepted and validated; not yet active.

| Key | Meaning | Default |
|---|---|---|
| `model` | The guardian model as `<account>/<model>`; it may use a different account than `[model]`. | unset |
| `profile` | `full` or `minimal`. `minimal` narrows the guardian for containers; it never turns it off. | `full` |
| `timeout_secs` | Time allowed for one verdict; at least 1. | `30` |
| `confidence_threshold` | Between 0 and 1. | `0.7` |
| `cache` | Whether verdicts are cached. | `true` |
| `scanner` | Tool-result scanner: `off`, `pg2`, `llm` or `command`. User file only. | `off` |
| `scanner_command` | Program used by the `command` scanner. User file only. | unset |
| `scanner_model_path` | Model directory for the `pg2` scanner. User file only. | unset |
| `scanner_hf_token_env` | Name of the environment variable holding a Hugging Face token. User file only. | unset |

```toml
[guardian]
model = "work/claude-haiku-5-5"
profile = "full"
timeout_secs = 30
confidence_threshold = 0.7
cache = true
scanner = "off"
```

### `[permissions]`

Rule strings use the `Tool` or `Tool(specifier)` form. At load, Fontaine checks the outer shape,
that the tool is one of `shell`, `read`, `edit`, `write`, `ls`, `glob`, `grep`, `webfetch`,
`agent`, `skill`, `send_message` or an `mcp__<server>[__<tool>|__*]` name, and that a `webfetch`
specifier has the form `domain:<host>` or `domain:*.<host>`. Other specifiers are kept as written.
Accepted and validated; not yet active.

| Key | Meaning | Default |
|---|---|---|
| `default_mode` | `manual`, `acceptEdits`, `plan`, `dontAsk` or `auto`. | `manual` |
| `deny` | Rules that always deny. | `[]` |
| `ask` | Rules that always prompt. | `[]` |
| `allow` | Pre-approval rules. User-trust scopes only. | `[]` |
| `additional_directories` | Extra workspace roots. User-trust scopes only. | `[]` |

`default_mode = "auto"` is rejected at load from the user file and from `--settings`, because auto
mode is not available yet. It is also never accepted from project scopes.

```toml
[permissions]
default_mode = "manual"
deny = ["shell(rm *)", "read(.env)"]
ask = ["webfetch(domain:*.example.com)"]
allow = ["shell(cargo test)"]
additional_directories = ["/home/me/shared-libs"]
```

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

Prose lists for the guardian: `environment`, `allow`, `soft_deny`, `hard_deny`. The marker
`$defaults` splices in the shipped defaults. Accepted and validated; not yet active.

```toml
[auto_mode]
environment = ["$defaults", "Source control: github.com/myorg"]
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
| `allow_extra` | Extra read-only commands. Each entry is a command name or a `command subcommand` pair with no flags. Accepted and validated; not yet active. | `[]` |

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

No exporter is built yet: with `enabled = true` the inputs are validated and nothing is exported.
Project scopes can only set `enabled = false`.

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

`<data dir>/credentials.toml` holds stored API keys. Fontaine only reads it. Each account has an
`api_key`, and the telemetry key is kept separately, so an account named `telemetry` cannot shadow
it:

```toml credentials.toml
[accounts.work]
api_key = "sk-..."

[telemetry]
api_key = "ingestion-key"
```

A missing file or entry means no credential. A file that cannot be parsed, or that cannot be read
for any reason other than not existing, is a configuration error naming the path.

## Command line and environment

| Flag or variable | Effect |
|---|---|
| `--settings <file>` | Layers a TOML file over the project and user settings for this session (CLI scope). |
| `--debug-file <path>` | Writes debug-level diagnostics to the file, which is created readable by the current user only. The `config loaded` event, which carries the resolved scope stack, is written here. |
| `FONTAINE_LOG` | An `EnvFilter` directive that replaces the default `debug` filter of the debug file. On its own it creates no file and prints nothing. |
| `FONTAINE_CONFIG_DIR`, `FONTAINE_DATA_DIR` | Directory overrides, described above. |

An unparseable `FONTAINE_LOG` or an unopenable `--debug-file` path is a usage error: Fontaine exits
with code 2 and names the variable or flag.

## Exit codes

| Code | Meaning |
|---|---|
| 0 | Success. |
| 2 | Configuration or usage error. |
