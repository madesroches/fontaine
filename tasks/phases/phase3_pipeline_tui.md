# Phase 3 — Permission pipeline & manual TUI

**Status:** not started · **Depends on:** [Phase 2](phase2_runtime_tools.md) · **Master:** [fontaine_harness_plan.md](../fontaine_harness_plan.md)

Fills the Phase-2 `GuardedTool` stub with the real gate: the pipeline order is specified by the
`tasks/design/permissions.md` §Core data flow diagram (steps 1–9). **Milestone: usable Claude-Code-like agent
with manual approvals.**

## Design (excerpt owned by this phase)

### Permission modes

Five modes with the **same semantics as Claude Code**: `manual`, `acceptEdits`, `plan`, `auto`,
`dontAsk`. The shipped v1 default is `auto` (Claude Code parity — its built-in starting mode since
v2.1.283+); during the Phase-3 interim the default is `manual`, because nothing runs `auto` before
the guardian exists (Phase 4) — no auto without a guardian. `bypassPermissions` is not shipped, so
the deterministic deny → ask → critical-path → protected-path floor (and, from Phase 4, the guardian) gates every
call in every mode. Shift+Tab cycles manual → acceptEdits → plan → auto. `auto` is powered by our local
guardian against any model.

### TUI (fontaine-tui) — base surface

ratatui + crossterm, codex-rs-style: transcript pane (streaming assistant text, collapsed tool
calls), input box, status bar showing mode (`⏵⏵ auto mode on`). Approval prompt widget for
manual/escalate (allow once / allow session / always allow / deny + feedback): "allow session" adds
a session-scope grant to `SessionState` (recorded in the ledger); "always allow" shows the exact
`Tool(specifier)` rule it will write and appends it to the user file's `[permissions] allow` and adds it to `SessionState`'s allow rules immediately, so the next matching call in the same session skips the prompt (user-file writes — "always allow", `/model`, `/guardian-model` — go through `toml_edit =0.25.17`, preserving the file's formatting and comments); the
proposed rule is the narrowest that covers the call (exact argv for `shell`, the resolved path for
path tools, the host for `webfetch`). `/add-dir <path>` adds a
workspace root; Shift+Tab cycles modes. (The denial UX — inline `[Rule]` rendering,
`/denied`, `/guardian-model` — is designed and built in Phase 4; the background-agents panel in
Phase 5; see those plans.)

### Tier-0.5 initial allowlist

Conservative starting table; any flag not listed, any `--flag=value` whose flag is not listed, and
any combined short-flag cluster containing an unlisted letter fall through to the guardian (`ls -la`
is accepted as `-l -a`). Positional tokens are validated as paths. Numeric flag values are parsed as integers, not pattern-matched. Check the
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

No `git` subcommand is in the curated set: every git invocation goes to the guardian, because repo-controlled config and attributes can make a read-only subcommand execute a program.

### Permission rules and allow (pipeline steps 2 and 5a)

The `tasks/design/permissions.md` §Permission rules is the spec: one `Tool(specifier)` grammar (parsed at load since
Phase 0; Phase 0 checks only the outer shape, tool name, `mcp__…` names and `webfetch` `domain:` form, and this phase adds per-kind specifier parsing behind the same load-time error) for deny/ask/allow; `shell` specifiers matched on the Tier-0.5 `brush-parser` argv — deny/ask
as a prefix anywhere in the word stream and on any unparseable command containing the rule's first
word, allow only on one simple command; path specifiers gitignore-style (matched with the `ignore` crate, `=0.4.33`) against scope-validator
resolved paths; `webfetch(domain:)`; MCP names. Allow rules are step 5a, after the floor; the broad
code-execution list that `auto` suspends (`tasks/design/permissions.md` §Permission rules, Where allow acts) is a shipped
constant in `permission_rules.rs`, applied when Phase 4 wires `auto`. The pipeline also gets its step-1a slot: a `PreToolUseDecision` input
(deny / ask / allow / `updated_input`) with the composition and failure rules of `tasks/design/permissions.md` §User
hooks, driven in this phase only by test doubles — user hooks load in Phase 5.

`GateDecision` is the one typed record per call: written to the audit log first, then projected to
the `tool_decision` telemetry event and the headless `gate_decision` event; all three carry the one tier enum in `tasks/design/permissions.md` §Core data flow unchanged.

## Steps

1. `PermissionPipeline` (implements the `ToolGate` trait, replacing the Phase-2 stub; injected at registration) with modes `manual`/`acceptEdits`/`plan`/`dontAsk` (`auto` wires in at
   Phase 4 step 2; there is no `bypass` mode) and the step order from the `tasks/design/permissions.md` §Core data flow
   diagram (the step-2 deterministic floor: deny → ask → critical-path (rm/rmdir targets prompt; deny in
   `dontAsk`/headless) → protected-path (the enumerated list in `tasks/design/permissions.md` §Core data flow; `edit`/`write` only — prompt in manual/acceptEdits,
   deny in `dontAsk`/headless), in every mode, with shell commands parsed by the Tier-0.5 `brush-parser` pass (`brush-parser =0.4.0` in `fontaine-tools`; no hand-rolled lexer); steps 4–5:
   read-only fast path, then the Tier-0.5 shell pre-classifier with its curated set +
   per-command read-only flag allowlist (initial table above) + `allow_extra` entries (a command or `command subcommand` pair; no flags — any flag falls through; positional args validated as in-scope paths) + audit log, schema per
   `tasks/design/permissions.md` §Core data flow); rule matching and allow rules (design above; `--allowed-tools` /
   `--disallowed-tools` as CLI-scope rules: two repeatable flags in `rust/fontaine/src/cli.rs`, merged by `rust/fontaine-core/src/config.rs` into the CLI layer's `permissions.deny`/`allow` and validated by the same load-time rule parser); the step-1a hook-decision slot; `GateDecision` → audit
   log, then `tool_decision` telemetry and the headless `gate_decision` event; `fontaine.permission`
   and `fontaine.tool.blocked_on_user` spans, `permission_mode_changed` event; human-approval
   callback trait.
   Plan mode's step-9 guardian leg falls through to the human prompt until the guardian ships (audit `tier = mode`, including its headless Deny).
   Interim default mode: `manual` — no auto without a guardian. Headless `-p` semantics: `tasks/design/permissions.md` §Headless.
2. `fontaine-tui`: transcript, streaming render, input, approval prompts (incl. allow session /
   always allow), status bar, mode cycling, `/add-dir`.
3. Differential baseline (manual/eval-side, never CI — see AGENTS.md): implement the `fontaine-evals`
   runner mode `--guardian none` (`tasks/design/testing.md` Testing Strategy, Eval harness: `pipeline` cases through the real `PermissionPipeline` for the non-auto modes) and the
   `baseline capture` / `baseline diff` subcommands (Differential
   baselines; `diff` runs against a `--guardian none --out` file) and run them for manual/acceptEdits/plan/dontAsk over the Tier-0.5 adversarial set and
   the floor/headless cases (the cases' `permissions` field is written as equivalent `.claude/settings.json` permissions in `Tool(specifier)` syntax in the scratch workspace); also confirm the protected-path list and the allowlist table against
   the captured `claude -p` behavior. Fix or record each divergence. In `none` mode each case's risk class comes from `builtin_risk(tool)`, falling back to `Arbitrary` as `risk_of` in `fontaine-tools/src/builtins.rs` does (`agent` is not in `BUILTIN_RISK`). A symlink-creation failure or a dangling link in a case workspace (`tier05-symlink-escape`, `tier05-out-of-scope-cwd` on Windows) counts as out of scope, not a runner error. Add `cargo run -p fontaine-evals -- --dataset ../evals/dataset --guardian none` (from `rust/`) to `ci.yml` on all three OSes.
4. Cassette test (offline build; recording stays the human step): `rust/fontaine/tests/cassettes.rs` with the manual-mode
   test (in-scope read round-trip) marked `#[ignore = "cassette not recorded"]`, the rig-cassette scrubbing
   configuration, and the account `base_url` pointed at the cassette proxy.
5. Docs — `docs/permissions.md`: the mode table (`manual`/`acceptEdits`/`plan`/`dontAsk`; the default is
   `manual`; `auto` is not yet available), the deterministic floor (deny → ask →
   critical-path with its rm/rmdir list → protected-path with its enumerated list), the
   `Tool(specifier)` rule grammar and allow rules (sources, step order, session vs always grants,
   suspension of broad rules in `auto`), and the Tier-0.5 pre-classifier (curated set,
   per-command read-only flag allowlist, fail-through rule, `[shell_preclassifier] allow_extra` —
   user trust scopes only (per the config scope table), flag-free, positionals must be in-scope paths); complete the full permission-pipeline diagram in `docs/architecture.md`, update its crate tree (add `preclassify`; `fontaine-tui` is no longer a skeleton) and module table (`permissions`/`audit` modules);
   start `docs/tui.md` (keybinds incl. Shift+Tab, approval-prompt options, `/add-dir`); add
   the `--allowed-tools` / `--disallowed-tools` flags to the `docs/headless.md` flag table and rewrite its Denials paragraph (no longer deny-by-default; `gate_decision` is emitted; `result.denials` is populated); add
   the `tool_decision` event and permission spans to `docs/telemetry.md`; in `docs/tools.md` replace "Permissions today" with a pointer to `docs/permissions.md` and repoint the inbound links in `docs/headless.md` and `docs/configuration.md`; in `docs/configuration.md` update the `[permissions]` intro, the `allow_extra` "not yet active" note and the CLI table (add the two flags); add `docs/index.md` entries for `docs/permissions.md` and `docs/tui.md`; fill the README mode
   table; append a CHANGELOG entry.

## Files created or modified

`rust/Cargo.toml` (new crates: dev `rig-cassette =0.45.0` (features `http`, `agent`), `ratatui =0.30.2` (crossterm via `ratatui::crossterm`, no direct pin), `brush-parser =0.4.0`, `toml_edit =0.25.17`, `sha2 =0.11.1` (audit `args_digest`), `serde_jcs =0.2.0` (args canonicalization)); `rust/fontaine-core/src/{permissions,permission_rules,audit}.rs`; `.github/workflows/ci.yml` (adds the `--guardian none` eval step on all three OSes); `rust/fontaine-evals/src/baseline.rs`; `rust/fontaine-evals/tests/{baseline,none_mode}.rs`;
`rust/fontaine-tools/src/preclassify.rs`; `rust/fontaine/tests/cassettes.rs` (manual-mode cassette, built in step 4; the auto-mode cassette is added in Phase 4);
`rust/fontaine-tui/src/{app,transcript,approval,statusbar}.rs`; tests under `rust/fontaine-core/tests/`,
`rust/fontaine/tests/`, `rust/fontaine-tui/tests/`;
`rust/fontaine/src/{lib,cli}.rs` (TUI wiring; the two rule flags); `rust/fontaine-core/src/config.rs` (CLI rule merge); `docs/{permissions,tui,headless,telemetry,tools,configuration}.md`, `docs/architecture.md`,
`docs/index.md`, `README.md`.

## Tests owned by this phase

- **permission pipeline** (`tasks/design/testing.md` Testing Strategy; `rust/fontaine-core/tests/permissions.rs`): mode
  × risk-class matrix; plan-mode terminal branch 9 (edit-class blocked outright; the guardian leg
  prompts in the interim and denies in headless; in-scope `shell("ls")` proceeds via Tier 0.5); read-only scope check incl. symlink-escape denial
  and an out-of-scope `cwd`; sequential tool calls in one turn (a Deny returns its own tool_result; later calls are still adjudicated, one at a time); the `[permissions] deny` rule
  holds in EVERY mode; critical-path `rm`/`rmdir` (incl. unresolvable targets and commands that fail
  to parse or are compound) prompts in manual/acceptEdits/plan, denies in `dontAsk` and headless, and is never
  auto-approved; critical-path covers every workspace root and its parents; protected-path writes via `edit`/`write` (the enumerated list) prompt or deny per
  mode; an `[permissions] ask` rule forces a prompt in `manual` and denies in `dontAsk`; every
  decision appends an audit record matching the audit schema in `tasks/design/permissions.md` §Core data flow (`tier`, `decision`, `rule`,
  `args_digest`, `agent_id`).
- **permission rules** (`rust/fontaine-core/tests/permission_rules.rs`; the `auto` legs are Phase 4): grammar per tool kind; `shell` deny/ask hit compound, wrapped and
  unparseable commands; `shell` allow never matches a compound command; resolved-path matching incl.
  symlinks (a symlink into a denied path is denied; an allow on a symlinked alias does not match); `webfetch(domain:)` normalization; a redirect leaving an allowed domain is returned, not followed; an allow rule never passes deny/ask/critical-path/
  protected-path and never lets an edit run in `plan`; an allow rule pre-approves in `dontAsk` and
  skips the prompt in `manual`; "allow session" lasts only for the session, "always allow" writes
  exactly the shown rule to the user file and the next matching call in the same session skips the prompt; `--allowed-tools` / `--disallowed-tools` act as CLI rules.
- **windows** (`#[cfg(windows)]`; compile-checked locally with `cargo clippy --all-targets --target x86_64-pc-windows-gnu -- -D warnings`, executed in the windows CI job): critical-path prompts for a drive root and for
  `%USERPROFILE%` targets; path-rule matching is case-insensitive and resolves junctions; protected
  paths match across separators and case.
- **baseline** (`rust/fontaine-evals/tests/baseline.rs`): `diff` over constructed baseline and run files reports exactly the cases whose outcomes differ under the mapping in `tasks/design/testing.md` (Fontaine `allow` ↔ CC allowed; `deny` and `prompt` ↔ CC denied in headless); denial extraction from a checked-in sample `claude -p --output-format json` result (fixture under `rust/fontaine-evals/tests/fixtures/`).
- **eval `none` mode** (`rust/fontaine-evals/tests/none_mode.rs`): `pipeline` cases for manual/acceptEdits/plan/dontAsk run through the real `PermissionPipeline` and are scored against `expected`/`expected_tier`; `auto` cases are skipped; exit 1 on any mismatch.
- **hook slot**: with a test-double decision source, `updated_input` is what the floor, rules and
  audit see; deny > ask > allow; an `allow` never passes the floor; error / timeout / malformed ⇒ no
  decision.
- **audit/telemetry consistency**: over the mode × risk-class matrix, each audit entry has exactly one
  matching `tool_decision` event and one headless `gate_decision` event (same ids; `tier` and `decision` equal as strings);
  the audit log is still written when telemetry is off or disabled by the project.
- **Tier 0.5 pre-classifier**: the golden adversarial set from `tasks/design/testing.md` Testing Strategy — the
  fail-through soundness property must hold for anything not fully parsed as one simple command; each
  allowlist-table row (listed flags pre-classify, the excluded flags fall through); in-scope `ls` → no
  prompt plus an audit record with `tier = tier05`; `ls ../` → one human prompt (interim; the
  counting-guardian form is owned by Phase 4); a relative path under an out-of-scope `cwd` falls
  through; `allow_extra` — an extra entry given a flag falls through to the guardian, an out-of-scope
  positional falls through, an in-scope positional pre-classifies.
- **headless** (`rust/fontaine/tests/pipeline_headless.rs`, scripted `openai-compatible` endpoint via the Phase-1 `common/mod.rs` helper, passing `--permission-mode manual` explicitly):
  every would-prompt outcome becomes a Deny returned as the tool_result, the run continues, exit
  code 0.
- **TUI** (`rust/fontaine-tui/tests/`): ratatui `TestBackend` with injected key events and a scripted
  `rig_core::test_utils` model — approval prompt → allow runs the tool (file written), deny returns
  the denial tool_result; Shift+Tab cycles manual → acceptEdits → plan → (auto once Phase 4 lands;
  wraps to manual until then).

## Definition of done

- Offline gates (no keys, no human): `cargo fmt --check` / `cargo clippy --all-targets -- -D warnings` /
  `cargo test` green, including the scripted-endpoint headless, pipeline, permission-rule, baseline, `none`-mode, and TUI tests above; `cargo run -p fontaine-evals -- --dataset ../evals/dataset --guardian none` (from `rust/`) exits 0, and the same step is added to `ci.yml` on all three OSes; interim default is
  `manual`; no mode or allow rule reaches past the deterministic floor; `docs/permissions.md` + pipeline diagram
  match the shipped step order and `rust/fontaine-core/tests/docs.rs` is green; CHANGELOG entry appended.
- Needs a human (the agent cannot push): push the branch and confirm the windows CI job runs the `#[cfg(windows)]` tests green.
- Needs a human and live keys (not CI): `tasks/design/testing.md` Manual Verification item 1 passes (TUI manual approval
  flow, end to end); the manual-mode rig-cassette recorded per `tasks/design/testing.md` Testing Strategy (Integration, cassettes).
- Agent step needing a logged-in `claude` CLI (manual or eval-side; never CI): the `claude -p` baseline capture, then `baseline diff` against a `--guardian none --out` run file, and triage of each divergence (fix or record).
- Log the phase in the master plan's Progress log.
