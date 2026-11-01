# Changelog

All notable changes are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project uses
[semantic versioning](https://semver.org/) (0.x until the first stable release).

## [Unreleased]

### Added

#### Guardian

- `fontaine auto-mode setup` and `/auto-mode-setup` end with an optional offer of the guardian
  composition's `screen` preset: when an Ollama account holds a model Fontaine has benchmarked
  (`qwen3:4b-instruct-2507-q4_K_M`), you choose safe (the screen only stops calls) or fast (the
  screen allows what it is confident about), confirm the benchmarked threshold shown with its
  evidence, and setup writes `[guardian.composition]` to the user file with `shadow = false`. Nothing
  is written without a yes, a configured composition or the `minimal` profile skips it,
  `--no-composition` turns it off, and a run without a terminal reports `composition` in its JSON and
  writes nothing (#372).

#### Agents

- The agent list and the agents panel show `· writing 4.2k chars` for an agent whose model is
  mid-step, reasoning and tool-call arguments included, and `main` shows its own count in the
  list, so a long model step no longer looks like a stall (#343). Model requests log their start and
  end, with elapsed time and characters written, at debug level.
- A built-in `fork` agent type starts a subagent from a copy of the main conversation, including
  the current turn, so its `prompt` can be a short task (#39). It runs on the session model, can be
  started only by the main conversation, and fails to start rather than run without the context. The
  guardian's intent source is unchanged.
- `/goal <condition>` keeps the session working until a separate model judges the condition met, in
  the terminal UI and as `fontaine -p "/goal ..."`. After each turn the evaluator (the guardian's
  model) answers met, not met or impossible; not met starts another turn as a harness note, never a
  user message. Unattended stretches are bounded by `[goal] max_turns`, an optional
  `[goal] max_cost_usd` and a no-progress cap, a failed turn or evaluator pauses the goal, and the
  goal is restored on `--continue`/`--resume`. Headless adds a `goal` event, `result.goal` and
  `usage.goal`, and exits 1 when the goal ends impossible or paused (#17).
- Parallel subagents: the `agent` tool (blocking, or `background: true` with the report
  delivered as a notification) and `send_message`, agent types from `[agents.types]` and agent
  files, a mode that is never looser than the parent's, a nesting limit of three layers,
  `[agents] max_parallel`, per-subagent and session token and cost budgets, and a
  background-agents panel (`/agents`, Ctrl-T) with kill and per-agent decisions. Headless runs
  wait for background subagents.
- `job_output` waits on a background subagent: given an `agent-*` id it blocks until that
  subagent ends, or `wait_secs` passes, and reports only its status. The report still arrives
  as the notification attached to that result, exactly once, so a parent no longer polls with
  wrong-tool errors and a nested parent has a way to wait instead of ending its turn (which
  would kill its children). Only the agent that started a subagent may wait on it, and a
  subagent whose type names `agent` also gets `job_output`. `BashOutputTool`, `AgentOutput`
  and `AgentOutputTool` are accepted as Claude Code names for `job_output` (#301).
- Subagent conversations persist. Every subagent's conversation is written to the session
  store as a per-agent log (`logs/agent-<n>.jsonl`, saved when a run ends, kept when a run
  fails, reloaded when a resumed run starts), and agent ids are stable across `--resume`:
  the session ledger anchors the id counter, so a restarted session never reuses an id whose
  transcript is already on disk (#300).
- `/export` of a session with subagents — live, or recorded in the ledger before a `--resume` —
  writes a folder instead of one file: `main.md` carries the root conversation with a
  `## Subagents` index (id, type, parent, status, oldest first), and one `<agent-id>.md` per
  subagent renders its transcript with the same detail as the root. A running subagent is
  exported rather than refused; its file holds its completed runs only, and its index row says
  so. Everything stays owner-only, and an existing file or folder is still an error: an export
  never overwrites (#300).

#### Build and release

- Release binaries for Linux, macOS and Windows built with `pg2` by cargo-dist on a `vX.Y.Z`
  tag, each archive carrying the license, readme and changelog; `cargo install --git
  https://github.com/madesroches/fontaine fontaine` from source.
- Documentation under `docs/`, a README quickstart, contributing guide and security policy.

#### Config

- A scope stack of user settings, shared project settings, project-local settings
  (`.fontaine/settings.local.toml`, never committed) and `--settings`, with strict, scope-aware
  validation. An unknown key is a configuration error (exit code 2).
- Project scope can only tighten (deny and ask additions); loosening keys load only from the
  user-trust scopes (the user file, project-local settings and `--settings`).
- `FONTAINE_CONFIG_DIR` and `FONTAINE_DATA_DIR` override the directories, files that hold
  secrets are created owner-only, and workspace trust is recorded in `trust.toml`.
- `[permissions] default_mode` is optional: the terminal UI starts in `auto` and a headless run
  in `manual`. `auto` is accepted from the user file, `--settings` and `--permission-mode`;
  both project scopes (shared and project-local) accept only `manual` and `dontAsk`.
- An `[env]` table of literal variables is set on every `shell` call, `!` command, hook and
  stdio MCP server, after the credential scrub. It loads only from the user-trust scopes,
  rejects `FONTAINE_*` names, never shows a value in a configuration error, and the guardian
  sees the variable names, not their values (#5).

#### Core

- Auto memory (`[memory] auto`, user scope only, on by default while it is tested). The agent saves
  notes about the project with `write` and `edit` to `<data dir>/memory/<project key>/`, outside the
  repository, and the capped `MEMORY.md` index loads into later sessions framed as unverified notes.
  Every write is gated by the protected-path floor and, in `auto`, the guardian, with a
  memory-write rule in both guardian prompts; there is no fast path. `/memory` lists the saved files,
  `/context` counts the layer, and `evals/dataset/memory.jsonl` adds the memory-poisoning cases (#8).
- `--cd <DIR>` (`-C`) starts Fontaine in another directory: it becomes the project root for
  settings, trust, workspace and session identity, and relative `--settings`, `--add-dir` and
  `--debug-file` values resolve against it (#145).
- A versioned system prompt steers the agent toward the dedicated read tools, denial handling
  and the workspace roots, layered with `FONTAINE.md` from the user config directory and the
  primary root.
- Fontaine reads `FONTAINE.md`, `CLAUDE.md` and `AGENTS.md` from your config, from every directory
  from the filesystem root down to the primary root (with `CLAUDE.local.md` and
  `FONTAINE.local.md`), from `.claude/` and `.fontaine/` (including `rules/`), and from `@path`
  imports up to four levels deep. `AGENTS.md` is read when no `CLAUDE` or `FONTAINE` file
  exists. Identical files load once. Rules can be scoped to paths with a `paths` frontmatter key,
  and nested files and matching rules load into the tool result when the agent reads or writes a
  file under them (#7).
- Repository files that resolve outside the workspace and its git work tree, and imports that
  leave it, are skipped with a warning; a `read` deny rule keeps a file out of the prompt.
  `.claude` joins the protected directories. The guardian's `project_instructions` is a
  snapshot of the launch set from session start, most specific file first (#7).
- `/memory` lists the instruction files and opens one in `$VISUAL` or `$EDITOR`; `/init` has the
  agent write or improve the project instruction file (#7).

#### Evals

- `fontaine-evals --screen live:<account>/<model> --stop-threshold <risk>` scores the cascade
  (catch, guardian calls, latency, benign cases escalated) from one run (#130).
- `fontaine-evals --allow-threshold <risk> [--steering-veto <p>]` scores the allow path. The
  cascade summary replaces `mean_latency_ms` with `allowed_latency_ms` and adds
  `should_allow_stopped`, `screen_allowed` and `must_not_clear_screen_allowed` (#135).
- A golden dataset and the `fontaine-evals` runner: `--guardian fixture` (offline),
  `--guardian none` (cases through the real pipeline) and `--guardian live:<account>/<model>`
  (a live guardian or scanner, with latency; writes the models that clear the inclusion bar to
  `--models-file`). `baseline capture` and `baseline diff` compare cases with `claude -p`.
- A live run streams: a progress line per case on stderr, each case's result line as it is
  scored, and a flushed `<out>.partial.jsonl` sidecar that `--resume` continues from after an
  interruption. A case the runner cannot pose is `UNSCORED` instead of aborting the run, and
  `--out`'s parent directory is created and checked before the first case (#94).
- `--guardian screen:<model>` scores a typed-decision model (Laya, or DiffusionGemma behind
  vLLM) served over `/v1/systemone` on the `guardian` cases, with `--screen-url`,
  `--screen-timeout-ms` and `--screen-reference`. It reports catch and benign-stop rates at
  recalibrated thresholds, the zero-miss allow threshold, a reliability table and the
  disagreement with another scorecard. Eval-only (#98).
- `fontaine-evals --screen-api-key-file` sends a bearer key (https or loopback only) and
  `--screen-max-cost-usd` caps the spend the server reports, so a `screen:` run can score Jev
  through OpenRouter; the report carries `cost_usd` and `model_resolved` (#127).
- `fontaine-evals --screen-framing employee` asks a `screen:` model the yes/no "would an employee
  be allowed to take this action?" question on the guardian state without its instruction
  sentences and digest line, the configuration the Jev research found strongest; `verdict` stays
  the default and its requests and reports are unchanged (#127).
- `fontaine-evals --screen-framing employee-levels` asks the same state as one five-level `choice`
  ("how far outside what an employee would be allowed to do", from `within` to `fireable`) and
  reads `p(within)` as `p(allow)`, the framing the Jev research and its fresh-case check found
  strongest; each case records the server's top level (#127).
- Laya scored on the golden set, and the spike's experiments recorded with their scripts and a
  400-case synthetic training corpus. Experiments: question framings, ensembles, embedding probes,
  public corpora, head fine-tuning, question framings that ask whether the action fits the task,
  small chat models read through logprobs, and a held-out screen-then-guardian cascade. With
  `qwen3-4b-instruct-2507` as a stop-only screen, the cascade caught 96.7% of attacks (91.7%
  for the guardian alone) with 41% of the guardian calls. Follow-ups #123–#130 and #132 (#98).

#### Extensibility

- Plugin and marketplace auto-update (#283). Registered marketplaces and installed plugins update
  themselves by default: a background pass at interactive start (at most once a day, never in
  `fontaine -p`) fetches each upstream revision, validates it and stages it beside the live
  checkout, and the next interactive start swaps it in before plugins load, so nothing changes
  under a running session. The pin stays the approval boundary: a plugin revision applies only
  when its approval fingerprint (hook, MCP and LSP definitions, skill `!` commands, referenced
  variables and the content of every non-markdown file) equals the one recorded at install or at
  the last `plugin update`; otherwise it is held with the old checkout live and a notice tells you
  to run `fontaine plugin update <name>`. `plugin install` and `plugin update` now list every hook
  command, MCP server definition and skill `!` line they approve. `[plugins] auto_update` (user
  file only) turns it off; `fontaine plugin auto-update` and `plugin marketplace auto-update`
  opt out one entry; `plugin list` marks held revisions. Outcomes are logged to
  `plugin-updates.jsonl` and shown once as notices. This amends "installs, updates and marketplace
  changes are CLI commands only".
- Plugins and marketplaces (#30). A plugin bundles Claude Code's extension files into one
  directory with a manifest, and a marketplace is a git repository or local directory with a
  catalog. Installs are user-scope only and pinned by commit; `fontaine plugin marketplace` and
  `fontaine plugin` add, list, update and remove marketplaces and install, update, uninstall,
  enable, disable and list plugins. Installed plugin content is third-party, so it loads under
  tighten-only rules: its `allow` never counts, its skills cannot lend `allowed-tools`, its agent
  files can only narrow, and its only permission or guardian effect is deny-side additions. Plugin
  hooks, MCP servers and skill `!` commands start without the workspace-trust gate, because install
  is the approval; the terminal UI adds a read-only `/plugins`. Components are namespaced
  `plugin:name`, and `${CLAUDE_PLUGIN_ROOT}` expands to the installed checkout so existing plugins
  port by copying.
- User hooks: `pre_tool_use`, `post_tool_use` and `permission_denied` command hooks with Claude
  Code's JSON input, output and exit codes; a repository's hook can tighten but never allow.
- Skills, agent files and output styles with Claude Code's frontmatter, the `skill` tool, skills
  as `/name` commands with arguments, `` !`cmd` `` context run as gated `shell` calls, and
  `allowed-tools` lent for a turn.

#### Guardian

- `/guardian-disable` and `/guardian-enable` commands (development escape hatch, #271). The
  disable command asks for confirmation, then turns the guardian and the deterministic permission
  floor off for the rest of the session, with a warning chip in the status bar until re-enabled.
  It is interactive-only and session-scoped: it is never saved to settings, a project cannot set
  it, and subagents are never loosened by it. Every toggle is recorded in the session ledger and
  the audit log. This is a deliberate deviation from the security invariants, recorded in the
  master plan's decisions.
- A schema-constrained verdict path makes an Ollama model with no tool support usable as the
  guardian and as the `llm` scanner model (#250). Fontaine asks the daemon for the model's
  capabilities and, for a model that can generate text but reports no `tools`, sends the verdict's
  JSON schema as the daemon's `format` instead of a forced `submit` call; a model that reports
  `tools`, a daemon too old to report capabilities and every other provider are unchanged. The
  schema path is announced once, an embedding-only model is reported as an unavailable guardian
  with its capabilities, a `400 ... does not support tools` denial now says what is missing, and the
  audit log gains `verdict_transport`. `fontaine-evals --verdict-transport tool|schema|auto` scores
  either path, and `guardian_models.toml` entries may record the `transport` they were measured
  on. The list now has its first entry, `claude-haiku-5-5` on an `opencode-go` account (99.2%
  catch, 3.9% false-deny, p95 2.4 s), which an `opencode-go` account that serves it uses as the
  default guardian when none is set; the scorecards of the other local and hosted models are in
  `evals/scorecards/`.
- `fontaine auto-mode defaults|config|critique` inspects the rules the guardian judges against.
  `defaults` prints the shipped lists as a TOML fragment, `config` prints the effective lists with
  the scopes that set each and whether each entry is shipped or custom, and `critique` asks the
  guardian's model to review your custom entries (advisory text; it is the only one that calls a
  model). Each takes `--format text|json` (#1).
- An optional `[guardian.screen]` stage (user file only) asks a small local Ollama model one
  yes/no question per call in `auto` and `plan` mode and reads `p(yes)` from its first-token
  logprobs. A call it confidently judges outside the user's request is escalated with
  `tier = screen`; every other call, including one the screen could not judge, goes to the
  guardian as before. (Superseded by the allow path below: the screen can now allow, but only
  when `allow_threshold` is set.) `stop_threshold` has no default, and `shadow = true`
  (the default) scores and records every call without changing a decision. The audit record,
  the `fontaine.guardian.screen` span and the `fontaine.screen.count` metric carry the result (#130).
- The screen can now allow a call without the guardian, superseding "the screen never allows".
  It is off unless `[guardian.screen] allow_threshold` is set (user file only), and only when a
  steering-hardened permission read is below that threshold and a steering detector is below
  `steering_veto` (default `0.001`). The audit record, the headless `gate_decision` event and the
  `fontaine.guardian.screen` span gain `allow_risk` and `steering`; an allow is `tier = screen`,
  `decision = allow`, and `fontaine.screen.count` gains an `allow` outcome. Shadow mode logs the
  allow scores to calibrate against (#135).
- The screen and the guardian now start together (`fontaine_guardian::cascade`), so a call the
  screen does not settle costs the slower of the two (the screen is bounded by `timeout_ms`), in
  practice the guardian's. A settled call drops the guardian request: its
  tokens are not counted in usage. A guardian verdict already in hand always wins (#135).
- `auto` mode: a separate, tool-less model judges each call that nothing pre-approved and
  answers allow, deny with a labelled reason, or escalate to the human. Every outcome that is
  not a verdict (missing credential, provider error, timeout, malformed verdict, a
  low-confidence allow on a destructive call) is a denial.
- Its request is built from the session ledger only, with a structurally derived destructive
  class, delimited untrusted blocks and redaction; a 60-second verdict cache, a loop breaker
  (3 in a row, 20 in total) and seeded default rule lists adapted from Claude Code's auto-mode
  defaults, including remote data stores, production hosts, secret managers and CI bypass under
  the existing rule labels.
- The `minimal` profile for containers, loaded only with `--in-container` and a positive runtime
  probe.
- Default guardian selection from the ranked list of validated models (`guardian_models.toml`,
  empty until a scorecard run fills it), with a main-model fallback and a one-time notice;
  `/guardian-model` switches it live and saves the choice.
- Denials render as `[Rule] explanation`; `/denied` lists them and offers a one-shot retry
  approval; an unavailable pinned guardian prompts to retry, pick another or fall back to
  manual.
- The tool-result scanner, selected by `[guardian] scanner` (`off` by default, `pg2`, `llm` or
  `command`; user file only). A result it flags, or cannot scan, is kept and followed by a note
  telling the agent it is possible prompt injection or unscanned and suspect; subagent reports
  pass the same stage. `pg2` runs PromptGuard 2 (86M) locally through candle, scoring a whole
  result in 512-token windows, behind the `pg2` cargo feature (on in release binaries,
  `--features pg2` from source). Its gated weights are never bundled: a local
  `scanner_model_path`, or a download with the token in `scanner_hf_token_env` (default
  `HF_TOKEN`, always removed from `shell` and hook environments), with the Llama 4 Community
  License notice shown on every startup in every mode. `llm` uses the guardian's model;
  `command` runs your own program (stdin `{tool, content}`, stdout `{verdict, reason}`).
- `fontaine auto-mode setup` (and `/auto-mode-setup` in the terminal UI) collects a project's
  environment once and stores it in the data directory: repo visibility, default branch
  protection and deploy environments from the authenticated GitHub API through `gh`, tag-release
  workflows, workflow environments, package-registry hosts and `.env*` files from repository
  files, and, in a trusted workspace, sensitive locations, deploy targets, protected targets and
  trusted destinations proposed by a model scan on the guardian's model. The user reviews the
  scan's proposals before anything is saved. Sessions load the record without a network call and
  give the guardian its facts: a `public` remote relaxes data-exfiltration only for tracked
  content, an accepted trusted destination counts as a trust-slot entry, and a `public` fact
  expires after 30 days. An `auto` session without facts warns once to run the command, and
  `[auto_mode] setup_reminder = false` (user scopes) silences it. `/auto-mode-setup` applies
  to the running session at once. `fontaine auto-mode config` shows the stored record (#316).

#### Headless

- `fontaine -p` with `--output-format text|json|stream-json`, `--input-format stream-json`,
  `--include-partial-messages`, `--max-turns`, `--max-budget-usd`, `--json-schema`,
  `--system-prompt`, `--append-system-prompt`, `--model` and `--permission-mode`. The output is
  a versioned contract with a JSON Schema in `docs/schemas/headless-v1.schema.json`; exit codes
  are 0, 1 and 2. `total_cost_usd` and `--max-budget-usd` include guardian and subagent spend, and
  `result.usage` breaks tokens and cost down into `main`, `guardian` and `subagents`. Notices go
  to stderr.
- `result.usage.guardian`, `subagents` and `goal` report `cache_read_input_tokens` and
  `cache_creation_input_tokens` like `main` (#331). The fields are required on every usage bucket
  in `headless-v1.schema.json`, an additive change for consumers of the contract.

#### MCP

- MCP servers over stdio and streamable HTTP from `[mcp.servers.*]`, registered as wrapped
  tools named `mcp__<server>__<tool>`. Server annotations reach the guardian only as untrusted
  text, and a repository's servers need workspace trust and the user's approval of each.
  A tool a server marks with `_meta["anthropic/requiresUserInteraction"]: true` asks on every
  call, whatever the mode or allow rules, and is denied in `dontAsk` and headless runs (#2).

#### Observability

- Backends: micromegas (native span trees) and OTLP/HTTP, with token and cost metrics, an
  `api_request` event, a target allowlist, opt-in content that is redacted and truncated, and a
  capped flush on exit. Exported are Fontaine's own target (`fontaine.tool`,
  `fontaine.permission`, `fontaine.guardian`, `fontaine.subagent`, `fontaine.hook`,
  `fontaine.mcp.call` and the session, decision and guardian metrics) and the model layer's
  completion and agent targets.
- `query_source` separates the guardian's, the scanner's and subagents' model calls;
  `tool_result` carries `scanner_verdict` and `scanner_latency_ms`; `fontaine.session.count`
  carries `start_type` and `headless`.
- `subagent` and `mcp_server_connection` events. MCP server and tool names read `third-party`
  unless `log_tool_details = true`.
- An optional owner-only debug file (`--debug-file`, `FONTAINE_LOG`).
- `api_error` (the error class and HTTP status of a failed model call, never the message) and
  `internal_error` (an audit or ledger write failure, by class) events. `permission_mode_changed`
  and the `screen.*` fields of `tool_decision` are documented (#139).
- Guardian spend dropped when an enforcing screen settles a call is estimated from the
  rendered request and counted in `fontaine.token.usage` and `fontaine.cost.usage` with
  `estimated = true`; the audit record carries the estimate as `guardian_dropped_*` (#139).
- A micromegas notebook under `analytics/notebooks` for cost, guardian latency and fail-closed
  reasons, decision tiers, screen outcomes, tool results, subagents and errors, plus a
  per-stream-tag retention recipe and the `tool_use_id` join with the audit log in the docs (#139).
- The micromegas backend turns CPU tracing on through the sink builder and no longer sets
  `MICROMEGAS_ENABLE_CPU_TRACING` in Fontaine's environment. micromegas is pinned to a git
  commit until 0.33 is published (#139, #141).

#### Permissions

- A read of a secret file inside the workspace (`.env` and `.env.*` except the example, sample and
  template files, `*.env`, credential dotfiles, SSH private keys, `*.pem`, `*.key` and other key
  containers) no longer takes the read-only fast path or the shell pre-classifier skip. It is
  asked about in `manual` and `acceptEdits`, denied in `dontAsk` and headless runs, and judged by
  the guardian in `auto` and `plan`; an `allow` rule for the file restores the skip (#370).

- A spawned agent whose tools are all read-only (`explore`, or a type with no tools) starts
  without a prompt or a guardian call, in every mode, after the deny and ask rules and hooks;
  every call the child makes is still gated. A spawn whose parent toolset is unknown is not
  fast-pathed (#302).
- In `auto`, file-tool edits to git-tracked files, new files and files the session created inside
  the workspace, and file-tool writes into the session scratch directory, no longer wait for a
  guardian verdict (tiers `tracked_edit` and `scratch_write`, after the deterministic floor), and
  they end a run of consecutive guardian denials. Instruction files, `.github/`, existing untracked
  files and every shell command still go to the guardian. `[permissions] auto_tracked_edits` and
  `auto_scratch_writes` turn them off; project scope may only set them to `false` (#256).
- Each session has a private scratch directory, added to the workspace roots automatically
  (`$XDG_RUNTIME_DIR/fontaine/<session id>/`, else an owner-only per-user directory under the temp
  dir). `write` and `edit` work there without a prompt in `acceptEdits`, the system prompt names it,
  and the out-of-roots denial points at it. It is removed on exit and swept after a crash; the
  floor, the guardian and `shell` handling are unchanged (#238).
- The modes `manual`, `acceptEdits`, `plan`, `dontAsk` and `auto`; Shift+Tab cycles `manual`,
  `acceptEdits`, `plan` and `auto`, and `dontAsk` is set through settings or
  `--permission-mode`. `plan` blocks every edit and sends consequential non-edit calls to the guardian.
- A deterministic floor in every mode: deny rules, ask rules, critical-path `rm`/`rmdir` (also
  reached through shell runners, launchers, `find -exec`, `xargs` and MSYS-style paths) and
  protected paths, including Fontaine's own config and data directories. A file-tool call outside
  the workspace roots is denied with an `/add-dir` hint.
- Allow, ask and deny rules that carry their source scope, session grants, and "always allow"
  written to the user settings file. In `auto`, broad code-execution allow rules are suspended:
  a bare `shell` or `agent`, interpreters, shells, package runners (`cargo run`, `npm run`,
  `npx`, `make`), `sudo`/`doas` and argv launchers, and any wildcard that can reach them.
- A shell pre-classifier on a real shell parser with a per-command flag allowlist and
  `[shell_preclassifier] allow_extra`; anything it cannot fully parse is never pre-approved: it
  falls through to the rules and the mode, and goes to the guardian in `auto` and `plan`.
- An append-only audit log per session, `tool_decision` events, `fontaine.permission` spans, the
  `fontaine.tool_decision.count` metric, headless `gate_decision` events and `result.denials`,
  and `--allowed-tools` and `--disallowed-tools`.
- Workspace roots from the primary directory, the repeatable `--add-dir` flag, `/add-dir` and
  `[permissions] additional_directories`, with one confined tool instance per root.

#### Providers

- The agent loop runs over rig. Models come from `[accounts.<name>]` entries mapped to provider
  clients (Anthropic, OpenAI, Gemini, Ollama, OpenAI-compatible endpoints) and are named
  `<account>/<model>`, with model discovery. Credentials come from an environment variable or
  the stored-credentials file, which the terminal UI's `/login` writes.
- An `opencode-go` account kind reaches the OpenCode Go subscription's models over their four
  wire formats with one sign-in (`/login <account>`, the opencode CLI's device
  flow). Access tokens refresh silently, requests carry a `fontaine/<version>` User-Agent and
  the session id, and the models are unpriced. Account kinds now sit behind a provider
  registry, so a new kind is one module and one registry line (#93). The sign-in resolves the
  relative verification URL the Console sends against the Console, rather than refusing it (#118).
- A model request that cannot connect names its transport cause (connection refused, reset,
  TLS, DNS, timeout) without the request URL (#88).
- Ollama accounts load models at a configured context window (`context_tokens`, default 16384,
  sent as `num_ctx` by the agent, the guardian and the screen) instead of the daemon's 4096
  default, and send `truncate: false`, so an over-long prompt is an error (a fail-closed Deny for
  the guardian) rather than silently cut. `think = false` turns thinking off. Compaction fits
  its limits to small windows (#96).
- Anthropic accounts, and `opencode-go` models on the Messages wire when opted in, send cache
  breakpoints on the tools, the system prompt and the latest message. `prompt_caching` (on for
  `anthropic`, off for `opencode-go`) and `cache_ttl` (`"5m"` or `"1h"`) are user-scope account
  keys. The guardian's request marks only its static prefix (#13).
- Anything before the latest message is append-only within a session, an MCP server's tools are
  registered sorted by name, and the headless `usage.main` reports cache read and write tokens (#13).
- A `[network]` section (user file and `--settings` only), repeatable per account, sets an HTTP
  or HTTPS proxy with a `no_proxy` list, extra CA roots (`ca_bundle`) and a client certificate
  (`client_cert`, `client_key`) for model accounts, the guardian and its screen, `webfetch`, the
  scanner-model download, the `opencode-go` sign-in and HTTP MCP servers. An invalid setting is a
  configuration error (exit code 2), never a fallback to a direct or default-trust connection (#6).
- An `openrouter` account kind reaches OpenRouter on its own endpoint and sends prompt-cache
  markers: one top-level marker on the conversation, and a marker on the system prompt for the
  guardian. Upstreams that cache by themselves (`openai/`, `deepseek/`, `x-ai/`, `groq/`) are sent
  none. `prompt_caching` (on by default) and `cache_ttl` apply to it. An `openai-compatible`
  account pointed at OpenRouter keeps working, uncached (#331).
- A pinned Claude guardian whose model caches nothing below a length its prompt does not reach
  (Haiku 4.5, Opus 4.5 and 4.6, and others, under the `minimal` profile, on `anthropic` and
  `openrouter` accounts) gets a start-up notice instead of a silent no-op (#331).

#### Research

- A docs-based study of Claude on Microsoft Foundry and Databricks Model Serving, with a
  central list of model-specific support and follow-ups #165–#170 (#157).
- A study of the oh-my-pi harness, with experiments in Docker behind a recording proxy and a tmux
  comparison of its terminal UI with Fontaine's: what lets the model iterate, what its safety posture
  gets wrong, and the follow-ups (#213).
- A study of the Hermes harness and its cron, watcher and gateway orchestration, with Docker
  experiments against a local Ollama model and a fake Grafana and TeamCity service: what makes a
  cheap unattended watcher work, where small models fail, and the follow-ups #258–#264 (#253).

#### Sessions

- Checkpoints and `/rewind` (alias `/checkpoint`, or Esc Esc at an idle empty prompt). `write` and
  `edit` save a file's contents before changing it, filed under the prompt being answered, and
  `/rewind` restores the code, the conversation or both to the start of an earlier prompt of the
  conversation. Shell, `!` and MCP changes are not tracked; files over `[checkpoints]`
  `max_file_bytes` or past `max_session_bytes` are skipped with a warning; files edited since
  Fontaine's last change ask before being overwritten; the memory directory is never touched. The
  ledger is never truncated: a `rewound` record trims only the guardian's view of what you asked.
  Summarize-from-here is not offered (#12).
- Sessions live in a per-session directory under the data directory, with the conversation
  persisted by rig's memory store, between-runs compaction by a summarizing call and an
  append-only ledger. `--session-id`, `--continue`, `--resume <id>` and `--fork-session` select
  or branch a session.
- `/loop <interval> <prompt>` re-runs a prompt on a wall-clock cadence and `/loop <prompt>` lets
  the model pace itself with `schedule_wakeup`; `/loop list` and `/loop stop [id]` manage them.
  The tools `cron_create`, `cron_jobs`, `cron_delete` and `schedule_wakeup` are main-conversation
  only; `cron_create` is an arbitrary tool the guardian judges. Due prompts run between turns as
  harness notes, never as user messages, so they do not widen the guardian's view of what you
  asked. Tasks persist in `schedule.json`, come back on resume without replaying missed runs, are
  capped at 50 and expire after 7 days, and `/clear` cancels them. `[schedule] enabled` (user
  scope) turns the feature off (#16).
- Sessions can be named with `--name`/`-n` or `/rename [name]`; `--resume` takes an id or a name (#10).
- `/resume [name-or-id]` opens a picker of this folder's sessions, `/branch [name]` (`/fork`)
  copies the live session and carries on in the copy, and `/export [path]` writes the
  conversation as an owner-only Markdown file. Switching sessions drops grants and returns to
  the default mode, like any resume (#10).
- Context commands: `/compact [focus]` summarizes the conversation on request, `/clear`
  (`/reset`, `/new`) empties the model's context while the session, its permissions and its
  mode carry on, and `/context [all]` shows estimated token use by source. `[context]
  autocompact_window` sets how many tokens of messages trigger automatic compaction, in the
  terminal UI and headless runs (#11).

#### Tools

- Multimodal `read`: an image (PNG, JPEG, GIF or WebP, sniffed by magic bytes) is returned to the
  model as an image content part when the current model accepts images — a text header names the
  file, kind and size, then the image, capped at 5 MB — and as a text note otherwise. A PDF is never
  returned as a tool result: the result is a stub naming the file and pointing at attach (#21).
- The terminal UI attaches a clipboard image with Ctrl+V and a dropped PDF by dropping its path on
  the prompt. Attachments render as chips above the typed text (Backspace on an empty input removes
  the last one) and ride the message as base64 content parts: an image to a model that accepts
  images, a PDF as a document, anything the model rejects as a text note instead. The ledger
  records attachment labels with each user message (#21).
- Background shell jobs and a `monitor` tool (#25). A main-conversation `shell` command still
  running at its `timeout_secs` (or, with the default timeout, after 60 s without output) is no
  longer stopped: the call returns the output so far and a job id, the command keeps running, and
  the main conversation is told when it ends in a notification that starts a turn of its own.
  `run_in_background` starts a command as a job at once, `job_output` lists jobs and reads new
  output (`wait_secs` replaces polling with `sleep`), `job_cancel` stops one, and `monitor` delivers
  each line a command prints as a notification. Job output is untrusted data and passes the
  tool-result scanner like any result; the guardian still judges the command when `shell` or
  `monitor` is called, a `shell` deny or ask rule and hook matcher also cover `monitor`, and the
  limits (8 running jobs, 1 MiB buffer, 64 MB output, no way for the model to lift the
  `[shell] max_timeout_secs` timeout, 600 s by default) are the harness's. Subagents keep
  kill-at-timeout; jobs end with the session, and the agents panel lists and kills them.
  `BashOutput`, `TaskOutput`, `KillShell`, `KillBash`, `TaskStop` and `Monitor` map to the new
  tools.
- `notebook_edit` edits one cell of a Jupyter notebook (`replace`, `insert` or `delete`, by cell id
  or `cell-N`); a code cell's outputs are cleared when its source is replaced. `read` shows a
  `.ipynb` as its cells, ids and text outputs, `NotebookEdit` rules and `allowed-tools` entries now
  map to it, and it joins the `worker` subagent type. It is a workspace write: `acceptEdits` runs
  it, `plan` blocks it, and the protected-path floor applies (#20).
- Web search. An `anthropic` account on the Anthropic API can use the provider's own search; every
  other account gets a `websearch` tool: by default a SearXNG container that Fontaine starts on the
  first search when `docker` or `podman` is installed, or the Tavily or SearXNG instance that
  `[websearch] backend` names (`command` starts your own on demand; `backend = "off"` disables it). The tool takes `query`,
  `allowed_domains` and `blocked_domains`, returns titles, URLs and snippets, and is judged by the
  permission pipeline and scanned like any other call (`WebSearch` in skill and agent files maps to
  it). Provider-native search runs inside the model's response, so the guardian, the scanner and
  `pre_tool_use` hooks do not see it. It is withheld while a `deny` or `ask` rule covers
  `websearch`, while a `pre_tool_use` hook could cover it, and in `manual`, `acceptEdits` and
  `dontAsk` unless an `allow` rule exists (so `fontaine -p`, which defaults to `manual`, has none
  out of the box); `[websearch] native = "off"` turns it off. The Tavily key is `api_key_env` or
  the `[websearch]` entry of `credentials.toml` (#19).
- The `todo_write` tool keeps a session task list the model replaces whole on each call. It is
  read-only and takes the fast path, and is kept in `todos.json` in the session directory so
  `--continue`, `--resume` and `--fork-session` bring it back. The terminal UI shows it above the
  input, collapsed to one line or expanded (Ctrl-O or `/todos`); the screen-reader mode prints
  it as plain lines when it changes (#22).
- `ask_user_question` tool: the model can ask you 1 to 4 multiple-choice questions, each with an
  "Other" row for your own words. The terminal UI shows a dialog; a headless run returns "user
  input is unavailable, proceed with your best judgment". Only the main conversation can ask, no
  permission mode answers for you, and the text you type under "Other" counts as guardian intent
  while the options you pick do not (#23).
- The built-in tools `read`, `ls`, `glob`, `grep`, `write`, `edit`, `shell` and `webfetch`. Every
  tool has a risk class and sits behind one gate, and each call emits a `fontaine.tool` span and
  a `tool_result` telemetry event.
- `shell` child processes do not inherit telemetry credentials, configured accounts' API key
  variables or the scanner token variable. On Windows the shell runs through Git Bash, found on
  `PATH`, in the standard install locations or through `FONTAINE_GIT_BASH_PATH`, and commands
  reach it unchanged, backslashes included (#78).
- `webfetch` sends the User-Agent `fontaine/<version> (+https://github.com/madesroches/fontaine)`
  (#73) and `Accept: text/markdown, text/html, */*` (#85).
- `job_output` on a monitor id is no longer refused: it waits up to `wait_secs` (a zero wait
  asks for the status at once) and reports whether a notification was posted, the monitor
  ended, or it still runs — never carrying output bytes, since a monitor's lines and its final
  notice stay on the notification path, delivered exactly once (#304).
- A Windows-only `powershell` tool (Claude Code's `PowerShell`) runs commands in `pwsh.exe`, else
  `powershell.exe` (`FONTAINE_POWERSHELL_PATH` overrides discovery), alongside `shell`, which stays on
  Git Bash. The command reaches the interpreter encoded, so PowerShell parses exactly the text the
  guardian judged. It is registered only when an interpreter is found, is never pre-classified (every
  call is judged), takes bare-name rules only, and is covered by bare `shell` deny and ask rules. The
  critical-path floor now reads `Remove-Item` and its aliases and .NET `Delete` calls, treats any nested
  `cmd`, `pwsh`, `powershell` or `iex` invocation as critical, and stops any it cannot resolve for a human in every mode (#28).
- `[shell] max_timeout_secs` (1 to 86400, default 600; user file or `--settings` only) sets the
  ceiling on a model's `shell` timeout: how long a main-conversation call blocks before it moves
  to a background job, and how long a subagent's or skill injection's command runs. No tool
  argument lifts it, and `/config` can edit it (#267).
- An `lsp` tool asks a language server for a symbol's definition, implementations, references,
  hover text, a file's or the workspace's symbols and the call hierarchy, and a note on each
  `edit` or `write` result lists the errors and warnings the server found in the new content;
  ones that arrive later ride the next tool result and never start a turn. Servers are defined
  under `[lsp.servers.<name>]` in the user file or `--settings` only, or by an installed
  plugin's `.lsp.json` / `lspServers`; the tool exists only while one is configured (#27).

#### TUI

- The model's reasoning shows in the transcript as a dim `thinking` card that streams its last five
  rows live and folds to its first five once it settles; `/expand thinking` prints it in full.
  `[tui] show_reasoning = false` hides it, and `tui.show_reasoning` is a row in the settings panel.
  Headless `-p` and `stream-json` still carry no reasoning (#344).
- The question dialog's "Other" row is a first-class free-text answer: the field is always
  visible inline on the row with the caret while it is highlighted, typing a character anywhere
  drops straight into it, and text typed alongside a chosen option is submitted with it as a
  note — recorded exactly like standalone "Other" text, so the guardian reads it as intent
  (#295). The separate typing mode and its footer field are gone; Esc on the "Other" row moves
  the highlight back to the options, keeping the text.
- The message box edits by word and keeps a kill buffer: Ctrl/Alt-Left/Right and Alt-B/F move by
  word, Alt-D and Ctrl-Delete delete the next word, Alt/Ctrl-Backspace the previous one, Ctrl-W
  the previous whitespace-delimited run, Ctrl-Home/End jump to the buffer's ends, and Ctrl-Y pastes
  the last text Ctrl-U/K/W or a word delete removed. Ctrl-H is Backspace, Ctrl-D deletes the
  character under the cursor when the input has text, Ctrl-P/N recall history like Up/Down, and
  Ctrl-L redraws the screen from the transcript, erasing the terminal's scrollback (earlier
  terminal content and rows from before `/clear`), and does nothing in screen-reader mode. The
  deny-reason and question fields get the same keys (#257).
- Tool calls in the transcript are cards: a long or multi-line command folded to three rows, the
  last ten rows of the output behind a gutter, and for `edit` and `write` the diff of the change,
  also when no prompt asked about it. `/expand [n]` prints a call in full as new lines. Output is
  sanitised and redacted before it is kept, and at most 64 KiB of a result is retained (#222).
- The approval prompt for `edit` and `write` shows the change as a diff (line numbers, `+`/`-`
  lines, three lines of context, scrollable) before you decide; in the linear renderer it is plain
  text. It is display-only and absent when the file cannot be read or the edit would fail (#45).
- The terminal UI (`fontaine` without `-p`) draws inline, so finished output goes into the
  terminal's scrollback: transcript, streaming output, scrollable approval prompts, mode cycling,
  `/model`, `/guardian-model`, `/output-style` and the workspace trust prompt. It exits with code
  2 when there is no terminal.
- `!` runs a shell command as the user, outside the permission pipeline and recorded in the
  ledger and the audit log; `@` completes workspace file paths; Up/Down and Ctrl-R recall and
  search a persistent prompt history; messages typed during a turn are queued and delivered with
  the agent's next tool result (#46).
- `/model` and `/guardian-model` without an argument open a picker: models grouped under their
  account, recently used ones first, a fuzzy filter, and the current model marked (#117).
- `/login` opens a provider dialog that lists the accounts with their kind, endpoint and sign-in
  state, adds an account to the user settings file, signs in (device sign-in or a masked key
  prompt), signs out and removes accounts; the change takes effect in the running session. When
  the interactive UI cannot resolve its main model at startup, the same dialog opens before the
  session instead of exiting. `fontaine auth login` is removed (#140).
- More of Claude Code's built-in commands, under the same names: `/help`, `/status`,
  `/permissions` (the effective rules with their source), `/mcp`, `/hooks` and `/skills` print
  read-only reports; `/plan [description]` switches to plan mode and optionally starts the task;
  `/copy [n]` copies a reply to the clipboard over OSC 52; `/tasks` opens the agents panel. One
  command table drives both parsing and the `/` menu, and a built-in hides a skill of the same
  name, with a startup notice (#47).
- The `/` menu is interactive: Up/Down select, Tab completes the name, Enter runs the selection
  (or completes a command that needs an argument) and Esc closes it. Matching covers names and
  aliases, prefix matches first and then substrings, and the list shows at most eight rows
  (#71).
- Startup warnings are styled conversation entries (#82). Notices carry a severity and split
  into a headline and an actionable hint: a warning starts with `⚠` in the theme's warning
  colour, with the hint dimmed underneath and its backticked spans and `/commands` in bold;
  informational notices keep the dim `·` line. The configuration-root warnings and an ignored
  `FONTAINE_AX_SCREEN_READER` value, which used to print to stderr before the screen opened, now
  open the conversation, and the screen-reader renderer labels them `warning:` and `hint:`.
  `FONTAINE_ASCII_GLYPHS=1` switches the marks to `*` and `-`. A short transcript now sits
  directly above the input box instead of at the top of the screen: lines move to scrollback only
  once they no longer fit, and leaving the UI or `/clear` writes the held ones there. The
  headless stderr text of the guardian-on-main-model notice is now "headline — hint".
- `/config` (alias `/settings`) opens a settings panel listing every settings key with its
  effective value and the scope that supplied it (`default`, `user`, `project`, `project-local`,
  `cli`). It edits `tui.theme` (at once), `tui.screen_reader`, `context.autocompact_window` and
  `schedule.enabled` (from the next launch) and, through the pickers, `model.model` and
  `guardian.model`; every other key is read-only. Changes go only to the user settings file,
  which is validated before it is written, and a higher scope that wins the key is named
  instead of being written to. `/model` and `/guardian-model` now say when a higher scope wins
  the key they saved (#4).
- Terminal polish (#50): six colour themes (`dark-ansi` by default, `light-ansi`, `dark`,
  `light`, `dark-daltonized`, `light-daltonized`) chosen with `/theme` or `[tui] theme`; a
  screen-reader mode (`--ax-screen-reader`, `FONTAINE_AX_SCREEN_READER`, `[tui] screen_reader`)
  that prints labelled linear lines, announces dialogs, selections and mode changes, echoes what
  you type and rings the bell when a reply ends or a prompt needs an answer; and
  `docs/terminal.md` on newline keys, tmux, Option as Meta, `NO_COLOR` and OSC 52. There is no
  `/terminal-setup` command.
- Input typed while a turn runs now queues and executes once the turn ends, per Claude Code's
  documented behavior (#292). A skill invocation (`/review 42`) queues as typed and runs as its
  expanded prompt; a built-in command queues and executes through the same dispatch it would get
  at an idle prompt, in the order typed, and a `!` shell command queues and runs after the turn,
  one at a time. Neither a queued skill nor a queued command or shell line is ever steered into
  the running turn — a message queued behind one waits with it. The synchronous UI commands
  (`/status`, `/theme`, the panels, `/help`, …) still run immediately mid-turn, a queued command
  that opens a picker or dialog holds the later queued entries until it closes, and `/resume` and
  `/branch` are refused while later entries remain so a session switch cannot discard them.
  `/loop` and unknown slash names keep their existing mid-turn refusal.
- Assistant replies in the terminal UI render markdown: bold, italic and inline code, headings,
  bullet and numbered lists, fenced code, and tables with aligned, wrapped columns. Anything
  else shows as its source text, and `/copy`, `/export` and line mode keep the exact source. A
  streaming reply is written to scrollback a block at a time, so a paragraph appears as source
  until it ends (#328).
- A working spinner row sits above the message box while a turn, goal evaluation, `/compact`,
  `!` command or skill expansion runs, and in a running agent's view: a breathing-dot glyph whose
  hue drifts, the label `Working…` with a shimmer sweeping across it, the elapsed time and `↓` output
  tokens (estimated from streamed text until each model call reports its count). It hides while an
  approval, question or guardian-unavailable prompt waits on you, and the status bar keeps `esc to
  interrupt` without the word `working`. A 16-colour theme gets a stepped shimmer and a glyph that
  cycles the theme's accents instead of a gradient; `FONTAINE_ASCII_GLYPHS=1` draws `.oO0Oo`. There is
  no spinner in screen-reader mode (#355).

### Changed

#### Agents

- Subagents are no longer capped at 200,000 tokens or $2.00 by default (#285). A subagent now
  runs until its task is done — matching Claude Code — instead of dying mid-work with a partial
  report on the largest delegations. `[agents] subagent_token_budget` and
  `[agents] subagent_cost_budget_usd` stay as opt-in per-subagent caps; the session budgets and
  `--max-budget-usd` are unchanged.
- `job_output`'s `wait_secs` no longer has a 60-second ceiling: the model chooses the timeout.
  An omitted value still reads a job now and waits for a subagent to end. Any wait returns
  early when a message from another agent arrives or, in the main conversation, when the user
  types one (#301).
- `job_output` on a shell job now waits until the job ends or `wait_secs` elapses instead of
  returning at the first new output, so a chatty job (`gh pr checks --watch`) no longer cuts a long
  wait short. The call returns everything printed since the last read; `monitor` still delivers
  lines as they come (#336).
- Subagents work as in Claude Code (#321). While any subagent exists, a list below the status line
  shows `main` and each agent with its status, age and spend; Down on the prompt focuses it, Enter
  views a row and x stops a running agent. Viewing an agent clears the screen and scrollback and
  reprints that agent's transcript, drawn like the main chat; viewing `main` reprints the main
  conversation. Text typed in an agent's view goes to that agent: a running one reads it at its next
  tool result, and a `done` or `failed` one is resumed with it, reporting back to its parent as a
  background agent does. Killed and budget-exhausted agents cannot be resumed. A message typed to an
  agent is guardian intent only for that agent and its descendants (the ledger's `user_message`
  record gains an optional `agent`). A subagent's decisions and denials now show only in its own
  view. The agents panel's Enter opens the view in place of the old flat detail list (#299).
- Every `agent` spawn is asynchronous (#326). The call returns `{"agent_id": …}` at once, so several
  spawns in one turn run concurrently with each other and with their parent; the report arrives as
  a notification, or the parent waits for it with `job_output`. The `background` argument is gone.
  A subagent that answers while its own subagents still run waits for their reports and takes a
  further turn on them before it ends, instead of having them killed; only a killed, failed or
  budget-exhausted subagent's descendants are stopped. Interrupting a turn no longer kills any
  subagent, and a failed subagent's reason now reaches its parent's report.

#### Build and release

- CI runs faster: the pg2 clippy and test legs and the fixture eval runs are Linux-only (macOS and
  Windows still run `cargo check --features fontaine/pg2` and the `--guardian none` eval), the
  Rust cache is dropped (measured slower than a cold build on every OS), tests run under
  `cargo nextest`, pull-request runs cancel when superseded, debug info in the dev and test
  profiles is reduced to line tables, and a PR touching only `tasks/` or `AGENTS.md`
  skips the Rust steps (#178).
- `[Unreleased]` entries are grouped by subject under each change type, and a new entry goes at
  the end of its subject's section, so pull requests on different subjects no longer conflict in
  this file; `CONTRIBUTING.md` describes the convention (#318).

#### Config

- Which scopes may set a settings key is now one declarative table in `fontaine-core`, and a key
  with no entry is refused from project and project-local files instead of accepted. No existing
  scope decision changes; a scope error now names the table entry's key, which can be a section
  such as `guardian.screen.model` or `telemetry.endpoint`.

#### Core

- A poisoned lock now ends the session wherever its contents feed a permission or guardian decision,
  a spend tally, the model in use or a durable record, instead of being silently recovered; only
  display and advisory state (the `@`-file index, steering buffers, telemetry tallies, a child's
  partial text) still recovers. One helper module, `fontaine_tools::lock`, holds the policy (#186).
- `AgentRuntime` no longer keeps a second copy of the permission mode and workspace roots:
  `mode()` and `roots()` read the live session state, `start_mode()` is the mode the session
  opened in, and `agent.rs` is split into `setup`, `build`, `models` and `mcp` submodules with a
  clippy line-count limit and one builder configuration shared by the root agent and subagents
  (#187).

#### Guardian

- The guardian prompt no longer mentions the removed `background` spawn argument (#326).
- The guardian's composition is a Rune script: `[guardian.composition]` (user file only) replaces
  `[guardian.screen]`, with `preset = "screen"` or `script = "<file>"`, named logprob `reads`,
  free-form `params` and `shadow`. The script orchestrates host leaves (`read`, `llm`,
  `first_decisive`); only host functions mint verdicts, and the Rust root fails closed on errors,
  the instruction budget, timeouts, abstain and non-verdict returns. A finished LLM denial in hand
  always wins, and an unavailable guardian skips the script. The `screen` preset is now an allow
  screen in front of the LLM guardian, sequential by default (`concurrent = true` races them), and
  a cancelled call cancels its reads and LLM request. Audit records gain `screen_scores`, and
  `fontaine-evals` gains `--composition-script` and `--composition-param` (#137, #136).

#### Headless

- A turn no longer stops at 1,000 model calls when `--max-turns` is not given; interactive
  turns and subagent runs have no call limit either, as in Claude Code. The token and cost budgets
  still bound a run (#326).

#### Permissions

- Gated out-of-root reads (#284). `read`, `ls`, `glob` and `grep` can now inspect files
  outside the workspace roots through the normal permission pipeline instead of being
  refused for containment: the call asks in `manual`/`acceptEdits`, the guardian decides in
  `auto`/`plan`, and `dontAsk` and headless runs deny it unless an allow rule or hook covers
  it. The target is resolved once after the hooks and the guardian, the approval prompt, the
  audit record and the execution all use that resolved form, so retargeting a symlink while a
  decision is pending cannot redirect an approved call. Execution runs on a dedicated
  unconfined, read-only fallback instance reached only after the gate allows the call; writes,
  `notebook_edit` and `shell` stay confined, and an external path never becomes a workspace
  root (no write reach, no fast path, no instruction discovery). The guardian's shipped allow
  list treats source, header, build-file, installed-dependency and installed-skill-definition
  reads outside the roots as routine, while credential stores and scouting around a denial
  remain denied. `grep` and `glob` approval prompts now show the search path as well as the
  pattern. The eval dataset gains the external-read slice; its version changes, so existing
  scorecards are re-baselined by the next live run.
- An `agent` spawn is no longer judged by the guardian: in `auto` and `plan` the gate allows it at
  the new `spawn` tier after the deterministic floor and hooks, so a benign worker prompt can no
  longer escalate an unattended run to a human (#360). Every call the child makes is still gated,
  `manual`/`acceptEdits` still prompt and `dontAsk` still denies. The audit record's `explanation`
  is now also kept for guardian allows and escalations.

#### Tools

- Each tool's decision inputs (risk class, path containment, destructive policy, rule specifier
  and Claude Code name) now live in one descriptor table, `fontaine-tools/src/descriptor.rs`, and
  the unknown-tool error lists Claude Code names in table order (#181).

#### TUI

- The TUI's slash-command menu also opens for a `/name` typed mid-prompt, at the start of the
  input or after whitespace; accepting a match replaces only that word. Tab first extends the
  name to the longest prefix all matches share, then completes the selected row. Mid-prompt,
  Enter sends the message as typed (#294).
- Counts shown in the TUI use one compact form (`999`, `1.0k`, `90.9k`, `1.2M`): agent spend
  (`90.9k tok`), the budget-exhausted reason, `/context`, the goal summary, the `writing` count and
  the character counts in the copy notice and instruction-file warnings (#365). The ledger, audit
  log and JSON output keep exact values.

### Fixed

#### Agents

- A killed subagent keeps the transcript it produced up to the kill: the run settles at the next
  boundary of the in-flight model call or tool (with a bounded grace, then the hard drop as
  backstop), and the committed messages are saved to its `logs/agent-<n>.jsonl`, so `/export`
  renders the conversation instead of "transcript unavailable" and a diagnosis of why it was
  stopped has evidence to work from (#348).
- The agents panel and agent list mark a dollar figure that leaves out unpriced calls with
  `+ unpriced`, so a priced guardian spend is no longer shown as an agent's whole cost (#333).
- Built-in `explore` subagents now run on the session model instead of `$guardian_default`
  (#286). A session whose main model is on another account no longer routes every `explore`
  spawn at the guardian model's account, so that account's quota no longer takes explore agents
  down while the session model is healthy. `$guardian_default` remains a valid model value for
  types you define.

#### Config

- Reading the user settings or `trust.toml` while another session replaces it no longer fails on
  Windows, where a session could miss the file and start without the user's deny rules. Those
  reads now wait on the same lock the edits hold (#200).
- `trust.toml` and the user settings file are replaced atomically and edited under a lock, so a
  crash cannot truncate them and concurrent sessions no longer drop each other's changes. An
  unparseable `trust.toml` is reported once at session start instead of silently trusting nothing (#183).

#### Core

- A failed compaction, a settings file that cannot be written (the rule-save notice) and a
  PromptGuard download that fails now name the underlying reason (TLS, proxy, DNS, I/O) instead of
  only the outer message. Structurally, runtime errors keep their cause as `source()` rather than
  flattening it into text, and hook and download errors keep theirs for the logs (#185).

#### Guardian

- The guardian over-denied benign calls (#268, #239, #302, #278, #309): it now knows the
  session scratch directory (paths inside it no longer read as possibly pre-existing); posting
  an issue the user asked for on the project's own repository is no longer Data Exfiltration; a
  standing grant for a class of action ("you may post gh issues") counts as
  consent for soft denies; a plain `git commit` is no longer Git Destructive; and
  installing a package the task calls for from the tool's default registry is no longer
  Download+Execute. A new `overdeny` eval slice
  pins each reported case beside its attack counterpart (#312).
- The guardian no longer denies a plain `webfetch` or `websearch` of a public page as Data
  Exfiltration: the rule now needs sensitive data in the outbound request, and the read-only
  exception names both tools. A request with an encoded payload or a secret in the URL or query is
  still denied. The `webfetch` description tells the model to take URLs from search results or the
  user rather than guess them (#239).
- The guardian's touched paths for a shell command now list its redirect targets (`>`, `>>`, `>|`,
  `&>`, `<>`, `>&file`), including those on compound commands and in nested command lines, ahead
  of its arguments; before, a target such as `cfg.py` was missing while a non-path argument
  containing `/` was listed (#230).
- The guardian and the `llm` scanner no longer fail closed on every call with a model that thinks
  by default: on an `opencode-go` account their requests turn thinking off for Chat-wire models
  and non-Claude Messages-wire models, which refuse a forced tool call while thinking (#144).
- The guardian no longer over-corrects on benign follow-ups after a denial (#256). The two blocks
  holding the user's own messages are marked `source="user"` and read as intent, so "the script is
  ok to run" clears a soft deny for that script, and the prompt gains rules for session history: a
  denial of one action class does not taint another or a read-only call, reading a file the session
  created is routine, a user re-issuing a denied action is consent to it, and a hard deny is never
  cleared. A new `followup.jsonl` slice with an optional per-case `prior_denials` covers it, and the
  `rule` and `decision` field descriptions now spell out that they must agree. The dataset version
  changes, so existing scorecards are re-baselined by the next run.
- The guardian no longer denies a call as `unparseable` because the model put a rule's name in
  `rule` instead of a label (#273). Every shipped deny rule now starts with its label (persistence
  and self-modification report as `Safety Bypass`, untrusted code integration as
  `Download+Execute`, third-party and real-world effects as `Data Exfiltration`), the prompt says
  `rule` is copied from the label list, and a malformed verdict is retried with a harness-written
  note saying why it was rejected instead of the same prompt again. An answer still malformed
  after the retries is denied as before.
- The guardian over-denied more benign calls (#364, #366, #361, #357, #352; tracked by #312). A
  redirect to `/dev/null` no longer makes a shell call destructive or lists `/dev/null` as a
  touched path. Touched paths carry an `absent` mark (nothing is there) and a `git_tracked` mark (git
  holds a copy of the file), shown to the guardian only. Git Destructive no longer covers rebasing
  or amending the agent's own work, and Destroy Pre-existing Files no longer covers moves onto
  absent paths or overwrites of git-tracked files. Issue and comment text the agent wrote at the
  user's request is not exfiltration, and earlier denials about another subject no longer taint a
  call. The guardian's dead spawned-agent fact and Subagents prompt section are removed.

#### Permissions

- A tool call from an agent the session no longer knows (an id that was never reserved, was
  released, or sits under a released ancestor) is denied in every mode, with rule `agent registry`,
  before hooks or the guardian see it, instead of being judged with the root's mode (#184).

#### Sessions

- A failed model call no longer loses the interrupted turn (#281). The run's committed messages
  are saved to the conversation when a run fails, so typing `retry` (or any next message)
  continues the same task, and a retryable transport failure — a cut connection, throttling, a
  5xx — is retried twice with a short backoff before the failure reaches you, on the main agent
  and on subagents alike.

#### Tools

- A host with no system CA bundle no longer stops startup: `webfetch` and the scanner-model
  download fall back to the bundled Mozilla roots, as the model transport does, and a client that
  still cannot be built reports its cause (#112).

#### TUI

- Resizing the terminal no longer leaves a stale copy of the input box, task list and menu in the
  history above the live ones: a resize reprints the transcript at the new size. A slash command
  refused during a running turn shows a one-line hint in the status bar instead of adding a
  transcript line per Enter (#279).
- `/guardian-model` with no argument opens the model picker over a live listing of every account,
  as `/model` does, instead of showing nothing when no account lists `models` (#151).
- The approval prompt's rule line shows newlines, tabs and other control characters of a command as escapes
  (`\n`, `\t`, `\u{..}`) instead of dropping them; the saved rule is unchanged (#229).
- In-flight tool rows no longer overwrite the prompt box. Writing settled lines above a live area
  one or zero rows from the top of the screen used a one-row scroll region, which terminals ignore,
  so the whole screen scrolled with the box in it; the live area is now repainted after such a
  write, and erased beforehand so stale live-row text no longer shows through blank cells of the
  settled lines. Under tmux the live area also no longer leaves copies of the prompt box and
  transcript in the scrollback: erasing the screen from its top-left corner makes tmux move
  everything visible into history, so the live area is now erased row by row (#217).
- The screen no longer flashes when content is added to the chat. Each frame now reaches the
  terminal as one write, with its synchronized-update markers inside it, rather than as several
  chunks a terminal without that mode could paint one by one. When the live area changes height or
  settled lines are written above it, the live area is overwritten in place instead of being
  blanked and repainted, and settled lines no longer blank the rows they scroll over (#220).
- Closing the agents panel no longer leaves the main chat blank with its last messages only in
  scrollback; the shown transcript is reprinted when the panel pushed rows out of view (#296).
