# Phase 3 — Permission pipeline & manual TUI

**Status:** not started · **Depends on:** [Phase 2](phase2_runtime_tools.md) · **Master:** [fontaine_harness_plan.md](../fontaine_harness_plan.md)

Fills the Phase-2 `GuardedTool` stub with the real gate: the pipeline order is specified by the
master plan's §Core data flow diagram (steps 1–9). **Milestone: usable Claude-Code-like agent
with manual approvals.**

## Design (excerpt owned by this phase)

### Permission modes

Five modes with the **same semantics as Claude Code**: `manual`, `acceptEdits`, `plan`, `auto`,
`dontAsk`. The shipped v1 default is `auto` (Claude Code parity — its built-in starting mode since
v2.1.283+); during the Phase-3 interim the default is `manual`, because nothing runs `auto` before
the guardian exists (Phase 4) — no auto without a guardian. **Deliberate subtraction from Claude
Code**: `bypassPermissions` is not shipped at all in v1 — there is no permission-bypass mode, so
the deterministic deny → ask → critical-path floor (and, from Phase 4, the guardian) gates every
call in every mode (reasoning in Decisions). Shift+Tab cycles manual → acceptEdits → plan → auto.
Keeping the same set so muscle memory transfers; the difference is `auto` is powered by our local
guardian against any model.

### TUI (fontaine-tui) — base surface

ratatui + crossterm, codex-rs-style: transcript pane (streaming assistant text, collapsed tool
calls), input box, status bar showing mode (`⏵⏵ auto mode on`). Approval prompt widget for
manual/escalate (allow once / allow session / always allow / deny + feedback): "allow session" adds
a session-scope grant to `SessionState` (recorded in the ledger); "always allow" shows the exact
`Tool(specifier)` rule it will write and appends it to the user file's `[permissions] allow`; the
proposed rule is the narrowest that covers the call (exact argv for `shell`, the resolved path for
path tools, the host for `webfetch`). `/permissions` edits rules; `/add-dir <path>` adds a
workspace root; `/model` switches live; Shift+Tab cycles modes. (The denial UX — inline `[Rule]` rendering,
`/denied`, `/guardian-model` — is designed and built in Phase 4; the background-agents panel in
Phase 5; see those plans.)

### Tier-0.5 initial allowlist

Conservative starting table; any flag not listed, any `--flag=value` whose flag is not listed, and
any combined short-flag cluster containing an unlisted letter fall through to the guardian (`ls -la`
is accepted as `-l -a`). Positional tokens are validated as paths (a ref name such as `HEAD` resolves
in scope and passes). Numeric flag values are parsed as integers, not pattern-matched. Check the
table against `claude -p` baselines (step 3) and record differences in Decisions.

| Command | Allowed flags | Values validated as paths |
|---|---|---|
| `ls` | `-a -A -l -h -1 -d -t -r -S -F` | positionals |
| `pwd` | none | — |
| `wc` | `-l -w -c -m` | positionals |
| `file` | `-b -i --mime --mime-type --brief` | positionals; `-m`/`--magic-file` value. `-f`/`--files-from` not allowed (indirect reads) |
| `cat` | `-n -b -s` | positionals |
| `head`, `tail` | `-n N -c N -q -v` (`tail -f`/`-F` not allowed) | positionals |
| `rg` | `-i -s -S -n -N -H -w -x -F -v -c -l -A N -B N -C N -m N -t TYPE -T TYPE --hidden --no-heading` | positionals; `-f`/`--file`, `--ignore-file` values. `--pre`, `--pre-glob`, `--hostname-bin`, `-z`/`--search-zip`, `-L`/`--follow`, `-g`/`--glob` not allowed |
| `grep` | `-i -n -v -w -x -F -E -c -l -L -H -h -o -q -s -r -A N -B N -C N` | positionals; `-f`/`--file` value. `-R` and `--exclude-from` not allowed |
| `git status` | `-s --short -b --branch --porcelain` | positionals after `--` |
| `git log`, `git diff`, `git show` | `--oneline --stat --name-only --name-status --graph --decorate --no-color --cached --staged -n N -U N` | positionals after `--`. Only when `--no-ext-diff --no-textconv` are also present (repo config can name external diff/textconv programs); `--output`, `--ext-diff`, `--textconv` not allowed |

`git` must have the subcommand as `argv[1]` (`git -c …` and `git -C …` fall through).

### Permission rules and allow (pipeline steps 2 and 5a)

The master plan's §Permission rules is the spec: one `Tool(specifier)` grammar (parsed at load since
Phase 0) for deny/ask/allow; `shell` specifiers matched on the Tier-0.5 shlex-lexed argv — deny/ask
as a prefix anywhere in the word stream and on any unlexable command containing the rule's first
word, allow only on one simple command; path specifiers gitignore-style against scope-validator
resolved paths; `webfetch(domain:)`; MCP names. Allow rules are step 5a, after the floor; the broad
code-execution list that `auto` suspends is a shipped constant in `permission_rules.rs`, applied when
Phase 4 wires `auto`. The pipeline also gets its step-1a slot: a `PreToolUseDecision` input
(deny / ask / allow / `updated_input`) with the composition and failure rules of the master's §User
hooks, driven in this phase only by test doubles — user hooks load in Phase 5.

`GateDecision` is the one typed record per call: written to the audit log first, then projected to
the `tool_decision` telemetry event and the headless `gate_decision` event; all three carry the master's one tier enum unchanged.

## Steps

1. `PermissionPipeline` (implements the `ToolGate` trait, replacing the Phase-2 stub; injected at registration) with modes `manual`/`acceptEdits`/`plan`/`dontAsk` (`auto` wires in at
   Phase 4 step 2; there is no `bypass` mode) and the step order from the master's data-flow
   diagram (the step-2 deterministic floor: deny → ask → critical-path (rm/rmdir targets prompt; deny in
   `dontAsk`/headless) → protected-path (the master's enumerated list; `edit`/`write` only — prompt in manual/acceptEdits,
   deny in `dontAsk`/headless), in every mode, with shell commands lexed by the Tier-0.5 shlex lexer; steps 4–5:
   read-only fast path, then the Tier-0.5 shell pre-classifier with its curated set +
   per-command read-only flag allowlist (initial table above) + audit log, schema per the master
   §Core data flow); rule matching and allow rules (design above; `--allowed-tools` /
   `--disallowed-tools` as CLI-scope rules); the step-1a hook-decision slot; `GateDecision` → audit
   log, then `tool_decision` telemetry and the headless `gate_decision` event; `fontaine.permission`
   and `fontaine.tool.blocked_on_user` spans, `permission_mode_changed` event; human-approval
   callback trait.
   Plan mode's step-9 guardian leg falls through to the human prompt until the guardian ships.
   Interim default mode: `manual` — no auto without a guardian. Headless `-p` semantics for
   manual/dontAsk: every would-prompt outcome becomes a Deny returned to the agent and the run
   continues (master §Core data flow, Headless).
2. `fontaine-tui`: transcript, streaming render, input, approval prompts (incl. allow session /
   always allow), status bar, mode cycling, `/add-dir`.
3. Differential baseline (manual/eval-side, never CI — see AGENTS.md): implement the `fontaine-evals`
   `baseline capture` / `baseline diff` subcommands (master Testing Strategy, Differential
   baselines) and run them for manual/acceptEdits/plan/dontAsk over the Tier-0.5 adversarial set and
   the floor/headless cases; also confirm the protected-path list and the allowlist table against
   the captured `claude -p` behavior. Fix or record each divergence.
4. Docs — `docs/permissions.md`: the mode table (`manual`/`acceptEdits`/`plan`/`dontAsk`, the
   interim default and its Phase-4 flip to `auto`), the deterministic floor (deny → ask →
   critical-path with its rm/rmdir list, protected-path with its enumerated list), the
   `Tool(specifier)` rule grammar and allow rules (sources, step order, session vs always grants,
   suspension of broad rules in `auto`), and the Tier-0.5 pre-classifier (curated set,
   per-command read-only flag allowlist, fail-through rule, `[shell_preclassifier] allow_extra` —
   user-scope only); complete the full permission-pipeline diagram in `docs/architecture.md`;
   start `docs/tui.md` (keybinds incl. Shift+Tab, approval-prompt options, `/add-dir`); add
   `--allowed-tools` / `--disallowed-tools` and the `gate_decision` event to `docs/headless.md`; add
   the `tool_decision` event and permission spans to `docs/telemetry.md`; fill the README mode
   table; append a CHANGELOG entry.

## Files created or modified

`rust/fontaine-core/src/{permissions,permission_rules,audit}.rs`; `rust/fontaine-evals/src/baseline.rs`;
`rust/fontaine-tools/src/preclassify.rs`; `rust/fontaine/tests/cassettes.rs`;
`rust/fontaine-tui/src/{app,transcript,approval,statusbar}.rs`; tests under `rust/fontaine-core/tests/`,
`rust/fontaine/tests/`, `rust/fontaine-tui/tests/`;
`rust/fontaine/src/main.rs` (TUI wiring); `docs/{permissions,tui,headless,telemetry}.md`, `docs/architecture.md`,
`docs/index.md`, `README.md`.

## Tests owned by this phase

- **permission pipeline** (master Testing Strategy; `rust/fontaine-core/tests/permissions.rs`): mode
  × risk-class matrix; plan-mode terminal branch 9 (edit-class blocked outright; the guardian leg
  prompts in the interim and denies in headless); read-only scope check incl. symlink-escape denial
  and an out-of-scope `cwd`; sequential tool calls in one turn (a Deny returns its own tool_result; later calls are still adjudicated, one at a time); the `[permissions] deny` rule
  holds in EVERY mode; critical-path `rm`/`rmdir` (incl. unresolvable targets and commands that fail
  to lex) prompts in manual/acceptEdits/plan, denies in `dontAsk` and headless, and is never
  auto-approved; protected-path writes via `edit`/`write` (the enumerated list) prompt or deny per
  mode; an `[permissions] ask` rule forces a prompt in `manual` and denies in `dontAsk`; every
  decision appends an audit record matching the master schema (`tier`, `decision`, `rule`,
  `args_digest`, `agent_id`).
- **permission rules** (`rust/fontaine-core/tests/permission_rules.rs`): the master Testing Strategy
  bullet minus the `auto` legs — grammar per tool kind; `shell` deny/ask hit compound, wrapped and
  unlexable commands; `shell` allow never matches a compound command; resolved-path matching incl.
  symlinks; `webfetch(domain:)` normalization; a redirect leaving an allowed domain is returned, not followed; an allow rule never passes deny/ask/critical-path/
  protected-path and never lets an edit run in `plan`; an allow rule pre-approves in `dontAsk` and
  skips the prompt in `manual`; "allow session" lasts only for the session, "always allow" writes
  exactly the shown rule to the user file; `--allowed-tools` / `--disallowed-tools` act as CLI rules;
  a project-scope allow rule is rejected at load.
- **windows** (`#[cfg(windows)]`; compile-checked locally with `cargo check --all-targets --target x86_64-pc-windows-gnu`, executed in the windows CI job): critical-path prompts for a drive root and for
  `%USERPROFILE%` targets; path-rule matching is case-insensitive and resolves junctions; protected
  paths match across separators and case.
- **hook slot**: with a test-double decision source, `updated_input` is what the floor, rules and
  audit see; deny > ask > allow; an `allow` never passes the floor; error / timeout / malformed ⇒ no
  decision.
- **audit/telemetry consistency**: over the mode × risk-class matrix, each audit entry has exactly one
  matching `tool_decision` event and one headless `gate_decision` event (same ids; `tier` and `decision` equal as strings);
  the audit log is still written when telemetry is off or disabled by the project.
- **Tier 0.5 pre-classifier**: the golden adversarial set from master Testing Strategy — the
  fail-through soundness property must hold for anything not fully parsed as one simple command; each
  allowlist-table row (listed flags pre-classify, the excluded flags fall through); in-scope `ls` → no
  prompt plus an audit record with `tier = tier05`; `ls ../` → one human prompt (interim; the
  counting-guardian form is owned by Phase 4); a relative path under an out-of-scope `cwd` falls
  through.
- **headless** (`rust/fontaine/tests/pipeline_headless.rs`, scripted `openai-compatible` endpoint via the Phase-1 `common/mod.rs` helper):
  every would-prompt outcome becomes a Deny returned as the tool_result, the run continues, exit
  code 0.
- **TUI** (`rust/fontaine-tui/tests/`): ratatui `TestBackend` with injected key events and a scripted
  `rig_core::test_utils` model — approval prompt → allow runs the tool (file written), deny returns
  the denial tool_result; Shift+Tab cycles manual → acceptEdits → plan → (auto once Phase 4 lands;
  wraps to manual until then).

## Definition of done

- Offline gates (no keys, no human): `cargo fmt` / `cargo clippy --all-targets -- -D warnings` /
  `cargo test` green, including the scripted-endpoint headless, pipeline, permission-rule, and TUI tests above; interim default is
  `manual`; no mode or allow rule reaches past the deterministic floor; `docs/permissions.md` + pipeline diagram
  match the shipped step order and `rust/fontaine-core/tests/docs.rs` is green; CHANGELOG entry appended.
- Needs a human (the agent cannot push): push the branch and confirm the windows CI job runs the `#[cfg(windows)]` tests green.
- Needs a human and live keys (not CI): Manual Verification item 1 passes (TUI manual approval
  flow, end to end); the first rig-cassette recording of a manual-mode session via `cassettes.rs`
  (`RIG_PROVIDER_TEST_MODE=record`, account `base_url` at the cassette proxy; once committed it
  replays in CI with no keys); the `claude -p` baseline capture.
- Log the phase in the master plan's Progress log.
