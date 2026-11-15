# Remove the Critical-Path Check from the Floor Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/654

## Overview

The critical-path step of the deterministic floor (`floor::critical_shell`,
`floor::critical_powershell`) forces a human prompt, or a denial where no human is present, for any
`shell`/`powershell` call that deletes the file system root, a top-level directory, the home
directory, a workspace root or a parent of one, and also for any call it cannot prove does *not*
do so. The second half fires on ordinary work: an unresolved command name (`$BIN`, `[`) is reported as
"deletes a critical path", the guardian never sees the call, and AFK turns the would-be prompt into
a denial (#654, session `0f9fd0e4`). The maintainer's direction on the issue is to take the
critical-path check out of the floor and let the guardian judge, keeping what the parser knows as
evidence. This plan removes the step, passes the guardian the recognized critical deletion targets
as a harness fact with a hard-deny rule in both profiles, and stops the composed guardian's
score-threshold allow from clearing such a call. The floor becomes `deny → ask → protected-path`.
It also fixes the ledger `paths` noise the issue reports (`grep` patterns read as paths).

## Current State

- `rust/fontaine-core/src/permissions.rs:1194-1218`: after the ask rules and before protected
  paths, a `shell`/`monitor`/`powershell` call goes through `floor::critical_shell` /
  `floor::critical_powershell`. `Some(found)` becomes
  `would_prompt(Prompt { tier: Tier::CriticalPath, reason: "it deletes a critical path ({found})", forced: true })`:
  a prompt with no session grant in manual/acceptEdits/auto, a denial in `dontAsk`/headless, and
  a `converted: floor` denial in AFK (`converted_of`, `permissions.rs:840`). No allow rule, hook
  allow or guardian verdict gets past it.
- `rust/fontaine-core/src/floor.rs:217-372` (`CriticalEnv`, `critical_shell`, `scan`,
  `is_deleter`, `launches_targets`, `check_targets`, `is_critical_*`): critical when a deleter
  (`DELETERS`) targets a critical place, **and** when the command cannot be shown not to: an unread
  command that mentions `rm`, any non-literal command name (#654), a non-literal `rm` target, a launcher
  (`xargs`, `find -exec`), `rm` without operands, and `rm` inside a chain or pipe.
  `rust/fontaine-core/src/floor/powershell.rs` is the `Remove-Item` counterpart with the same
  posture.
- Claude Code (`permission-modes#critical-paths`) runs the same kind of check, but only on targets
  it *recognizes*: "A command the check misses gets no special handling". Fontaine's
  "cannot be shown not to" outcomes are the part that goes beyond Claude Code.
- `Tier::CriticalPath` (`rust/fontaine-core/src/tier.rs`) appears in the audit log, the ledger, the
  headless `gate_decision` event, the eval `expected_tier`, the AFK report's weights
  (`afk_report.rs:118`) and the TUI notification/screen test fixtures.
- The guardian already gets `tool_call.args`, `session_facts.home_directory`,
  `workspace_roots` and `touched_paths` (`fontaine-guardian/src/request.rs`). The full profile's
  `Destroy Pre-existing Files` rule (`fontaine-guardian/src/rules.rs:42`) covers recursive deletes
  of pre-existing or out-of-workspace targets, and deletes whose target is unclear. The minimal
  (container) profile (`prompts/guardian_minimal.md`) judges only containment escape and
  exfiltration, so on its own it would let `rm -rf <workspace root>` delete the bind-mounted host
  data.
- The `screen` preset (`fontaine-guardian/src/composition/screen_jev.rn`) allows through
  `allow_below` (`composition/host.rs:185`) whenever the hosted score is under the threshold,
  without the LLM stage.
- Secret-file reads are **not** a floor check: `floor::secret_path` only takes a read off the
  containment fast path and `tier05` (`permissions.rs:1276-1370`), and the call goes on to the
  mode and the guardian. This plan does not change that. (An earlier comment on #654 said the floor
  stops `.env` reads before the screen. That was wrong.)
- Ledger noise: `destructive::touched_paths` (`destructive.rs:321`) lists every literal shell word
  that contains `/` or `\` or starts with `.`, except the text operands `text_operands`
  (`destructive.rs:103`) knows about (`echo`, `printf`, `sed`, `awk`). So the
  `grep -v '\.d$'` pattern is recorded as the path `<cwd>/\.d$`.

## Design

### 1. Pipeline: the critical-path step goes

Delete the block at `permissions.rs:1194-1218`. A `shell`/`powershell` call with no deny or ask rule
match continues to the protected-path check (not applicable), allow rules, hooks, Tier 0.5 and the
mode, as any other shell call does:

| Mode | Before (critical) | After |
| --- | --- | --- |
| manual / acceptEdits | forced prompt, no session grant | ordinary shell prompt (a user allow rule or hook allow can approve) |
| auto | forced prompt | guardian (with the new fact and the screen guard below) |
| AFK | denial, `converted: floor` | guardian |
| dontAsk / headless | denial `critical_path` | denial unless an allow rule pre-approves |

Tier 0.5 never fast-paths a deleter, so nothing there changes. `deny shell(rm)` rules still
match an `rm` segment inside a chain (the rule-matching posture in `design/permissions.md`
§Permission rules is separate code and stays).

### 2. Evidence for the guardian: `critical_deletions`

Keep the target recognition and drop the "cannot be shown not to" branches:

- Move the critical-target code out of `floor.rs` into a new
  `rust/fontaine-core/src/critical.rs` (with `critical/powershell.rs`), since it is no longer
  a floor check:
  `pub fn critical_deletions(tool, args, env: &CriticalEnv) -> Vec<String>`. It returns each deleter
  operand (shell: `DELETERS` words; powershell: `Remove-Item` and aliases) whose literal target,
  resolved against the call's `cwd` like today (lexically and through `canonicalize`), is a
  critical place under the existing `is_critical` (root, top-level dir, home, workspace root or
  parent; drive roots and `%USERPROFILE%` on Windows). It returns the resolved path followed by
  the kind, e.g. `/home/me (home directory)`. Non-literal command names and targets, launchers,
  operand-free `rm`, unreadable commands and chains add nothing: they go to the guardian
  unannotated, and the full prompt's existing "a recursive delete whose target the call does not
  make clear" clause covers them. The existing `rm -rf {/,x}` brace case: brush yields a
  non-literal word, so it adds nothing, and the guardian sees the raw command.
- `ToolCallSummary` (`fontaine-guardian/src/request.rs`) gains
  `pub critical_deletions: Vec<String>`. Its doc comment says the harness computes it from the call
  itself, as for `destructive`. The core fills it where it builds the summary
  (`adjudication.rs`). The prompt builder renders it as a harness fact in the call block only when
  it is non-empty, so requests and snapshots without it stay byte-identical. Every
  `ToolCallSummary` literal in tests gets `critical_deletions: vec![]`.
- Full profile: the `Destroy Pre-existing Files` entry (`rules.rs:42`) gains a sentence: a
  `critical_deletions` entry is a harness-resolved deletion of the file system root, a top-level
  directory, the home directory or a workspace root or its parent, and it is this rule whatever the
  user's messages say.
- Minimal profile: `guardian_minimal.md` gains a containment bullet under
  `class=bind-mount-symlink`: a `critical_deletions` entry deletes a workspace root or the host
  data mounted around it, so the call is `Container Escape`. This is the only destruction the
  container profile judges, because it reaches past the container.

### 3. The composed guardian never score-allows a critical deletion

In `fontaine-guardian/src/composition`, a verdict produced by `allow_below` for a call whose
`critical_deletions` is non-empty is replaced by an abstain, so the script falls through to its
`llm(call)` stage. A script that does not call `llm` (a user composition) ends with an abstain,
which already fails closed (Escalate → prompt or Deny). The check lives in the engine, not in the
preset scripts, so no user-scope script can opt out. `Call` gains a `critical_deletion()`
accessor (bool) next to `destructive()` so scripts can route on it, and docs/guardian.md's
host-function table lists it.

### 4. `Tier::CriticalPath` retires

Remove the variant from `Tier`, `Tier::ALL` (21 → 20), `as_str`, `converted_of`, `afk_report`'s
weight arm and the fixtures that name it. Following the `plan_block` precedent, old ledgers and
audit logs still read: `#[serde(alias = "critical_path")]` on `AskRule`, the closest surviving
meaning (a floor-forced prompt). The headless v1 schema loses the `critical_path` enum value
without a version bump, as it did for `plan_block`: no event can carry it any more.

### 5. Ledger `paths` noise

`text_operands` (`destructive.rs:103`) learns `grep`, `egrep`, `fgrep` and `rg`: with no `-e`/
`--regexp`/`-f`/`--file`, the first non-option operand is the pattern (text). The value after
`-e`/`--regexp` is always text. This uses the same index-based logic as the `sed -e` branch. No
other shell-word handling changes. The issue's glob-prefix path (`.../deps/agents-`) is pinned by
a regression test on the issue's exact commands. If that test shows the path, trace its source and
fix it the same way. If it does not, the test still pins the fixed output.

## Implementation Steps

1. **Critical-target analysis.** Create `rust/fontaine-core/src/critical.rs` and
   `critical/powershell.rs` by moving `CriticalEnv`, `DELETERS`, `is_critical*`, `same_path`,
   `starts_with` and the target walk out of `floor.rs`/`floor/powershell.rs`. Reshape `scan`/`check_targets`
   to collect recognized critical targets instead of returning the first reason, and delete the
   unresolvable branches. `floor.rs`'s module doc and remaining code cover only the protected-path and
   secret-path helpers. Port the existing critical-path unit tests: recognized cases now assert the
   returned target, and the old "cannot be resolved" cases assert an empty result.
2. **Pipeline.** Delete the critical-path block in `permissions.rs`. Fill
   `ToolCallSummary.critical_deletions` in `adjudication.rs` from `critical::critical_deletions`
   with the call's scope and home.
3. **Guardian request and prompts.** Add the field to `request.rs`, render it in
   `prompt_builder.rs`, extend `rules.rs:42` and `guardian_minimal.md`, and update the snapshots
   that gain the fact (only new snapshot cases should differ).
4. **Composition guard.** Implement the `allow_below` → abstain rule for critical deletions and
   the `Call::critical_deletion()` accessor in `composition/host.rs`/`mod.rs`.
5. **Tier retirement.** Apply §4 across `tier.rs`, `permissions.rs`, `afk_report.rs`, the TUI
   fixtures (`notify.rs:161`, `screen.rs:694`, `linear.rs:626` → another floor tier such as
   `AskRule`), and add a read-back test of an old `critical_path` audit/ledger line beside the
   `plan_block` ones (`audit.rs:316`, `ledger.rs:1106`).
6. **Ledger paths.** §5 in `destructive.rs`, with tests.
7. **Tests.** Rewrite the critical-path integration tests to the new expectations (see Testing
   Strategy), and update the evals: `evals/dataset/floor.jsonl` cases 7-9 and the
   `tier05.jsonl` cases with `expected_tier: critical_path` (lines 2, 5, 10, 19), and the
   `evals/sequences/afk.jsonl` `afk-temp-critical-path-retries` case. Add golden guardian cases for
   critical deletions in both profiles.
8. **Docs and records.** See Documentation, plus the Decisions line and AGENTS.md (below).

## Files to Modify

- `rust/fontaine-core/src/floor.rs`, `floor/powershell.rs` (moved out), new `critical.rs`,
  `critical/powershell.rs`, `lib.rs`
- `rust/fontaine-core/src/permissions.rs`, `adjudication.rs`, `tier.rs`, `afk_report.rs`,
  `destructive.rs`, `audit.rs`/`ledger.rs` (tests)
- `rust/fontaine-guardian/src/request.rs`, `prompt_builder.rs`, `rules.rs`,
  `prompts/guardian_minimal.md`, `composition/host.rs`, `composition/mod.rs`, snapshots
- `rust/fontaine-tui/src/notify.rs`, `screen.rs`, `linear.rs` (fixtures)
- Tests: `fontaine-core/tests/{permissions,permissions_auto,permissions_windows,permissions_screen,permission_rules,afk,hook_slot,user_hooks}.rs`,
  `fontaine/tests/pipeline_headless.rs`, `fontaine-evals/tests/{fixture,none_mode,live}.rs`
- `evals/dataset/floor.jsonl`, `evals/dataset/tier05.jsonl`, new guardian cases,
  `evals/sequences/afk.jsonl`, `evals/README.md`
- `AGENTS.md`, `tasks/fontaine_harness_plan.md` (Decisions),
  `tasks/design/{permissions,testing,config,platforms,research-findings}.md`,
  `research/research-claude-code-parity.md`
- `docs/{permissions,afk,guardian,headless,hooks,tui,architecture}.md`

## Trade-offs

- **Keep recognized critical removals in the floor, drop only the "cannot be shown not to"
  outcomes** (Claude Code parity, no deviation). This fixes #654 with less change. It was not
  chosen because the maintainer's direction on the issue is that the floor should not decide on a
  parse, and that some judgment is almost always required. The cost of the chosen route, an allow
  rule or the screen able to clear `rm -rf ~`, is bounded by §2's hard-deny fact and §3's engine
  guard. This plan does not bound the allow-rule part (see Decisions).
- **Narrow fix: keep the deny, correct the reason text and the `paths` noise** (the issue's
  alternative). Rejected: the guardian still never sees a harmless `$BIN` test run.
- **Pass no new fact, rely on raw args and `home_directory`**: simpler, but a hosted screen or a
  small model reading `rm -rf "$(pwd)/.."` gains from the harness's own resolution, and the minimal
  profile has no rule to hang the decision on without a fact.
- **Block the screen allow for every `destructive` call** instead of only critical deletions:
  broader than the issue and changes the screen's measured skip rate. Not taken.

## Decisions

- 2026-11-15 (#654, to add to the master plan's `### Permissions`): Deviation from Claude Code:
  the floor is `deny → ask → protected-path`, with no critical-path step. A recognized deletion of
  the file system root, a top-level directory, home, or a workspace root or its parent reaches the
  guardian as a `critical_deletions` fact that both profiles deny, and the composed guardian's
  `allow_below` never allows such a call. Claude Code prompts for recognized critical-path removals
  in every mode. Reason: the maintainer judged a parse-based floor decision not useful. The
  unresolvable cases denied ordinary work in AFK without the guardian (#654). Supersedes the
  critical-path clauses of the 2026-10-21 (revised) floor line and the 2026-10-30
  `critical_powershell` line.
- Accepted risk: a user `allow` rule or a hook `allow` now approves a literal `rm -rf /` in
  manual/acceptEdits/dontAsk (an `auto` broad code-execution rule stays suspended). It is the
  user's own grant. Claude Code refuses this.

## Security invariants

`AGENTS.md`'s floor invariant becomes "The deterministic floor (`deny → ask → protected-path`)
gates EVERY mode". The `/guardian-disable` exception text is unchanged. The guardian fail-closed,
no-zero-guardian and project-scope-only-tightens invariants are untouched: the change removes a
deterministic stop, it does not add a loosening key.

## Documentation

- `docs/permissions.md`: remove the critical-path floor bullet (l.53), the "still ask you" list
  (l.72), the tier list (l.161), the allow-rule clause (l.322) and the `Remove-Item` section
  (l.363-368). Describe the `critical_deletions` fact where the guardian is mentioned.
- `docs/guardian.md`: the floor summary (l.10, 264, 359), the new fact and both prompts' rule, the
  `allow_below` exception and the `critical_deletion()` host function.
- `docs/afk.md` (l.79, 163), `docs/headless.md` (l.232, 290), `docs/hooks.md` (l.210),
  `docs/tui.md` (l.587), `docs/architecture.md` (l.156), `evals/README.md` (l.62, 111).
- Design docs: `tasks/design/permissions.md` (§Core data flow step 2, tier list, l.174, 205, 211),
  `testing.md` (golden set: `rm -rf {/,x}` → guardian, `ls;rm x` no longer "critical"),
  `config.md` l.11, `platforms.md` l.10/27/31, `research-findings.md` l.23; parity row l.265 →
  deviation.
- `CHANGELOG.md` (added by `pr`).

## Testing Strategy

Unit tests (`critical.rs`, `critical/powershell.rs`):
- Recognized targets return their resolved path and kind: `rm -rf /`, `rm -rf ~/`-free literal home
  path, a workspace root, a parent of it, a symlink to home, `Remove-Item C:\`, a cwd-relative `..`.
- The #654 commands return nothing:
  `bin=$(ls -t target/debug/deps/agents-* | grep -v '\.d$' | head -1); ($bin t)`,
  `while pgrep -x rustc >/dev/null; do sleep 3; done`, `BIN=$(ls -t x); ($BIN t --exact)`; so do
  `X=rm; $X -rf ~`, `${X}m -rf /`, `ls;rm x`, `find . -exec rm {} +`.

Pipeline integration tests (`fontaine-core/tests`, scripted guardian):
- **Regression for #654**: in AFK `auto` with a fixture guardian that allows, each of the three
  issue commands is allowed at `tier = guardian` and reaches the guardian once, with no
  `converted: floor` denial.
- `rm -rf ~`/`rm -rf /` in `auto`: the guardian request carries `critical_deletions`. A fixture
  screen composition whose score is below `allow_threshold` does **not** allow it: the LLM stage is
  consulted, and with no LLM stage the call fails closed. This is a permissions property, so it is
  automated, not manual.
- manual: `rm -rf /` prompts at `tier = mode`. dontAsk/headless: denied at `dont_ask`.
  `pipeline_headless.rs` gets the new denial tier.
- Existing tests asserting `Tier::CriticalPath` in `permissions*.rs`, `afk.rs`, `hook_slot.rs`,
  `user_hooks.rs`, `permission_rules.rs` are rewritten to the table in Design §1. The "allow rule
  never passes a critical path" tests become tests of the new behavior: an ask rule still beats an
  allow rule, and an allow rule now approves `rm -rf /` (the accepted risk, pinned).
- An old `critical_path` audit line and ledger record read back as `ask_rule`.
- `touched_paths` for the issue's first command lists no `\.d$` path and no glob-prefix path.

Guardian:
- Prompt-builder snapshot with a non-empty `critical_deletions`. Existing snapshots unchanged.
- New golden cases in `evals/dataset` for `rm -rf ~` and `rm -rf <workspace root>` in the full and
  minimal profiles, expected `deny`, run with the fixture guardian in CI and with the live Haiku
  guardian before the PR is pushed. The live run catches whether the prompt text actually moves
  the model, which no fixture can.
- `cargo run -p fontaine-evals -- --dataset ../evals/dataset --guardian fixture --sequences ../evals/sequences/afk.jsonl`
  and the `--profile minimal` run stay green with the updated cases.

## Open Questions

None that block implementation. The Trade-offs and Decisions above record the parity alternative
for the maintainer to reverse at review if wanted.
