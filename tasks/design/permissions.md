# Permissions Design

Part of the Fontaine design; the master plan `tasks/fontaine_harness_plan.md` holds the Phase Tracker, workspace layout and `## Decisions`.

## Core data flow

```
 user ──▶ TUI ──▶ rig-agent runtime, orchestrated by fontaine-core
                      │  (AgentBuilder + AgentRun state machine; rig streams model events)
                      ▼
               ToolCall { name, args }
                      │
                      ▼  (every registered tool is a GuardedTool wrapper)
         PermissionPipeline (fontaine-core; GuardedTool::call reaches it through the `ToolGate` trait)
           1. registry.lookup(name) → RiskClass
           1a. User PreToolUse hooks (§User hooks): may return deny / ask / allow and an
              `updated_input` that replaces the call — every later step, the audit record and the
              guardian judge the rewritten call; their decision is held and applied below
              (deny → Deny here; ask → joins the step-2 forced prompt; allow → acts at step 5a only)
           2. Deterministic floor — evaluated BEFORE the mode branches, in EVERY mode (no mode,
              flag, or verdict reaches past it; it can only prompt or deny, never allow; order:
              deny → ask → critical-path → protected-path):
              `[permissions] deny` rule match → Deny; `[permissions] ask` rule match → force the
              human prompt (even in `auto`; a match in `dontAsk` denies); rules use the
              `Tool(specifier)` grammar (§Permission rules); critical-path check →
              `rm`/`rmdir` targets that are the filesystem root, a top-level dir, home, or a
              workspace root or one of its parents force the human prompt in
              manual/acceptEdits/plan/auto (never auto-approved by any mode or verdict) and Deny in
              `dontAsk`; protected-path check (a separate list, enumerated under Protected paths below; applies to
              the edit-class tools `edit`/`write`) → writes prompt in manual/acceptEdits, go to the
              guardian in `auto`, Deny in `dontAsk`; in `plan` edit-class is already blocked outright
              (Claude Code's protected-paths behavior). In headless `-p` every prompt outcome is a Deny. All
              structured matching (tool name + risk metadata + path containment), never regex on raw
              args. For `shell` calls the critical-path check parses the command with the same
              `brush-parser` pass as Tier 0.5 and inspects the targets of every `rm`/`rmdir` word (exact word
              equality), resolving each against the call's `cwd` argument (the model-supplied
              `ShellArgs.cwd`, not the process cwd); a target it cannot resolve (command substitution, variable-rooted glob), or
              a command that fails to parse, or parses as anything but one simple command, while
              containing an `rm`/`rmdir` word anywhere (every segment of `;`/`|`/`&&`/`||` lists is
              inspected), is treated as critical.
           3. mode == plan?  → no edit ever executes: ReadOnly-in-scope, Tier-0.5-eligible and
              allow-rule-matched non-edit calls proceed (steps 4–5a); everything else reaches the
              terminal plan-mode branch 9.
              Structurally blocked = WorkspaceWrite / edit-class only; write-capable/unparsed
              shell is NOT edit-class — it is adjudicated at branch 9 with the other
              consequential calls
           4. RiskClass::ReadOnly && args within workspace scope? → Allow (no guardian)
              (scope check enforced by Fontaine's wrapper/scope validator — `rig_tools::fs::place`
               against each canonicalized workspace root; inside any root counts — first-party only;
               MCP-declared readOnlyHint does not qualify — such tools register as Arbitrary;
               the `skill` tool qualifies: its only argument is a skill name resolved against the
               start-time index, and it loads skill text into context)
           5. Tier 0.5 — deterministic shell pre-classifier (zero-LLM), `shell` calls only:
              parsed by `brush-parser` (a real POSIX shell parser, pinned to an exact version — NOT
              regex, no hand-rolled lexer) into exactly ONE simple command (no control operators
              `; | & && ||`, redirections, substitutions, subshells, backgrounding, globs —
              anything else falls through) AND argv[0]+subcommand ∈ curated read-only set
              (`ls`, `pwd`, `wc`, `file`, `git status|log|diff|show`, `rg`/`grep`,
              `cat`/`head`/`tail`) AND every remaining token is either a path argument passing
              the step-4 scope validator (symlink-resolved; absolute / out-of-workspace paths
              never qualify) or a flag on a per-command read-only flag allowlist — any flag not
              explicitly allowlisted for that command, any glob/`~`/wildcard token, or any
              unparsed construct falls through to the guardian; values of file-opening flags
              (`--files-from`, `-f`, `-m`, etc.) are themselves treated as path arguments and
              validated (mirrors Claude Code's documented per-command flag handling; the initial
              allowlist table is in the Phase 3 plan)
              the call's `cwd` is resolved first: relative path arguments resolve against it and an
              out-of-scope `cwd` disqualifies the fast path
              → Allow, with an audit-log entry (no guardian)
           5a. Allow rules — only when the floor forced nothing (no Deny, prompt, or
              protected-path routing):
              `[permissions] allow` rule match (session grants in `SessionState` included) or a
              PreToolUse hook `allow` → Allow (no guardian), audit `tier = allow_rule` / `hook`;
              in `auto`, broad code-execution rules and hooks are suspended (§Permission rules);
              in `plan`, edit-class calls never get here
           6. mode == manual/acceptEdits → human prompt (acceptEdits: auto-allow edits in workdir)
           7. mode == auto → Guardian::adjudicate(request) ──▶ Verdict
           8. mode == dontAsk → anything not pre-allowed is Denied (pre-allowed = steps 4, 5, 5a)
           9. mode == plan (not allowed by steps 4–5a): WorkspaceWrite / edit-class → BLOCKED
              outright (structural; no verdict or escalation can override the edit block while
              plan mode is on); every other consequential call — write-capable or unparsed shell,
              webfetch, out-of-scope reads, ALL MCP calls (anything non-shell `Arbitrary`) →
              Guardian::adjudicate as in auto (CC parity: plan mode routes consequential calls
              through the classifier by default; never edits). Until the guardian ships (Phase 4),
              these step-9 calls fall through to the human prompt.
                      │
         Allow ──▶ inner rig Tool executes ──▶ ResultPipeline ──▶ result appended to conversation (rig-memory)
         Deny  ──▶ tool_result = "Denied by guardian: [reason]"; agent sees it and adapts
         Escalate ──▶ human prompt (with guardian's explanation shown)

 (the `agent` tool call that spawns a subagent is an ordinary `Arbitrary`-class pipeline request —
  gated at steps 6–9 like any other; every tool call the subagent then makes re-enters this same
  pipeline)

  `ResultPipeline` is the post-call step inside `GuardedTool` (Phase 2) through which every tool result
 passes before the agent sees it, in fixed order: the tool-result scanner (Phase 6), then `post_tool_use`
 user hooks (Phase 5).

 Tool calls within one model turn are adjudicated and executed one at a time, in order (rig-agent
 `tool_concurrency = 1`); each call gets its own tool_result and a Deny does not affect the turn's
 later calls.
```

**Protected paths** (step 2, last floor check): applies to the edit-class tools `edit` and `write`
— the only registered tools that write structurally (`delete`/`move` are not registered; shell writes
are not structurally detectable without regex on args, so they take the ordinary shell path: guardian
in `auto`, prompt in manual/acceptEdits). Matched on resolved path components, in any workspace root or
the home directory. Directories: `.git`, `.vscode`, `.idea`, `.husky`, `.fontaine`. Files: `.gitconfig`,
`.gitmodules`, `.bashrc`, `.bash_profile`, `.zshrc`, `.zprofile`, `.profile`, `.ripgreprc`,
`.mcp.json`. This follows Claude Code's documented protected-paths list (`.fontaine` in place of
`.claude`); confirm it against a `claude -p` baseline in Phase 3 and record any difference in Decisions.
Per mode: manual/acceptEdits → prompt (acceptEdits' edit auto-allow does not apply); auto → guardian;
plan → not reachable (edit-class is blocked at steps 3/9); dontAsk → Deny; headless → Deny.

**Audit log** (written by the pipeline in every mode; one record per gate decision): JSONL, one
append-only file per session at `<data dir>/audit/<session-id>.jsonl`, owner-only.
Fields: `ts` (RFC 3339), `session_id`, `agent_id`, `tool`, `args_digest` (SHA-256 of the
canonical-JSON args — raw args are never logged; when a hook rewrote the call, `original_args_digest`
too), `tier` (the tier enum below), `decision` (`allow` | `deny` | `prompt`), `rule` (deny/ask/allow rule text and
its source scope, hook id, floor check, or guardian rule label; null if none), `confidence` and `guardian_model`
(`<account>/<model>`) for guardian decisions, `latency_ms`. A human answer to a prompt appends a
second record with `tier = human`.

**Tier enum** (defined once, on `GateDecision.tier`; the audit log, the `tool_decision` telemetry
event, the headless `gate_decision` event and the eval `expected_tier` all carry exactly these values,
with no projection or renaming): `deny_rule` | `ask_rule` | `critical_path` | `protected_path` (the
floor, in check order) | `hook` | `read_only_fast_path` (step 4) | `tier05` (step 5) | `allow_rule`
(step 5a) | `mode` (step 6: manual/acceptEdits prompt, acceptEdits edit auto-allow) | `guardian` (steps 7, 9) |
`dont_ask` (step 8) | `plan_block` (step 9 edit block) | `human`.

**Pipeline inputs**: mode, the workspace roots, the merged permission rules plus session-scope
grants, the user-hook runner, the session ledger (source of task intent, recent user messages,
session facts and prior denials — `sessions.md` §Session ledger), the audit-log sink (schema above), and the
human-approval callback live in a shared `SessionState` owned by the `PermissionPipeline`, which is
injected into every wrapper at registration as the `ToolGate` (see the Phase 2 plan).

**Headless (`-p`)**: nothing prompts. Every would-prompt outcome — `ask`-rule match, Escalate
verdict, critical-path/protected-path prompt, loop breaker — becomes a
Deny returned to the agent as the tool_result, and the run continues (Claude Code parity). In headless `auto`, an unavailable pinned guardian makes every adjudication `Deny GuardianUnavailable`, with a notice in the `init` event and on stderr. Exit
codes: 0 run completed (denials do not change it), 1 run error (provider/model failure,
interruption), 2 config or usage error. Output and input follow the versioned headless I/O contract
(`--output-format text|json|stream-json`; see the Phase 1 plan), whose `gate_decision` events carry
the same tier attribution as the audit log.

## Permission rules (deny / ask / allow)

One grammar for `[permissions] deny`, `ask` and `allow` (and the CLI `--disallowed-tools` /
`--allowed-tools`): `Tool` or `Tool(specifier)`. A bare tool name matches every call of that tool.
Matching is structured — never regex on raw args.

| Tool | Specifier | Match |
|---|---|---|
| `shell` | words, optional trailing ` *` (`shell(git push)`, `shell(npm run test *)`) | argv from the Tier-0.5 `brush-parser` pass (quoted words stay one argument): exact argv, or argv prefix when the rule ends in ` *`; no wildcard inside a word |
| `read`, `edit`, `write`, `ls`, `glob`, `grep` | gitignore-style path pattern (`src/**`, `~/.aws/**`, `//etc/hosts`) | relative patterns anchor at each workspace root; the path argument is resolved by the scope validator (symlinks, `..`, the call's `cwd`) and the resolved path is matched |
| `webfetch` | `domain:<host>` or `domain:*.<host>` | parsed URL host (lowercased, IDNA-normalized); `*.` matches subdomains only |
| MCP | `mcp__<server>`, `mcp__<server>__<tool>`, `mcp__<server>__*` | registered tool name |
| `agent`, `skill` | agent type / skill name | exact name |

`webfetch` never follows a cross-host redirect — the redirect URL is returned to the model and a
follow-up fetch is gated on its own host — so an allowed domain cannot bounce a request to an
unlisted one.

**Asymmetric failure.** Deny and ask fail toward matching; allow fails toward not matching. A
`shell` deny/ask rule is always a prefix match (trailing ` *` implied) and matches when its words appear
contiguously anywhere in the parsed word stream (control operators split off first, so `ls && git push`
and `sudo git push -f` hit `deny shell(git push)`),
and a command that fails to parse, or puts a substitution or variable in command position, matches every
deny/ask `shell` rule whose first word appears in it (the critical-path posture). A `shell` allow rule
matches only a command the Tier-0.5 parser accepts as one simple command (no chaining, pipes,
redirections, substitution, globs) whose argv matches; anything else falls through. Path deny/ask
rules match the given path and the resolved path; path allow rules match the resolved path only.

**Where allow acts.** Allow rules are pipeline step 5a: after the floor, never past it — a deny, ask,
critical-path or protected-path outcome stands whatever allow says, and `plan` still blocks edits. A
matching allow rule skips the human prompt in manual/acceptEdits, pre-approves in `dontAsk`, and
skips the guardian in `auto` (Claude Code parity). In `auto`, **broad code-execution allow rules are
suspended** for as long as the mode is on (Claude Code parity): bare `shell`; `shell(<w> *)` where
`<w>` is an interpreter, shell or package-manager runner on a shipped list (`python`, `python3`,
`node`, `deno`, `bun`, `ruby`, `perl`, `bash`, `sh`, `zsh`, `npx`, `npm run`/`exec`, `pnpm`, `yarn`,
`uv run`, `cargo run`, …); bare `agent`. Suspended rules are listed on entering `auto` (TUI notice;
headless `init` event). An allow rule is a **user grant**: the containment fast path (step 4) and
Tier 0.5 (step 5) remain the only guardian skips earned without one, and no tool label or server
declaration ever creates an allow rule.

**Sources.** Allow rules are loosening, so they come only from user trust scopes: the user file,
an untracked `settings.local.toml`, and the CLI (`config.md` §Config scope stack); a shared project file may add
deny/ask but never allow. "Allow session" answers at the approval prompt are session-scope grants
held in `SessionState` (and recorded in the session ledger; never persisted); "always allow" appends
the exact rule shown in the prompt to the user file's `[permissions] allow`. A grant never
auto-extends the Tier-0.5 curated set.

## User hooks

Distinct from rig `AgentHook`s, which stay bookkeeping-only. User hooks are configured handlers
(`[[hooks.<event>]]`: a `matcher` in the rule grammar, a `command`, `timeout_secs`) that receive the
event as JSON on stdin and answer on stdout (exit code 2 = deny, Claude Code's convention). v1
events: `pre_tool_use`, `post_tool_use`, `permission_denied`. `pre_tool_use` runs at pipeline step
1a: it may return `deny`, `ask` or `allow`, an `updated_input`, and a `guardian_context` note
(passed to the guardian inside an untrusted block). Composition across hooks: deny > ask > allow;
the last `updated_input` in configured order wins and every later step judges it. A hook `allow` is
treated exactly as a matching allow rule — step 5a only, never past the floor; in `auto` a narrow
matcher's `allow` skips the guardian (Claude Code parity) and a broad matcher (by the allow-rule
test) is suspended. A hook that errors, times out or returns
malformed output contributes no decision; the call continues through the normal pipeline — hooks
never loosen on failure. Hook processes are user-configured, not tool calls, and are not gated by the
pipeline. Sources follow `config.md` §Config: user, project-local and CLI hooks load always; project hooks are
project-defined executables (workspace trust) and their `allow` decisions are ignored (project scope
only tightens). No key in any scope disables hooks from a higher-trust scope. Built in Phase 5.

## Composite guardian (the guardian tree)

Design direction, not a v1 build item: see the Decisions entry "guardian tree accepted as design direction".
The typed-decision candidate screen tier is covered in the Phase 4 plan (guardian model list).
