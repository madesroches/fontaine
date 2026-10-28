# Config Design

Part of the Fontaine design; the master plan `tasks/fontaine_harness_plan.md` holds the Phase Tracker, workspace layout and `## Decisions`.

## Config (settings scope stack)

The **workspace** is an ordered set of roots. The primary root is the canonicalized launch cwd
(Claude Code parity: a session is rooted where it starts; no upward search for a repo root); further
roots come from `--add-dir`, the `/add-dir` command, and `[permissions] additional_directories`
(Claude Code parity). Containment means "inside any root after symlink resolution" — for the fast
path, Tier 0.5, rule path patterns, protected paths and the critical-path list (each root and its
parents). Only the user adds roots at runtime (`/add-dir` is a user command; no tool can add one). The one automatic root is the per-session scratch directory (outside the config and data directories, owner-only, removed on exit, swept after a crash), appended last.
Extra roots grant file access, not configuration: settings, hooks, MCP servers and extension files
load from the primary root only. The guardian's `session_facts` and trust boundary list every root.

**Scope stack** (highest precedence first; Claude Code parity):

1. **managed** — reserved slot, empty in v1 (no file is read); when it lands, its deny/ask/hard_deny
   entries cannot be removed and it can lock keys.
2. **CLI** — `--settings <file>` plus the flag forms (`--permission-mode`, `--model`,
   `--allowed-tools`, `--disallowed-tools`, `--add-dir`); one session; user trust.
3. **project-local** — `<root>/.fontaine/settings.local.toml`; user trust only while `gix` reports it
   untracked or ignored in a git work tree (a missing index counts as empty; the symlink-resolved path is looked up case-insensitively) AND the primary root is trusted; otherwise (tracked, untrusted root, no `.git`, or a `gix` error) it loads as project scope. Fontaine never spawns `git` for this.
4. **project** — `<root>/.fontaine/settings.toml`; may only tighten.
5. **user** — `<config dir>/config.toml`.

(`<root>` is the primary root — no parent-directory walk, no nested settings files.) Scalars: the
highest-precedence scope that sets a key wins. Lists merge as a union across scopes; a scope may add
only to the lists the scope table lets it set. A named table entry (`[accounts.<name>]`, `[mcp.servers.<name>]`, `[agents.types.<name>]`) is replaced whole by the highest-precedence scope defining that name; map-valued keys merge per key, the higher-precedence scope winning; `[[hooks.*]]` arrays merge by union. A key a scope may not set, and any unknown key or section in any scope, is rejected at load (exit 2).

**Workspace trust** (Claude Code parity: folder trust covers the whole folder). Project and
project-local executables — MCP servers, hooks, skills' `!cmd` dynamic context — load in the TUI only
after the user trusts the primary root at a trust prompt shown before they load; each project MCP
server is also approved individually, and a repository cannot pre-approve its own servers. Headless
`-p` loads them without a prompt, as Claude Code does. In both surfaces a project hook's `allow` is
ignored, project MCP tools are gated like any MCP tool (`Arbitrary`), and `!cmd` runs as a gated
`shell` call; a project-local (user-trust) hook's `allow` counts as a user grant. The trust record lives in `<data dir>/trust.toml`, keyed by
canonical root path (a trusted ancestor covers subdirectories, bounded at the enclosing git work-tree root as in Claude Code, unbounded outside a repo; hand-editable), never in the repo. Tightening keys (deny/ask, soft/hard deny) and prompt
content (the instruction hierarchy of `FONTAINE.md`, `CLAUDE.md` and `AGENTS.md` files, rules, imports; skills, agent files, output styles) load regardless of trust, though an instruction file or import that resolves outside the workspace roots and the enclosing git work tree is refused and a `read` deny rule keeps a file out. Trust state is
a ledger fact passed to the guardian in `session_facts`.

This block is schema-valid but not load-valid before Phase 4 (`default_mode = "auto"`) and names models that need provider discovery; `docs.rs` only parses and schema-validates such blocks.

```toml
[accounts.work]                   # named accounts (opencode-style); several of one kind allowed
kind = "anthropic"                # "anthropic" | "openai" | "gemini" | "openai-compatible" | "ollama"
api_key_env = "ANTHROPIC_API_KEY_WORK"   # credential source: env var NAME, or omit to use the
                                  #   stored-credentials file written by `/login` (or provisioned by hand)
                                  #   (`<data dir>/credentials.toml`, owner-only). Raw keys are never accepted inline.
# base_url = ""                   # optional; required for openai-compatible (OpenRouter/LM Studio/vLLM/LiteLLM)
# models = ["claude-sonnet-4-6"]  # optional explicit list; unset ⇒ discovered from the provider
                                  #   where it supports listing, else the account offers no models
                                  #   until `models` is set

[accounts.local]
kind = "ollama"
base_url = "http://localhost:11434"

[model]
model = "work/claude-sonnet-4-6"  # `<account>/<model>`; the account provides the model

[guardian]
model = "work/claude-haiku-5-5"   # `<account>/<model>`, may be a different account than [model]; optional; unset ⇒ top-ranked
                                  #   validated guardian model available on any configured account (see the Phase 4 plan)
profile = "full"                  # "full" | "minimal" — minimal is the container-mode narrowing (never off):
                                  #   escape-class ruleset + reduced context on the resolved `model` above; requires
                                  #   Fontaine-verified containment; loosening knob ⇒ user trust scopes only, never
                                  #   project-settable; see the Phase 4 plan
timeout_secs = 30
confidence_threshold = 0.7
cache = true
scanner = "off"                   # tool-result scanner: "off" | "pg2" | "llm" | "command"; `llm` uses the resolved `model`
                                  #   above; error/timeout/unparseable ⇒ flagged (fail closed); user file only
# scanner_command = ""           # `command` only: program run with JSON {tool, content} on stdin, prints
                                  #   {"verdict":"clean"|"flagged","reason":...}; bounded by `timeout_secs`
# scanner_model_path = ""        # `pg2` only: dir with model.safetensors + tokenizer.json + config.json; unset ⇒ download on first enable
# scanner_hf_token_env = "HF_TOKEN"   # env var NAME holding the user's HF token for that download

[permissions]
default_mode = "auto"             # shipped default; config error before Phase 4 (default `manual`)
deny = []                         # floor rules, every mode
ask = []                          # always prompt, even in `auto`
auto_tracked_edits = true         # `auto`: tracked/new/session-created file edits skip the guardian; project scope: false only
auto_scratch_writes = true        # `auto`: file-tool writes into scratch skip the guardian; project scope: false only
allow = []                        # pre-approval rules, same grammar; user trust scopes only
                                  #   (user, untracked project-local, CLI); step 5a, after the
                                  #   floor; broad code-exec rules suspended in `auto`
additional_directories = []      # extra workspace roots; user trust scopes only

[[hooks.pre_tool_use]]             # user hooks (`permissions.md` §User hooks); also post_tool_use, permission_denied
matcher = "shell(git *)"           #   rule grammar
command = "<config dir>/hooks/git-guard.sh"
timeout_secs = 10

[mcp.servers.github]               # stdio: command/args/env_vars; streamable HTTP: url
command = "github-mcp-server"
args = ["stdio"]
env_vars = ["GITHUB_TOKEN"]        # env var NAMES passed through; values never in config

[auto_mode]                        # prose lists; the loosening slots (allow/environment) are
environment = ["$defaults",        #   user trust scopes only; project files may add to
                                   #   soft_deny/hard_deny but NEVER to allow/environment
  "Source control: github.com/myorg"]
allow       = ["$defaults", "Deploying to staging namespace is allowed: resets nightly"]
soft_deny   = ["$defaults"]
hard_deny   = ["$defaults", "Never send repo contents to third-party code-review APIs"]

[agents]
max_parallel = 4                  # concurrent subagents (v1 requirement, Claude Code-style)
subagent_token_budget = 200000    # per-subagent cap; semantics in Phase 5 design
subagent_cost_budget_usd = 2.0    # per-subagent spend cap; inactive (one-time notice) on a model with no catalog price, the token cap still binds
# session_token_budget / session_cost_budget_usd   # root-of-tree totals, default unset

[agents.types]                    # per-type model + toolset + mode (`mode` accepts only `inherit`/`manual`/`dontAsk`, else load error (exit 2); default "inherit", never looser than parent; under a `plan` parent child edits are blocked and non-edits use the tighter of the child's mode and `manual`)
  explore = { model = "$guardian_default", tools = ["ls", "read", "glob", "grep"] }
  worker  = { model = "$main", tools = ["ls", "read", "glob", "grep", "edit", "write", "shell"] }

[context]
autocompact_window = 150000       # tokens; unset = the model's window minus the summarizer's reserve. Compaction runs once the
                                  #   conversation's messages pass the lower of the two. Positive integer, else load error (exit 2).
                                  #   Not from shared project scope; a catalog-listed model with no known window compacts only when set

[shell_preclassifier]
allow_extra = []                  # user extensions to the curated read-only command set — one
                                  #   mode-agnostic list, applying in every mode (the pre-classifier
                                  #   runs before the mode branches); Fontaine
                                  #   never auto-extends it from observed usage. Each entry is a
                                  #   command name or a `command subcommand` pair; no flags are
                                  #   allowed (any flag falls through to the guardian) and positional
                                  #   arguments must be in-scope paths; user trust scopes only

[telemetry]                        # off unless enabled (Claude Code parity)
enabled = false                    # true REQUIRES `endpoint` and an ingestion key (below); either
                                   #   missing ⇒ config error, exit 2
backend = "micromegas"             # "micromegas" (native sink, span trees) | "otlp" (OTLP/HTTP protobuf)
# endpoint = "https://telemetry.example.com"   # ingestion URL, no default: micromegas ingestion root,
                                   #   or OTLP base URL (/v1/{traces,metrics,logs} appended;
                                   #   micromegas over OTLP: …/ingestion/otlp)
# api_key_env = "MICROMEGAS_INGESTION_API_KEY" # ingestion key: env var NAME holding the bearer token,
                                   #   or omit to use the stored-credentials entry written by
                                   #   a hand-provisioned `[telemetry]` entry in credentials.toml; never inline; a named
                                   #   env var that is unset or empty counts as missing
traces = true                      # export spans (both backends export trees)
log_user_prompts = false           # content opt-ins, all default false
log_assistant_responses = false    #   (OTEL_LOG_USER_PROMPTS / _ASSISTANT_RESPONSES /
log_tool_details = false           #   _TOOL_DETAILS / _TOOL_CONTENT parity); tool_details = args,
log_tool_content = false           #   shell commands, paths, MCP server/tool names, guardian
                                   #   rule names + explanations; tool_content = tool outputs
content_max_bytes = 61440
resource_attributes = {}           # static tags, e.g. { "team.id" = "platform" }

[env]                              # user-trust scopes only; literal NAME = "value" pairs set on every
HTTPS_PROXY = "http://proxy.internal:3128"   # tool and extension subprocess (shell, `!`, hooks, stdio MCP)
```

Extension files (Claude Code-compatible frontmatter; built in Phase 5) live beside the settings
files: `<config dir>/` and `<root>/.fontaine/` each hold `skills/<name>/SKILL.md`,
`agents/<name>.md` and `output-styles/<name>.md`. Agent files and `[agents.types]` entries feed one
agent-type registry (project definitions override user ones of the same name, as in Claude Code); a
project agent file may only narrow — toolset a subset of the parent's, `mode` `manual` (default) or
`dontAsk` (no `inherit`), no `model` key (it runs on the session model). A user-scope agent file follows the `[agents.types]` mode rule: `permissionMode` is accepted only as `inherit`, `default` (↦ `manual`) or `dontAsk`; any other value (`acceptEdits`, `auto`, `plan`, `bypassPermissions`) is a load error (exit 2) naming the file (stricter than Claude Code). A skill's `allowed-tools`
acts as session-scope allow rules while the skill is active when the skill comes from a user trust
scope, and is ignored from project scope.

Rule-precedence semantics and the `$defaults` splice marker are adopted directly from Claude
Code's auto-mode config (proven design). Two more splice markers appear in `[agents.types]`:
`$main` = the session model (`[model]`), `$guardian_default` = the top-ranked validated guardian
model available on the configured accounts (per Decisions). `[accounts]` comes only from the user
file or the CLI, and every `<account>/<model>` reference is rejected from shared project scope — a
project file must not be able to point the guardian (or the session) at another endpoint; a missing or invalid credential for the guardian's account makes the
guardian unavailable, which fails closed. **Trust asymmetry**: the per-key scope table below is the rule (shared project settings may only tighten); the Decisions entries on trust asymmetry and `default_mode` record the deviations from Claude Code.

Per-key scope (the user file may set every key; a key a scope may not set is rejected at load). The single source is the `RULES` table in `fontaine-core/src/config_scope.rs`, one row per key; a key with no row is refused from project scopes, and a test fails until every `Settings` key has a row:

| Key | Project | Project-local (untracked) | CLI |
|-----|---------|---------------------------|-----|
| `[accounts.*]` (`kind`, `base_url`, `api_key_env`, `models`) | no | no | `--settings` |
| `[model]` `model` | no | yes | yes (`--model`) |
| `[guardian]` `model`, `profile`, `timeout_secs`, `confidence_threshold`, `cache` | no | yes | `--settings` |
| `[guardian]` `scanner`, `scanner_command`, `scanner_model_path`, `scanner_hf_token_env` | no | no | no (user file only) |
| `[permissions]` `default_mode` | `manual`/`dontAsk` only | `manual`/`dontAsk` only | yes (`--permission-mode`) |
| `[permissions]` `deny`, `ask` | add only | add only | add only (`--disallowed-tools` → deny) |
| `[permissions]` `allow` | no | yes | yes (`--allowed-tools`) |
| `[permissions]` `additional_directories` | no | yes | yes (`--add-dir`) |
| `[permissions]` `auto_tracked_edits`, `auto_scratch_writes` | `false` only | `false` only | yes |
| `[auto_mode]` `environment`, `allow` | no | yes | `--settings` |
| `[auto_mode]` `soft_deny`, `hard_deny` | add only | add only | add only |
| `[agents]` `max_parallel`, budgets; `[agents.types]` | no | yes | `--settings` |
| agent files `agents/*.md` | narrowing only (see above) | — | — |
| `[context]` `autocompact_window` | no | yes | `--settings` |
| `[shell_preclassifier]` `allow_extra` | no | yes | `--settings` |
| `[hooks.*]` | after workspace trust; `allow` ignored | yes, after workspace trust | `--settings` |
| `[mcp.servers.*]` | after workspace trust + per-server approval | yes, after workspace trust | `--settings` |
| `[telemetry]` | `enabled = false` only | `enabled = false` only | `--settings` |
| `[tui]` `theme`, `screen_reader` | no | no | `--settings` |
| `[env]` | no (scope error) | yes | `--settings` |
