# Changelog

All notable changes are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project uses
[semantic versioning](https://semver.org/) (0.x until the first stable release).

## [Unreleased]

### Added

- A built-in `fork` agent type starts a subagent from a copy of the main conversation, including
  the current turn, so its `prompt` can be a short task (#39). It runs on the session model, can be
  started only by the main conversation, and fails to start rather than run without the context. The
  guardian's intent source is unchanged.
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
- `notebook_edit` edits one cell of a Jupyter notebook (`replace`, `insert` or `delete`, by cell id
  or `cell-N`); a code cell's outputs are cleared when its source is replaced. `read` shows a
  `.ipynb` as its cells, ids and text outputs, `NotebookEdit` rules and `allowed-tools` entries now
  map to it, and it joins the `worker` subagent type. It is a workspace write: `acceptEdits` runs
  it, `plan` blocks it, and the protected-path floor applies (#20).
- Checkpoints and `/rewind` (alias `/checkpoint`, or Esc Esc at an idle empty prompt). `write` and
  `edit` save a file's contents before changing it, filed under the prompt being answered, and
  `/rewind` restores the code, the conversation or both to the start of an earlier prompt of the
  conversation. Shell, `!` and MCP changes are not tracked; files over `[checkpoints]`
  `max_file_bytes` or past `max_session_bytes` are skipped with a warning; files edited since
  Fontaine's last change ask before being overwritten; the memory directory is never touched. The
  ledger is never truncated: a `rewound` record trims only the guardian's view of what you asked.
  Summarize-from-here is not offered (#12).
- Auto memory (`[memory] auto`, user scope only, on by default while it is tested). The agent saves
  notes about the project with `write` and `edit` to `<data dir>/memory/<project key>/`, outside the
  repository, and the capped `MEMORY.md` index loads into later sessions framed as unverified notes.
  Every write is gated by the protected-path floor and, in `auto`, the guardian, with a
  memory-write rule in both guardian prompts; there is no fast path. `/memory` lists the saved files,
  `/context` counts the layer, and `evals/dataset/memory.jsonl` adds the memory-poisoning cases (#8).
- `fontaine auto-mode defaults|config|critique` inspects the rules the guardian judges against.
  `defaults` prints the shipped lists as a TOML fragment, `config` prints the effective lists with
  the scopes that set each and whether each entry is shipped or custom, and `critique` asks the
  guardian's model to review your custom entries (advisory text; it is the only one that calls a
  model). Each takes `--format text|json` (#1).
- `--cd <DIR>` (`-C`) starts Fontaine in another directory: it becomes the project root for
  settings, trust, workspace and session identity, and relative `--settings`, `--add-dir` and
  `--debug-file` values resolve against it (#145).
- Each session has a private scratch directory, added to the workspace roots automatically
  (`$XDG_RUNTIME_DIR/fontaine/<session id>/`, else an owner-only per-user directory under the temp
  dir). `write` and `edit` work there without a prompt in `acceptEdits`, the system prompt names it,
  and the out-of-roots denial points at it. It is removed on exit and swept after a crash; the
  floor, the guardian and `shell` handling are unchanged (#238).
- Tool calls in the transcript are cards: a long or multi-line command folded to three rows, the
  last ten rows of the output behind a gutter, and for `edit` and `write` the diff of the change,
  also when no prompt asked about it. `/expand [n]` prints a call in full as new lines. Output is
  sanitised and redacted before it is kept, and at most 64 KiB of a result is retained (#222).
- The approval prompt for `edit` and `write` shows the change as a diff (line numbers, `+`/`-`
  lines, three lines of context, scrollable) before you decide; in the linear renderer it is plain
  text. It is display-only and absent when the file cannot be read or the edit would fail (#45).
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
- `/goal <condition>` keeps the session working until a separate model judges the condition met, in
  the terminal UI and as `fontaine -p "/goal ..."`. After each turn the evaluator (the guardian's
  model) answers met, not met or impossible; not met starts another turn as a harness note, never a
  user message. Unattended stretches are bounded by `[goal] max_turns`, an optional
  `[goal] max_cost_usd` and a no-progress cap, a failed turn or evaluator pauses the goal, and the
  goal is restored on `--continue`/`--resume`. Headless adds a `goal` event, `result.goal` and
  `usage.goal`, and exits 1 when the goal ends impossible or paused (#17).
- Agent and models
  - The agent loop runs over rig. Models come from `[accounts.<name>]` entries mapped to provider
    clients (Anthropic, OpenAI, Gemini, Ollama, OpenAI-compatible endpoints) and are named
    `<account>/<model>`, with model discovery. Credentials come from an environment variable or
    the stored-credentials file, which the terminal UI's `/login` writes.
  - Sessions live in a per-session directory under the data directory, with the conversation
    persisted by rig's memory store, between-runs compaction by a summarizing call and an
    append-only ledger. `--session-id`, `--continue`, `--resume <id>` and `--fork-session` select
    or branch a session.
  - A versioned system prompt steers the agent toward the dedicated read tools, denial handling
    and the workspace roots, layered with `FONTAINE.md` from the user config directory and the
    primary root.
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
- Configuration
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
- Prompt caching (#13)
  - Anthropic accounts, and `opencode-go` models on the Messages wire when opted in, send cache
    breakpoints on the tools, the system prompt and the latest message. `prompt_caching` (on for
    `anthropic`, off for `opencode-go`) and `cache_ttl` (`"5m"` or `"1h"`) are user-scope account
    keys. The guardian's request marks only its static prefix.
  - Anything before the latest message is append-only within a session, an MCP server's tools are
    registered sorted by name, and the headless `usage.main` reports cache read and write tokens.
- Task list (#22)
  - The `todo_write` tool keeps a session task list the model replaces whole on each call. It is
    read-only and takes the fast path, and is kept in `todos.json` in the session directory so
    `--continue`, `--resume` and `--fork-session` bring it back. The terminal UI shows it above the
    input, collapsed to one line or expanded (Ctrl-O or `/todos`); the screen-reader mode prints
    it as plain lines when it changes.
- Session-scoped scheduling (#16)
  - `/loop <interval> <prompt>` re-runs a prompt on a wall-clock cadence and `/loop <prompt>` lets
    the model pace itself with `schedule_wakeup`; `/loop list` and `/loop stop [id]` manage them.
    The tools `cron_create`, `cron_jobs`, `cron_delete` and `schedule_wakeup` are main-conversation
    only; `cron_create` is an arbitrary tool the guardian judges. Due prompts run between turns as
    harness notes, never as user messages, so they do not widen the guardian's view of what you
    asked. Tasks persist in `schedule.json`, come back on resume without replaying missed runs, are
    capped at 50 and expire after 7 days, and `/clear` cancels them. `[schedule] enabled` (user
    scope) turns the feature off.
- Session UX (#10)
  - Sessions can be named with `--name`/`-n` or `/rename [name]`; `--resume` takes an id or a name.
  - `/resume [name-or-id]` opens a picker of this folder's sessions, `/branch [name]` (`/fork`)
    copies the live session and carries on in the copy, and `/export [path]` writes the
    conversation as an owner-only Markdown file. Switching sessions drops grants and returns to
    the default mode, like any resume.
- Cascade screen stage (#130)
  - An optional `[guardian.screen]` stage (user file only) asks a small local Ollama model one
    yes/no question per call in `auto` and `plan` mode and reads `p(yes)` from its first-token
    logprobs. A call it confidently judges outside the user's request is escalated with
    `tier = screen`; every other call, including one the screen could not judge, goes to the
    guardian as before. (Superseded by the allow path below: the screen can now allow, but only
    when `allow_threshold` is set.) `stop_threshold` has no default, and `shadow = true`
    (the default) scores and records every call without changing a decision. The audit record,
    the `fontaine.guardian.screen` span and the `fontaine.screen.count` metric carry the result.
  - `fontaine-evals --screen live:<account>/<model> --stop-threshold <risk>` scores the cascade
    (catch, guardian calls, latency, benign cases escalated) from one run.
- Screen allow path (#135)
  - The screen can now allow a call without the guardian, superseding "the screen never allows".
    It is off unless `[guardian.screen] allow_threshold` is set (user file only), and only when a
    steering-hardened permission read is below that threshold and a steering detector is below
    `steering_veto` (default `0.001`). The audit record, the headless `gate_decision` event and the
    `fontaine.guardian.screen` span gain `allow_risk` and `steering`; an allow is `tier = screen`,
    `decision = allow`, and `fontaine.screen.count` gains an `allow` outcome. Shadow mode logs the
    allow scores to calibrate against.
  - The screen and the guardian now start together (`fontaine_guardian::cascade`), so a call the
    screen does not settle costs the slower of the two (the screen is bounded by `timeout_ms`), in
    practice the guardian's. A settled call drops the guardian request: its
    tokens are not counted in usage. A guardian verdict already in hand always wins.
  - `fontaine-evals --allow-threshold <risk> [--steering-veto <p>]` scores the allow path. The
    cascade summary replaces `mean_latency_ms` with `allowed_latency_ms` and adds
    `should_allow_stopped`, `screen_allowed` and `must_not_clear_screen_allowed`.
- Tools
  - `ask_user_question` tool: the model can ask you 1 to 4 multiple-choice questions, each with an
    "Other" row for your own words. The terminal UI shows a dialog; a headless run returns "user
    input is unavailable, proceed with your best judgment". Only the main conversation can ask, no
    permission mode answers for you, and the text you type under "Other" counts as guardian intent
    while the options you pick do not (#23).
  - The built-in tools `read`, `ls`, `glob`, `grep`, `write`, `edit`, `shell` and `webfetch`. Every
    tool has a risk class and sits behind one gate, and each call emits a `fontaine.tool` span and
    a `tool_result` telemetry event.
  - Workspace roots from the primary directory, the repeatable `--add-dir` flag, `/add-dir` and
    `[permissions] additional_directories`, with one confined tool instance per root.
  - `shell` child processes do not inherit telemetry credentials, configured accounts' API key
    variables or the scanner token variable. On Windows the shell runs through Git Bash, found on
    `PATH`, in the standard install locations or through `FONTAINE_GIT_BASH_PATH`, and commands
    reach it unchanged, backslashes included (#78).
  - `webfetch` sends the User-Agent `fontaine/<version> (+https://github.com/madesroches/fontaine)`
    (#73) and `Accept: text/markdown, text/html, */*` (#85).
- Permissions
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
- Guardian
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
- Instruction files (#7)
  - Fontaine reads `FONTAINE.md`, `CLAUDE.md` and `AGENTS.md` from your config, from every directory
    from the filesystem root down to the primary root (with `CLAUDE.local.md` and
    `FONTAINE.local.md`), from `.claude/` and `.fontaine/` (including `rules/`), and from `@path`
    imports up to four levels deep. `AGENTS.md` is read when no `CLAUDE` or `FONTAINE` file
    exists. Identical files load once. Rules can be scoped to paths with a `paths` frontmatter key,
    and nested files and matching rules load into the tool result when the agent reads or writes a
    file under them.
  - Repository files that resolve outside the workspace and its git work tree, and imports that
    leave it, are skipped with a warning; a `read` deny rule keeps a file out of the prompt.
    `.claude` joins the protected directories. The guardian's `project_instructions` is a
    snapshot of the launch set from session start, most specific file first.
  - `/memory` lists the instruction files and opens one in `$VISUAL` or `$EDITOR`; `/init` has the
    agent write or improve the project instruction file.
- Terminal UI and headless
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
  - Context commands: `/compact [focus]` summarizes the conversation on request, `/clear`
    (`/reset`, `/new`) empties the model's context while the session, its permissions and its
    mode carry on, and `/context [all]` shows estimated token use by source. `[context]
    autocompact_window` sets how many tokens of messages trigger automatic compaction, in the
    terminal UI and headless runs (#11).
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
  - `fontaine -p` with `--output-format text|json|stream-json`, `--input-format stream-json`,
    `--include-partial-messages`, `--max-turns`, `--max-budget-usd`, `--json-schema`,
    `--system-prompt`, `--append-system-prompt`, `--model` and `--permission-mode`. The output is
    a versioned contract with a JSON Schema in `docs/schemas/headless-v1.schema.json`; exit codes
    are 0, 1 and 2. `total_cost_usd` and `--max-budget-usd` include guardian and subagent spend, and
    `result.usage` breaks tokens and cost down into `main`, `guardian` and `subagents`. Notices go
    to stderr.
- Extensibility
  - Parallel subagents: the `agent` tool (blocking, or `background: true` with the report
    delivered as a notification) and `send_message`, agent types from `[agents.types]` and agent
    files, a mode that is never looser than the parent's, a nesting limit of three layers,
    `[agents] max_parallel`, per-subagent and session token and cost budgets, and a
    background-agents panel (`/agents`, Ctrl-T) with kill and per-agent decisions. Headless runs
    wait for background subagents.
  - MCP servers over stdio and streamable HTTP from `[mcp.servers.*]`, registered as wrapped
    tools named `mcp__<server>__<tool>`. Server annotations reach the guardian only as untrusted
    text, and a repository's servers need workspace trust and the user's approval of each.
    A tool a server marks with `_meta["anthropic/requiresUserInteraction"]: true` asks on every
    call, whatever the mode or allow rules, and is denied in `dontAsk` and headless runs (#2).
  - User hooks: `pre_tool_use`, `post_tool_use` and `permission_denied` command hooks with Claude
    Code's JSON input, output and exit codes; a repository's hook can tighten but never allow.
  - Skills, agent files and output styles with Claude Code's frontmatter, the `skill` tool, skills
    as `/name` commands with arguments, `` !`cmd` `` context run as gated `shell` calls, and
    `allowed-tools` lent for a turn.
- Telemetry
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
- Evals
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
  - Laya scored on the golden set, and the spike's experiments recorded with their scripts and a
    400-case synthetic training corpus. Experiments: question framings, ensembles, embedding probes,
    public corpora, head fine-tuning, question framings that ask whether the action fits the task,
    small chat models read through logprobs, and a held-out screen-then-guardian cascade. With
    `qwen3-4b-instruct-2507` as a stop-only screen, the cascade caught 96.7% of attacks (91.7%
    for the guardian alone) with 41% of the guardian calls. Follow-ups #123–#130 and #132 (#98).
- Research
  - A docs-based study of Claude on Microsoft Foundry and Databricks Model Serving, with a
    central list of model-specific support and follow-ups #165–#170 (#157).
  - A study of the oh-my-pi harness, with experiments in Docker behind a recording proxy and a tmux
    comparison of its terminal UI with Fontaine's: what lets the model iterate, what its safety posture
    gets wrong, and the follow-ups (#213).
- Distribution
  - Release binaries for Linux, macOS and Windows built with `pg2` by cargo-dist on a `vX.Y.Z`
    tag, each archive carrying the license, readme and changelog; `cargo install --git
    https://github.com/madesroches/fontaine fontaine` from source.
  - Documentation under `docs/`, a README quickstart, contributing guide and security policy.
- Corporate network support (#6)
  - A `[network]` section (user file and `--settings` only), repeatable per account, sets an HTTP
    or HTTPS proxy with a `no_proxy` list, extra CA roots (`ca_bundle`) and a client certificate
    (`client_cert`, `client_key`) for model accounts, the guardian and its screen, `webfetch`, the
    scanner-model download, the `opencode-go` sign-in and HTTP MCP servers. An invalid setting is a
    configuration error (exit code 2), never a fallback to a direct or default-trust connection.

### Changed

- Each tool's decision inputs (risk class, path containment, destructive policy, rule specifier
  and Claude Code name) now live in one descriptor table, `fontaine-tools/src/descriptor.rs`, and
  the unknown-tool error lists Claude Code names in table order (#181).
- Which scopes may set a settings key is now one declarative table in `fontaine-core`, and a key
  with no entry is refused from project and project-local files instead of accepted. No existing
  scope decision changes; a scope error now names the table entry's key, which can be a section
  such as `guardian.screen.model` or `telemetry.endpoint`.
- A poisoned lock now ends the session wherever its contents feed a permission or guardian decision,
  a spend tally, the model in use or a durable record, instead of being silently recovered; only
  display and advisory state (the `@`-file index, steering buffers, telemetry tallies, a child's
  partial text) still recovers. One helper module, `fontaine_tools::lock`, holds the policy (#186).
- CI runs faster: the pg2 clippy and test legs and the fixture eval runs are Linux-only (macOS and
  Windows still run `cargo check --features fontaine/pg2` and the `--guardian none` eval), the
  Rust cache is dropped (measured slower than a cold build on every OS), tests run under
  `cargo nextest`, pull-request runs cancel when superseded, debug info in the dev and test
  profiles is reduced to line tables, and a PR touching only `tasks/` or `AGENTS.md`
  skips the Rust steps (#178).
- `AgentRuntime` no longer keeps a second copy of the permission mode and workspace roots:
  `mode()` and `roots()` read the live session state, `start_mode()` is the mode the session
  opened in, and `agent.rs` is split into `setup`, `build`, `models` and `mcp` submodules with a
  clippy line-count limit and one builder configuration shared by the root agent and subagents
  (#187).

### Fixed

- `/guardian-model` with no argument opens the model picker over a live listing of every account,
  as `/model` does, instead of showing nothing when no account lists `models` (#151).
- The approval prompt's rule line shows newlines, tabs and other control characters of a command as escapes
  (`\n`, `\t`, `\u{..}`) instead of dropping them; the saved rule is unchanged (#229).
- The guardian no longer denies a plain `webfetch` or `websearch` of a public page as Data
  Exfiltration: the rule now needs sensitive data in the outbound request, and the read-only
  exception names both tools. A request with an encoded payload or a secret in the URL or query is
  still denied. The `webfetch` description tells the model to take URLs from search results or the
  user rather than guess them (#239).
- The guardian's touched paths for a shell command now list its redirect targets (`>`, `>>`, `>|`,
  `&>`, `<>`, `>&file`), including those on compound commands and in nested command lines, ahead
  of its arguments; before, a target such as `cfg.py` was missing while a non-path argument
  containing `/` was listed (#230).
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
- Reading the user settings or `trust.toml` while another session replaces it no longer fails on
  Windows, where a session could miss the file and start without the user's deny rules. Those
  reads now wait on the same lock the edits hold (#200).
- A tool call from an agent the session no longer knows (an id that was never reserved, was
  released, or sits under a released ancestor) is denied in every mode, with rule `agent registry`,
  before hooks or the guardian see it, instead of being judged with the root's mode (#184).
- `trust.toml` and the user settings file are replaced atomically and edited under a lock, so a
  crash cannot truncate them and concurrent sessions no longer drop each other's changes. An
  unparseable `trust.toml` is reported once at session start instead of silently trusting nothing (#183).
- A host with no system CA bundle no longer stops startup: `webfetch` and the scanner-model
  download fall back to the bundled Mozilla roots, as the model transport does, and a client that
  still cannot be built reports its cause (#112).
- The guardian and the `llm` scanner no longer fail closed on every call with a model that thinks
  by default: on an `opencode-go` account their requests turn thinking off for Chat-wire models
  and non-Claude Messages-wire models, which refuse a forced tool call while thinking (#144).
- A failed compaction, a settings file that cannot be written (the rule-save notice) and a
  PromptGuard download that fails now name the underlying reason (TLS, proxy, DNS, I/O) instead of
  only the outer message. Structurally, runtime errors keep their cause as `source()` rather than
  flattening it into text, and hook and download errors keep theirs for the logs (#185).
