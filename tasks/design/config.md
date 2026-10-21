# Config Design

Part of the Fontaine design; the master plan `tasks/fontaine_harness_plan.md` holds the Phase Tracker, workspace layout and `## Decisions`.

## Config (settings scope stack)

The **workspace** is an ordered set of roots. The primary root is the canonicalized launch cwd
(Claude Code parity: a session is rooted where it starts; no upward search for a repo root); further
roots come from `--add-dir`, the `/add-dir` command, and `[permissions] additional_directories`
(Claude Code parity). Containment means "inside any root after symlink resolution" — for the fast
path, Tier 0.5, rule path patterns, protected paths and the critical-path list (each root and its
parents). Only the user adds roots at runtime (`/add-dir` is a user command; no tool can add one).
Extra roots grant file access, not configuration: settings, hooks, MCP servers and extension files
load from the primary root only. The guardian's `session_facts` and trust boundary list every root.

**Scope stack** (highest precedence first; Claude Code parity):

1. **managed** — reserved slot, empty in v1 (no file is read); when it lands, its deny/ask/hard_deny
   entries cannot be removed and it can lock keys.
2. **CLI** — `--settings <file>` plus the flag forms (`--permission-mode`, `--model`,
   `--allowed-tools`, `--disallowed-tools`, `--add-dir`); one session; user trust.
3. **project-local** — `<root>/.fontaine/settings.local.toml`; user trust only while git reports it
   untracked or ignored in a git work tree; otherwise it loads as project scope.
4. **project** — `<root>/.fontaine/settings.toml`; may only tighten.
5. **user** — `<config dir>/config.toml`.

(`<root>` is the primary root — no parent-directory walk, no nested settings files.) Scalars: the
highest-precedence scope that sets a key wins. Lists merge as a union across scopes; a scope may add
only to the lists the scope table lets it set. A key a scope may not set is rejected at load (exit 2).

**Workspace trust** (Claude Code parity). Project-defined executables — project MCP servers, project
hooks, project skills' `!cmd` dynamic context — load in the TUI only after the user trusts the
primary root at a trust prompt shown before they load; each project MCP server is also approved
individually, and a repository cannot pre-approve its own servers. Headless `-p` loads them without
a prompt, as Claude Code does. In both surfaces they can **never change a permission decision**: a
project hook's `allow` is ignored, project MCP tools are gated like any MCP tool (`Arbitrary`), and
`!cmd` runs as a gated `shell` call. The trust record lives in `<data dir>/trust.toml`, keyed by
canonical root path, never in the repo. Tightening keys (deny/ask, soft/hard deny) and prompt
content (FONTAINE.md, skills, agent files, output styles) load regardless of trust. Trust state is
a ledger fact passed to the guardian in `session_facts`.

```toml
[accounts.work]                   # named accounts (opencode-style); several of one kind allowed
kind = "anthropic"                # "anthropic" | "openai" | "gemini" | "openai-compatible" | "ollama"
api_key_env = "ANTHROPIC_API_KEY_WORK"   # credential source: env var NAME, or omit to use the
                                  #   stored-credentials file written by `fontaine auth login <name>`
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
                                  #   Fontaine-verified containment; loosening knob ⇒ user-scope only, never
                                  #   project-settable; see the Phase 4 plan
timeout_secs = 30
confidence_threshold = 0.7
cache = true
scan_tool_results = false
# scanner_model_path = ""        # local PromptGuard 2 ONNX dir; unset ⇒ download on first enable
# scanner_hf_token_env = "HF_TOKEN"   # env var NAME holding the user's HF token for that download

[permissions]
default_mode = "auto"             # shipped default (Claude Code parity); during the Phase-3
                                  #   interim the default is `manual` — no auto without a
                                  #   guardian until Phase 4 wires it in; before then
                                  #   `default_mode = "auto"` from any scope is a config error (exit 2)
deny = []                         # deterministic deny rules: tool name + risk metadata +
                                  #   workspace/path-containment predicates (structured matching,
                                  #   NOT regex on raw arg strings — honors the 2026-10-20
                                  #   no-regex decision); evaluated BEFORE the mode branches in
                                  #   every mode; blocks before the classifier and can't be
                                  #   overridden — Claude Code parity
ask = []                          # deterministic always-prompt rules (CC parity: content-scoped
                                  #   ask rules are evaluated before the classifier and always
                                  #   force a permission prompt, even in auto; in `dontAsk` a
                                  #   match denies); same structured matching as deny; user-scope
                                  #   AND project-scope may ADD ask rules (asking is tightening),
                                  #   never remove; evaluated right after deny, before Tier 0.5,
                                  #   in every mode
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
environment = ["$defaults",        #   user-scope file only; project files may add to
                                   #   soft_deny/hard_deny but NEVER to allow/environment
  "Source control: github.com/myorg"]
allow       = ["$defaults", "Deploying to staging namespace is allowed: resets nightly"]
soft_deny   = ["$defaults"]
hard_deny   = ["$defaults", "Never send repo contents to third-party code-review APIs"]

[agents]
max_parallel = 4                  # concurrent subagents (v1 requirement, Claude Code-style)
subagent_token_budget = 200000    # per-subagent cap; hitting it stops that subagent, result
                                  #   returned as partial, pending escalation/approval prompts
                                  #   cancelled as denied — fan-out can't silently multiply cost
subagent_cost_budget_usd = 2.0    # per-subagent spend cap (same enforcement)
# session_token_budget / session_cost_budget_usd   # root-of-tree totals, default unset; when set,
                                  #   every agent in the tree debits them (hard fan-out ceiling);
                                  #   unset ⇒ tree exposure is structurally bounded by max_parallel
                                  #   (tree-wide) × per-subagent caps × 3-layer depth

[agents.types]                    # per-agent-type policy: model + restricted toolset + mode
                                  #   (default `mode = "inherit"`; an EXPLICIT per-type `mode`
                                  #   may only be `manual` or `dontAsk` — never looser than
                                  #   inherited, session ladder dontAsk < manual < acceptEdits <
                                  #   auto; `plan` is a session-root interactive posture, not a
                                  #   child mode; plan/acceptEdits/auto are reachable for a child
                                  #   only via `inherit` from a parent already there); a child's
                                  #   effective mode is re-evaluated on every call as the tighter
                                  #   of its own setting and the parent's current mode; a type's
                                  #   toolset is always a subset of the parent's
  explore = { model = "$guardian_default", tools = ["ls", "read", "glob", "grep"] }
  worker  = { model = "$main", tools = ["ls", "read", "glob", "grep", "edit", "write", "shell"] }

[shell_preclassifier]
allow_extra = []                  # user extensions to the curated read-only command set — one
                                  #   mode-agnostic list, applying in every mode (the pre-classifier
                                  #   runs before the mode branches); Fontaine
                                  #   never auto-extends it from observed usage. Each entry is a
                                  #   command name or a `command subcommand` pair; no flags are
                                  #   allowed (any flag falls through to the guardian) and positional
                                  #   arguments must be in-scope paths; user-scope only

[telemetry]                        # off unless enabled (Claude Code parity)
enabled = false                    # true REQUIRES `endpoint` and an ingestion key (below); either
                                   #   missing ⇒ config error, exit 2
backend = "micromegas"             # "micromegas" (native sink, span trees) | "otlp" (OTLP/HTTP protobuf)
# endpoint = "https://telemetry.example.com"   # ingestion URL, no default: micromegas ingestion root,
                                   #   or OTLP base URL (/v1/{traces,metrics,logs} appended;
                                   #   micromegas over OTLP: …/ingestion/otlp)
# api_key_env = "MICROMEGAS_INGESTION_API_KEY" # ingestion key: env var NAME holding the bearer token,
                                   #   or omit to use the stored-credentials entry written by
                                   #   `fontaine auth login --telemetry`; never inline; a named
                                   #   env var that is unset or empty counts as missing
traces = true                      # export spans (both backends export trees)
log_user_prompts = false           # content opt-ins, all default false
log_assistant_responses = false    #   (OTEL_LOG_USER_PROMPTS / _ASSISTANT_RESPONSES /
log_tool_details = false           #   _TOOL_DETAILS / _TOOL_CONTENT parity); tool_details = args,
log_tool_content = false           #   shell commands, paths, MCP server/tool names, guardian
                                   #   rule names + reasons; tool_content = tool outputs
content_max_bytes = 61440
resource_attributes = {}           # static tags, e.g. { "team.id" = "platform" }
```

Extension files (Claude Code-compatible frontmatter; built in Phase 5) live beside the settings
files: `<config dir>/` and `<root>/.fontaine/` each hold `skills/<name>/SKILL.md`,
`agents/<name>.md` and `output-styles/<name>.md`. Agent files and `[agents.types]` entries feed one
agent-type registry (project definitions override user ones of the same name, as in Claude Code); a
project agent file may only narrow — toolset a subset of the parent's, `mode` `manual` (default) or
`dontAsk` (no `inherit`), no `model` key (it runs on the session model). A skill's `allowed-tools`
acts as session-scope allow rules while the skill is active when the skill comes from a user trust
scope, and is ignored from project scope.

Rule-precedence semantics and the `$defaults` splice marker are adopted directly from Claude
Code's auto-mode config (proven design). Two more splice markers appear in `[agents.types]`:
`$main` = the session model (`[model]`), `$guardian_default` = the top-ranked validated guardian
model available on the configured accounts (per Decisions). `[accounts]` comes only from the user
file or the CLI, and every `<account>/<model>` reference is rejected from shared project scope — a
project file must not be able to point the guardian (or the session) at another endpoint; a missing or invalid credential for the guardian's account makes the
guardian unavailable, which fails closed. **Trust asymmetry**: shared project settings may only
*tighten* (add deny/ask rules and hard/soft denies, choose a tightening default mode); loosening comes
only from user trust scopes (user, untracked project-local, CLI). Mirroring Claude Code's documented
semantics (`defaultMode: "auto"` and `"bypassPermissions"` are ignored in project settings — Fontaine
ships no `bypass` mode at all), **project and project-local settings may set `[permissions]
default_mode` to `manual` or `dontAsk` only; `auto`, `acceptEdits` and `plan` are rejected** — a
checked-in repo must not be able to disarm its own guardian. Rejecting `auto` from project-local
matches Claude Code; rejecting `acceptEdits` and `plan` is stricter (plan's guardian leg can loosen
a manual user). Tier-0.5 `[shell_preclassifier]` extensions, `[permissions] allow` and
`additional_directories` are loosening, so they follow the same rule. `[guardian] profile` follows it
too — a loosening knob (minimal vs full), and no value of it disables the guardian. Keys that name an
endpoint or a credential (`[accounts.*]`, `[telemetry]` other than `enabled = false`) never come from
a file inside the repository directory, project-local included: each would let a checked-in or
planted file send session data elsewhere. Project-defined executables (project MCP servers and hooks)
are a separate class, governed by Workspace trust above.

Per-key scope (the user file may set every key; a key a scope may not set is rejected at load):

| Key | Project | Project-local (untracked) | CLI |
|-----|---------|---------------------------|-----|
| `[accounts.*]` (`kind`, `base_url`, `api_key_env`, `models`) | no | no | `--settings` |
| `[model]` `model` | no | yes | yes (`--model`) |
| `[guardian]` `model`, `profile`, `timeout_secs`, `confidence_threshold`, `cache`, `scan_tool_results`, `scanner_model_path`, `scanner_hf_token_env` | no | yes | `--settings` |
| `[permissions]` `default_mode` | `manual`/`dontAsk` only | `manual`/`dontAsk` only | yes (`--permission-mode`) |
| `[permissions]` `deny`, `ask` | add only | add only | add only (`--disallowed-tools` → deny) |
| `[permissions]` `allow` | no | yes | yes (`--allowed-tools`) |
| `[permissions]` `additional_directories` | no | yes | yes (`--add-dir`) |
| `[auto_mode]` `environment`, `allow` | no | yes | `--settings` |
| `[auto_mode]` `soft_deny`, `hard_deny` | add only | add only | add only |
| `[agents]` `max_parallel`, budgets; `[agents.types]` | no | yes | `--settings` |
| agent files `agents/*.md` | narrowing only (see above) | — | — |
| `[shell_preclassifier]` `allow_extra` | no | yes | `--settings` |
| `[hooks.*]` | after workspace trust; `allow` ignored | yes | `--settings` |
| `[mcp.servers.*]` | after workspace trust + per-server approval | yes | `--settings` |
| `[telemetry]` | `enabled = false` only | `enabled = false` only | `--settings` |
