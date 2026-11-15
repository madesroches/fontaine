# Permissions Design

Part of the Fontaine design; the master plan `tasks/fontaine_harness_plan.md` holds the Phase Tracker, workspace layout and `## Decisions`.

## Core data flow

```
 user ──▶ TUI ──▶ rig-agent runtime, orchestrated by fontaine-sdk's `core` module
                      │  (AgentBuilder + AgentRun state machine; rig streams model events)
                      ▼
               ToolCall { name, args }
                      │
                      ▼  (every registered tool is a GuardedTool wrapper)
         PermissionPipeline (`fontaine-sdk::core`; GuardedTool::call reaches it through the `ToolGate` trait)
           1. registry.lookup(name) → RiskClass
           1a. User PreToolUse hooks (§User hooks): may return deny / ask / allow and an
              `updated_input` that replaces the call — every later step, the audit record and the
              guardian judge the rewritten call; their decision is held and applied below
              (deny → Deny here; ask → joins the step-2 forced prompt; allow → acts at step 5a only)
              After the hooks, an out-of-root read (step 5b's read tools) is resolved once to its
              absolute target — symlinks followed — and that normalized form is what steps 2 and
              5a match (deny/ask rules keep seeing the written form as well), what the guardian,
              the approval prompt and every allow return, and what execution consumes; retargeting
              the named symlink mid-decision cannot redirect the call. A hook `allow`'s matcher is
              revalidated against the normalized arguments before it acts: an allow that no longer
              covers the resolved target falls through to the mode. Malformed read arguments (a
              missing or non-string path, an untranslatable or non-absolute resolution) are an
              argument error right after the floor: never a prompt, never the guardian.
           2. Deterministic floor — evaluated BEFORE the mode branches, in EVERY mode (no mode,
              flag, or verdict reaches past it; it can only prompt or deny, never allow; order:
              deny → ask → protected-path):
              `[permissions] deny` rule match → Deny; `[permissions] ask` rule match, or an MCP tool its server marked
              `_meta["anthropic/requiresUserInteraction"]: true` → force the
              human prompt (even in `auto`; a match in `dontAsk` denies); rules use the
              `Tool(specifier)` grammar (§Permission rules); protected-path check (a separate list, enumerated under Protected paths below; applies to
              the edit-class tools `edit`/`write`) → writes prompt in manual/acceptEdits, go to the
              guardian in `auto`, Deny in `dontAsk`
              (Claude Code's protected-paths behavior). In headless `-p` every prompt outcome is a Deny. All
              structured matching (tool name + risk metadata + path containment), never regex on raw
              args. There is no critical-path step: a `shell`/`monitor`/`powershell` call that deletes the
              filesystem root, a top-level dir, home, or a workspace root or one of its parents is an
              ordinary call to the mode (a deliberate deviation from Claude Code, which prompts), and the
              guardian is told about it as a harness fact, `critical_deletions` (see Critical deletions below).
           4. RiskClass::ReadOnly && args within workspace scope? → Allow (no guardian);
              likewise an `agent` call whose Fontaine-computed child toolset holds only
              ReadOnly + Destructive::Never tools (plus the coordination tools `send_message` and `board_update`); an MCP tool, a missing
              descriptor row or a parent toolset that was never set never qualifies (a known, empty set can change nothing and does), and the child's own calls are
              gated one by one
              (scope check enforced by Fontaine's wrapper/scope validator — `rig_tools::fs::place`
               against each canonicalized workspace root; inside any root counts — first-party only;
               MCP-declared readOnlyHint does not qualify — such tools register as Arbitrary;
               the `skill` tool qualifies: its only argument is a skill name resolved against the
               start-time index, and it loads skill text into context)
           5. Tier 0.5 — (after the parse below, every path-valued word, including `--flag=value` values, is also checked as a `read` call — and as `ls`/`grep` for those commands — against the deny and ask rules: a deny match is a Deny, an ask match a prompt; each path word is checked, but files a recursive or path-less search reaches under a directory are not individually checked; read rules are not a containment boundary for shell) deterministic shell pre-classifier (zero-LLM), `shell` calls only:
              parsed by `brush-parser` (a real POSIX shell parser, pinned to an exact version — NOT
              regex, no hand-rolled lexer) into exactly ONE simple command (no control operators
              `; | & && ||`, redirections, substitutions, subshells, backgrounding, globs —
              anything else falls through) AND argv[0]+subcommand ∈ curated read-only set
              (`ls`, `pwd`, `wc`, `file`, `rg`/`grep`, `cat`/`head`/`tail`, plus the curated read-only
              `git` subcommands `status`, `diff`, `log`, `show`, `ls-files`, `branch`, `rev-parse`, `tag`;
              any other git invocation goes to the guardian; the curated git subcommands run in-process and never spawn `git`, so repo-config commands such as `core.fsmonitor` never execute) AND every argument word is literal text only (plain or quoted; no parameter expansion `$…`/`${…}`, no tilde, no brace pattern, no unquoted glob chars — brush-parser brace expansion is detected via `parse_brace_expansions`) AND every remaining token is either a path argument passing
              the step-4 scope validator (symlink-resolved; absolute / out-of-workspace paths
              never qualify) or a flag on a per-command read-only flag allowlist — any flag not
              explicitly allowlisted for that command, any glob/`~`/wildcard token, or any
              unparsed construct falls through to the guardian; values of file-opening flags
              (`--files-from`, `-f`, `-m`, etc.) are themselves treated as path arguments and
              validated (mirrors Claude Code's documented per-command flag handling; the initial
              allowlist table is in the Phase 3 plan)
              the call's effective `cwd` (argument, else the tracked directory) is resolved first: relative path arguments resolve against it and an
              out-of-scope `cwd` disqualifies the fast path
              → Allow, with an audit-log entry (no guardian)
           5a. Allow rules — only when the floor forced nothing (no Deny, prompt, or
              protected-path routing):
              `[permissions] allow` rule match (session grants in `SessionState` included) or a
              PreToolUse hook `allow` → Allow (no guardian), audit `tier = allow_rule` / `hook`;
              in `auto`, broad code-execution rules and hooks are suspended (§Permission rules)
           5b. a call whose path `Scope::locate` cannot place, for a tool with no external
              route — today only `lsp` among the file tools (`shell`/`powershell` are
              `Arbitrary` and never reach this branch) → Deny with the per-session scratch
              directory and the `/add-dir` hint; never reaches the prompt. An out-of-root READ
              (`read`/`ls`/`glob`/`grep`) and an out-of-root WRITE (`write`/`edit`/
              `notebook_edit`) do not stop here: they fall through to the mode (step 6 asks in
              manual/acceptEdits, step 7 asks the guardian in auto, step 8 denies in dontAsk),
              pre-approved calls run at the step-5a tiers, and an allowed call executes on the
              tool's unconfined fallback instance (below; read-only for the read tools, a write
              fallback for the write tools). An external path never becomes a root: no write
              reach of its own, no fast path, no instruction discovery.
           6. mode == manual/acceptEdits → human prompt (acceptEdits: auto-allow edits in workdir)
           7. mode == auto → the composed guardian (`fontaine_sdk::guardian::composition::adjudicate`):
              the Rune script of `[guardian.composition]` (the `screen` preset by default) over
              the host leaves, the LLM guardian and the logprob reads ──▶ Verdict, inside a
              fail-closed root (instruction budget, time bound, no verdict = Deny). A script
              `escalate` (prompt; headless Deny) and `deny` (no prompt) are `tier = screen` with
              `rule` = the node, and `allow_below` (a user-scope script's only way to allow) is
              `tier = screen` too; a script that fails is a guardian denial like any other.
              A completed LLM refusal in hand always wins, and shadow mode and a script that
              ends in the LLM leave the call to it. A paused breaker prompts before any of this,
              and a composition allow does not reset the breaker.
           8. mode == dontAsk → anything not pre-allowed is Denied (pre-allowed = steps 4, 5, 5a)
                      │
         Allow ──▶ inner rig Tool executes ──▶ ResultPipeline ──▶ result appended to conversation (rig-memory)
         Deny  ──▶ tool_result = "Denied by guardian: [explanation]"; agent sees it and adapts
         Escalate ──▶ human prompt (with guardian's explanation shown)

 (the `agent` tool call that spawns a subagent is an ordinary `Arbitrary`-class pipeline request —
  gated at steps 4–8 like any other, except that a spawn whose child can only read takes step 4 and any other spawn skips the guardian (`auto`/`afk` allow at tier `spawn`, `manual`/`acceptEdits` ask, `dontAsk` denies); `send_message` and `board_update` are likewise never guardian-judged: after the floor, hooks and rules every mode allows them at tier `coordination`, `dontAsk` included;
  every tool call the subagent then makes re-enters this same
  pipeline)

  `ResultPipeline` is the post-call step inside `GuardedTool` (Phase 2) through which every tool result
 passes before the agent sees it, in fixed order: the tool-result scanner (Phase 6), then `post_tool_use`
 user hooks (Phase 5).

 Tool calls within one model turn are adjudicated and executed one at a time, in order (rig-agent
 `tool_concurrency = 1`); each call gets its own tool_result and a Deny does not affect the turn's
 later calls.
```

**Workspace roots and the external-read fallback.** Each file tool and `shell` keeps one confined
instance per root, built by `make(root)`, and a call is served by the instance of the root that
holds its path. Beside them, each of the four read-only file tools registers one unconfined,
read-only fallback instance based at the primary root through a dedicated routing mode
(`per_root_with_fallback`), and each write tool (`write`, `edit`, `notebook_edit`) registers one
unconfined write fallback through `per_root_with_write_fallback`, sharing the registry's `Seen`
so read-before-edit tracking holds. The fallback is reached only when strict `Scope::locate`
answers `OutOfScope` and only after the gate has returned `Allow` — it validates the normalized
arguments' absolute external shape (`external_shape` / `external_write_shape`, no file system
access) and consumes them as they are, never re-resolving the original symlink. `shell` registers
no fallback and keeps strict per-root routing. The fallback is an execution capability only: the
gate remains the single choke point, the registrations keep their descriptor risk classes
(`ReadOnly` for the read fallbacks, `WorkspaceWrite` for the write fallbacks), and an external
path is added to no root list.

**Protected paths** (step 2, last floor check): applies to the edit-class tools `edit` and `write`
— the only registered tools that write structurally (`delete`/`move` are not registered; shell writes
are not structurally detectable without regex on args, so they take the ordinary shell path: guardian
in `auto`, prompt in manual/acceptEdits). Matched on resolved path components, in any workspace root or
the home directory, plus any path under Fontaine's own config and data directories. Directories: `.git`, `.vscode`, `.idea`, `.husky`, `.fontaine`, `.claude` (Fontaine reads `.claude/CLAUDE.md` and `.claude/rules/` into system prompts, so a model write there is a persistence vector). Files: `.gitconfig`,
`.gitmodules`, `.bashrc`, `.bash_profile`, `.zshrc`, `.zprofile`, `.profile`, `.ripgreprc`,
`.mcp.json`. This follows Claude Code's documented protected-paths list; confirm it against a `claude -p` baseline in Phase 3 and record any difference in Decisions.
Per mode: manual/acceptEdits → prompt (acceptEdits' edit auto-allow does not apply); auto → guardian;
dontAsk → Deny; headless → Deny.

**Audit log** (written by the pipeline in every mode; one record per gate decision): JSONL, one
append-only file per session at `<data dir>/audit/<session-id>.jsonl`, owner-only.
Fields: `ts` (RFC 3339), `session_id`, `agent_id`, `tool_use_id` (the model's tool-call id), `tool`, `args_digest` (SHA-256 over the
RFC 8785 JSON Canonicalization Scheme serialization of the args, produced by `serde_jcs` — raw args are never logged; when a hook rewrote the call, `original_args_digest`
too), `tier` (the tier enum below), `decision` (`allow` | `deny` | `prompt`), `rule` (deny/ask/allow rule text and
its source scope, hook id, floor check, or guardian rule label; null if none), `confidence` and `guardian_model`
(`<account>/<model>`) for guardian decisions, `latency_ms`. A human answer to a prompt appends a
second record with `tier = human`.

**Tier enum** (defined once, on `DecisionRecord.tier`; the audit log, the `tool_decision` telemetry
event, the headless `gate_decision` event and the eval `expected_tier` all carry exactly these values,
with no projection or renaming): `deny_rule` | `ask_rule` | `protected_path` (the
floor, in check order) | `hook` | `read_only_fast_path` (step 4) | `tracked_edit` and `scratch_write` (`auto` only, after the floor and hooks, before the guardian) | `tier05` (step 5) | `spawn` (an `agent` spawn after the floor, hooks and rules, never guardian-judged: `auto`/`afk` allow it) | `coordination` (`send_message` and `board_update` after the floor, hooks and rules, every mode) | `allow_rule`
(step 5a) | `mode` (step 6: manual/acceptEdits prompt, acceptEdits edit auto-allow) | `guardian` (step 7) |
`screen` (step 7: a composition node's escalate, deny or allow) | `dont_ask` (step 8) | `human`. A prompt (or headless Deny) caused by the loop-breaker pause carries `guardian`. A protected-path call routed to the guardian in `auto` records `tier = guardian`; in manual/acceptEdits/dontAsk/headless it records `protected_path`.

**Critical deletions.** `critical::critical_deletions(tool, args, env)` (`fontaine-sdk::core`) returns, for `shell`, `monitor` and `powershell`, each `rm`/`rmdir`/`Remove-Item` operand whose literal target (resolved against the call's `cwd`, lexically and through `canonicalize`; `~`, `~/<literal>`, `$HOME` and `${HOME}` against the session home, `$HOME` only when the command assigns nothing) is the filesystem root, a top-level dir, home, a workspace root or a parent of one, as `<resolved path> (<kind>)`. Whatever it cannot resolve (an expanded command name or target, a launcher, a wildcard, a chain, an unreadable command) adds nothing and reaches the guardian unannotated. `adjudication::build` copies the result into `ToolCallSummary.critical_deletions`; the prompt builder renders it as a harness fact; the full profile's hard deny (`Destroy Pre-existing Files`) and the minimal profile's `class=host-data-delete` deny it; and the composition's `allow_below` abstains for a call that has one, so a score never allows it.

**Pipeline inputs**: mode, the workspace roots, the merged permission rules plus session-scope
grants, the user-hook runner, the session ledger (source of task intent, recent user messages,
session facts and prior denials — `sessions.md` §Session ledger), the audit-log sink (schema above), and the
human-approval callback live in a shared `SessionState` owned by the `PermissionPipeline`, which is
injected into every wrapper at registration as the `ToolGate` (see the Phase 2 plan).

**Headless (`-p`)**: nothing prompts. Every would-prompt outcome — `ask`-rule match, Escalate
verdict, protected-path prompt, loop breaker — becomes a
Deny returned to the agent as the tool_result, and the run continues (Claude Code parity). In headless `auto`, an unavailable pinned guardian makes every adjudication `Deny GuardianUnavailable`, with a notice in the `init` event and on stderr. Exit
codes: 0 run completed (denials do not change it), 1 run error (provider/model failure,
interruption), 2 config or usage error. Output and input follow the versioned headless I/O contract
(`--output-format text|json|stream-json`; see the Phase 1 plan), whose `gate_decision` events carry
the same tier attribution as the audit log.

## Permission rules (deny / ask / allow)

One grammar for `[permissions] deny`, `ask` and `allow` (and the CLI `--disallowed-tools` /
`--allowed-tools`): `Tool` or `Tool(specifier)`. A bare tool name matches every call of that tool.
Matching is structured — never regex on raw args. A tool that is neither built-in nor `mcp__…` is a config error (exit 2).

| Tool | Specifier | Match |
|---|---|---|
| `shell` | words, optional trailing ` *` (`shell(git push)`, `shell(npm run test *)`) | argv from the Tier-0.5 `brush-parser` pass (quoted words stay one argument): exact argv, or argv prefix when the rule ends in ` *`; no wildcard inside a word |
| `read`, `edit`, `write`, `ls`, `glob`, `grep` | gitignore-style path pattern (`src/**`, `~/.aws/**`, `//etc/hosts`) | relative patterns anchor at each workspace root; the path argument is resolved by the scope validator (symlinks, `..`, the call's effective `cwd`: argument, else the tracked directory) and the resolved path is matched; for `ls`/`glob`/`grep` that is the `path` argument (default: the primary root), and for `glob` also the pattern's anchored base |
| `webfetch` | `domain:<host>` or `domain:*.<host>` | parsed URL host (lowercased, IDNA-normalized); `*.` matches subdomains only |
| MCP | `mcp__<server>`, `mcp__<server>__<tool>`, `mcp__<server>__*` | registered tool name |
| `agent`, `skill` | agent type / skill name | exact name |
| `send_message` | none (bare `send_message` only) | tool name |

`webfetch` never follows a cross-host redirect — the redirect URL is returned to the model and a
follow-up fetch is gated on its own host — so an allowed domain cannot bounce a request to an
unlisted one.

**Asymmetric failure.** Deny and ask fail toward matching; allow fails toward not matching. A
`shell` deny/ask rule is always a prefix match (trailing ` *` implied) and matches when its words appear
contiguously anywhere in the parsed word stream (control operators split off first, so `ls && git push`
and `sudo git push -f` hit `deny shell(git push)`),
and a command that fails to parse, or puts a substitution or variable in command position, matches every
deny/ask `shell` rule whose first word appears in it. A `shell` allow rule
matches only a command the Tier-0.5 parser accepts as one simple command (no chaining, pipes,
redirections, substitution, globs), optionally after one leading `cd <literal dir> &&` whose dir is inside the roots, whose argument words are all literal and whose argv matches; a non-literal word never matches; anything else falls through. Path deny/ask
rules match the given path and the resolved path; path allow rules match the resolved path only.

**Where allow acts.** Allow rules are pipeline step 5a: after the floor, never past it — a deny, ask
or protected-path outcome stands whatever allow says. The floor reads no shell deletion target, so an allow rule that matches `rm -rf /` approves it (the user's own grant; recorded in the master plan's Decisions). A
matching allow rule skips the human prompt in manual/acceptEdits, pre-approves in `dontAsk`, and
skips the guardian in `auto` (Claude Code parity). In `auto`, **broad code-execution allow rules are
suspended** for as long as the mode is on (Claude Code parity: its docs drop blanket `Bash(*)`,
wildcarded interpreters, package-manager run commands and `Agent` allow rules, and keep narrow rules
such as `Bash(npm test)`). The docs give categories, not an exhaustive list, so the list Fontaine
ships is: bare `shell`; bare `agent`; and `shell(<entry>)` or `shell(<entry> *)` for each `<entry>` in
`sh`, `bash`, `zsh`, `python`, `python3`, `node`, `ruby`, `perl`, `deno`, `bun`, `cargo run`,
`npm run`, `npx`, `pnpm`, `yarn`, `make`, `eval`, `exec`, `xargs`, `env`, `sudo`. A rule with more
words (`shell(npm test)`, `shell(python script.py)`) is narrow and stays. The list is a constant in
`permission_rules.rs`, checked against `claude -p` baselines. Suspended rules are listed on entering
`auto` (TUI notice, headless `init` event). An allow rule is a **user grant**: the containment fast path (step 4) and
Tier 0.5 (step 5) remain the only guardian skips earned without one, apart from the `agent` spawn (never guardian-judged, #360), the coordination tools (`send_message`, `board_update`; never guardian-judged, #394) and the opt-in
user-scope composition allow (step 7, `allow_below`, a verdict of the composed guardian), and no tool label or server
declaration ever creates an allow rule.

**Sources.** Allow rules are loosening, so they come only from user trust scopes: the user file,
an untracked `settings.local.toml`, and the CLI (`config.md` §Config scope stack); a shared project file may add
deny/ask but never allow. "Allow session" answers at the approval prompt are session-scope grants
held in `SessionState` (and recorded in the session ledger; never persisted); "always allow" appends
the exact rule shown in the prompt to the user file's `[permissions] allow`. A grant never
auto-extends the Tier-0.5 curated set.

## User hooks

Distinct from rig `AgentHook`s, which stay bookkeeping-only. User hooks are configured handlers
(`[[hooks.<event>]]`: a `matcher`, a handler, `timeout_secs`) that receive the event as JSON and
answer in Claude Code's shape (exit code 2 = block); the field names and output shape are pinned in
Phase 5 §User hooks. Events (one `HookEvent` enum in `config.rs` answers each event's matcher kind,
accepted handlers, project scope and default timeout): `pre_tool_use`, `post_tool_use`,
`post_tool_use_failure`, `permission_denied`, `task_created`, `task_completed`, `teammate_idle`,
`session_start`, `session_end`, `user_prompt_submit`, `stop`, `stop_failure`, `subagent_start`,
`subagent_stop`, `pre_compact`, `post_compact`, `notification`. Handlers: `command` (shell),
`http` (POST of the event; headers interpolate only `allowed_env_vars`, which may not name a scrubbed
variable) and `prompt` (one structured extraction on the guardian's model; `ok: false` blocks, it
never allows). Every handler normalizes to one `Answer` (blocked, json, plain, impossible) that the
event interpreters read. Only `pre_tool_use` `allow` and `permission_denied` `retry` loosen, and only
with user trust; no other event or handler can allow, and a hook's `continue: false` never ends a
turn. Context a hook adds is a harness note (`[Hook context from <event>, not the user's words: …]`),
never a ledger user message, so it cannot become guardian intent; it is capped at 10,000 characters.
`stop` and `subagent_stop` continuations are capped at 8 in a row (the streak resets when the agent
made tool calls since the previous stop, or on a human prompt) and `stop` runs before a `/goal`
evaluation. `pre_tool_use` runs at pipeline step
1a: it may return `deny`, `ask` or `allow`, an `updated_input`, and a `guardian_context` note
(passed to the guardian inside an untrusted block). Composition across hooks: deny > ask > allow;
the last `updated_input` in configured order wins and every later step judges it. A hook `allow` is
treated exactly as a matching allow rule — step 5a only, never past the floor; in `auto` a narrow
matcher's `allow` skips the guardian (Claude Code parity) and a broad matcher (by the allow-rule
test) is suspended. A hook that errors, times out or returns
malformed output contributes no decision; the call continues through the normal pipeline — hooks
never loosen on failure. The one exception tightens: on Windows without Git Bash, hooks run in PowerShell
and a `pre_tool_use` hook that exits with anything but 0 or 2, cannot start or times out denies the call
(`blocking because Git Bash was not found, so only exit code 0 or 2 counts as this hook's verdict`, Claude Code's rule); malformed output on exit 0 still decides nothing, and the
other events keep the rule above. Hook processes are user-configured, not tool calls, and are not gated by the
pipeline. Sources follow `config.md` §Config: user and CLI hooks load always; project and project-local hooks are
executables gated by workspace trust in the TUI, and a project hook's `allow` decision is ignored
(project scope only tightens). A repository's `http` and `prompt` hooks (data leaves the machine, or model
spend) and its `stop`, `subagent_stop` and `teammate_idle` hooks (forced model calls) are ignored with a
warning; user config, `--settings`, an untracked `settings.local.toml` of a trusted root and plugins may
define them. No key in any scope disables hooks from a higher-trust scope. Built in Phase 5.

## Composite guardian (the guardian tree)

Design direction, not a v1 build item: see the Decisions entry "guardian tree accepted as design direction".
