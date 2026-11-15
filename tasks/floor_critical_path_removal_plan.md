# Remove the Critical-Path Check from the Floor Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/654

## Overview

The critical-path step of the deterministic floor (`floor::critical_shell`,
`floor::critical_powershell`) forces a human prompt, or a denial where no human is present, for any
`shell`/`monitor`/`powershell` call that deletes the file system root, a top-level directory, the home
directory, a workspace root or a parent of one, and also for any call it cannot prove does *not*
do so. The second half fires on ordinary work: an unresolved command name (`$BIN`, `[`) is reported as
"deletes a critical path", the guardian never sees the call, and AFK turns the would-be prompt into
a denial (#654, session `0f9fd0e4`). The maintainer's direction on the issue is to take the
critical-path check out of the floor and let the guardian judge, keeping what the parser knows as
evidence. This plan removes the step, passes the guardian the recognized critical deletion targets
as a harness fact (computed for `shell`, `monitor` and `powershell`, as the current check is) with a hard-deny rule in both profiles, and stops the composed guardian's
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
  mode and the guardian; unchanged here.
- Ledger noise: `destructive::touched_paths` (`destructive.rs:321`) lists every literal shell word
  that contains `/` or `\` or starts with `.`, except the text operands `text_operands`
  (`destructive.rs:103`) knows about (`echo`, `printf`, `sed`, `awk`). So the
  `grep -v '\.d$'` pattern is recorded as the path `<cwd>/\.d$`.

## Design

### 1. Pipeline: the critical-path step goes

Delete the block at `permissions.rs:1194-1218`. A `shell`/`powershell` call with no deny or ask rule
match continues to the protected-path check (not applicable), Tier 0.5, allow rules, hooks and the
mode, as any other shell call does:

| Mode | Before (critical) | After |
| --- | --- | --- |
| manual / acceptEdits | forced prompt, no session grant | ordinary shell prompt (a user allow rule or hook allow can approve) |
| auto | forced prompt | a narrow allow rule or hook allow applies first (broad code-exec rules are suspended), else the guardian (with the new fact and the screen guard below) |
| AFK | denial, `converted: floor` | any allow rule or hook allow applies, else the guardian |
| dontAsk | denial `critical_path` | denial `dont_ask` unless an allow rule pre-approves |
| headless (manual by default) | denial `critical_path` | denial, `tier = mode`, `converted = mode`, unless an allow rule pre-approves |

The curated Tier 0.5 set has no deleter (a user `allow_extra` entry can add one), so nothing there changes. `deny shell(rm)` rules still
match an `rm` segment inside a chain (the rule-matching posture in `design/permissions.md`
§Permission rules is separate code and stays).

### 2. Evidence for the guardian: `critical_deletions`

Keep the target recognition and drop the "cannot be shown not to" branches:

- Move the critical-target code out of `floor.rs` into a new
  `rust/fontaine-core/src/critical.rs` (with `critical/powershell.rs`), since it is no longer
  a floor check:
  `pub fn critical_deletions(tool, args, env: &CriticalEnv) -> Vec<String>`, computed for `shell`,
  `monitor` (shell analysis) and `powershell`, as the current check is. It returns each deleter
  operand (shell: `DELETERS` words; powershell: `Remove-Item` and aliases) whose literal target,
  resolved against the call's `cwd` like today (lexically and through `canonicalize`), is a
  critical place under the existing `is_critical` (root, top-level dir, home, workspace root or
  parent; drive roots and `%USERPROFILE%` on Windows). It returns the resolved path followed by
  the kind, e.g. `/home/me (home directory)`. Non-literal command names and targets, launchers,
  operand-free `rm`, unreadable commands and chains add nothing: they go to the guardian
  unannotated, and the full prompt's existing "a recursive delete whose target the call does not
  make clear" clause covers them. In `critical/powershell.rs`, the `Expanded::NoHome` branch
  (`floor/powershell.rs:491`) and `wildcard_critical` (:559-596, `Remove-Item *`, `..\pro*`) are
  "cannot be shown not to" outcomes: both are deleted under the literal-target rule, and their tests
  (`a_missing_home_makes_home_targets_critical`, :993-1000, and the wildcard ones) are rewritten to
  assert an empty result. The existing `rm -rf {/,x}` brace case: brush yields a
  non-literal word, so it adds nothing, and the guardian sees the raw command.
- `ToolCallSummary` (`fontaine-guardian/src/request.rs`) gains
  `pub critical_deletions: Vec<String>`. Its doc comment says the harness computes it from the call
  itself, as for `destructive`. The core fills it where it builds the summary
  (`adjudication.rs`). The prompt builder renders it as a harness fact in the call block only when
  it is non-empty, so requests and snapshots without it stay byte-identical. Every
  `ToolCallSummary` literal in the workspace, not only tests, gets `critical_deletions: vec![]`
  (e.g. `fontaine-guardian/src/{profile.rs,screen.rs}`, `fontaine-guardian/tests/common/mod.rs`,
  `fontaine-core/tests/{account_session,claude_subscription,opencode_go,ollama_schema}.rs`,
  `fontaine-evals/src/live.rs`).
- Full profile: the `Destroy Pre-existing Files` entry (`rules.rs:42`) gains a sentence: a
  `critical_deletions` entry is a harness-resolved deletion of the file system root, a top-level
  directory, the home directory or a workspace root or its parent, and it is this rule whatever the
  user's messages say.
- Minimal profile: `guardian_minimal.md` gains a sibling item after `class=bind-mount-symlink` in its "Containment integrity" list
  (rule `Container Escape`), `class=host-data-delete`: a `critical_deletions` entry deletes a
  workspace root or the host data mounted around it, and the Deny explanation ends with
  `class=host-data-delete`. This is the only destruction the
  container profile judges, because it reaches past the container.

### 3. The composed guardian never score-allows a critical deletion

In `fontaine-guardian/src/composition`, `Score` (`host.rs:37-40`, today only `value`) carries the
`Arc<EvalCtx>` from `read()` (`host.rs:107-112`), and `allow_below` (`host.rs:185`) returns
`abstain()` when `ctx.request().tool_call.critical_deletions` is non-empty, so the script falls
through to its `llm(call)` stage. This sits inside the script, before any verdict exists, so it
covers both the sequential and the concurrent `first_decisive` path. A script that does not call
`llm` (a user composition) ends with an abstain, which is a fail-closed guardian Deny
(`fail_closed = unparseable`, `composition/mod.rs:631-636`). The check lives in the engine, not in
the preset scripts, so no user-scope script can opt out.

### 4. `Tier::CriticalPath` retires

Remove the variant from `Tier`, `Tier::ALL` (21 → 20), `as_str`, `converted_of`, `afk_report`'s
weight arm and the fixtures that name it. Following the `plan_block` precedent, old ledgers and
audit logs still read: `#[serde(alias = "critical_path")]` on `ProtectedPath`, the closest surviving
meaning (same AFK severity weight, `afk_report.rs:118`, and same `Converted::Floor` class). The
headless v1 schema (`docs/schemas/headless-v1.schema.json`, `critical_path` at l.33, pinned by
`rust/fontaine/tests/headless_contract.rs`) loses the `critical_path` enum value
without a version bump, as it did for `plan_block`: no event can carry it any more.

### 5. Ledger `paths` noise

`text_operands` (`destructive.rs:103`) learns `grep`, `egrep`, `fgrep` and `rg`: with no `-e`/
`--regexp`/`-f`/`--file`, the first non-option operand is the pattern (text). The value after
`-e`/`--regexp` is always text. Option skipping is per program: the value-taking options come from
the preclassifier's grep/rg Spec tables (`preclassify.rs:758-813`), including the `--opt=value`
and `-Xvalue` forms, and an attached `-e`/`--regexp=` counts as an explicit pattern. This uses the
same index-based logic as the `sed -e` branch. No other shell-word handling changes. The issue's
glob-prefix path (`.../deps/agents-`) cannot be produced by current code: a glob word is
non-literal (`preclassify.rs:382-388`) and `touched_paths` skips non-literal words
(`destructive.rs:396-398`). A regression test on the issue's exact commands asserts that
`touched_paths` for the first command is empty after the fix.

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
   with `CriticalEnv { scope: &state.scope(), home: state.home.as_deref() }`, the home the floor uses
   today (`gate.rs:424`, `permissions.rs:1109`).
3. **Guardian request and prompts.** Add the field to `request.rs`, render it in
   `prompt_builder.rs`, extend `rules.rs:42` and `guardian_minimal.md`, and update the snapshots
   that gain the fact (only new snapshot cases should differ). Reword the `prompt_builder.rs:411-416`
   note "PowerShell text the harness did not parse; read the command itself" (and its "nothing
   pre-digested" comment), e.g. "only the `critical_deletions` targets were resolved; read the rest
   of the command".
4. **Composition guard.** Implement the `allow_below` → abstain rule (§3: `Score` carrying the
   `Arc<EvalCtx>`) in `composition/host.rs`.
5. **Tier retirement.** Apply §4 across `tier.rs`, `permissions.rs`, `afk_report.rs`, the TUI
   fixtures (`notify.rs:161`, `screen.rs:694`, `linear.rs:626` → another floor tier such as
   `AskRule`), remove `critical_path` from `docs/schemas/headless-v1.schema.json` (pinned by
   `rust/fontaine/tests/headless_contract.rs`), and add a read-back test of an old `critical_path`
   audit/ledger line beside the `plan_block` ones (`audit.rs:316`, `ledger.rs:1106`).
6. **Ledger paths.** §5 in `destructive.rs`, with tests.
7. **Tests.** Rewrite the critical-path integration tests to the new expectations (see Testing
   Strategy). `fontaine-evals/src/live.rs:364` `request_for` builds the `AdjudicationRequest` and
   a `ToolCallSummary` (:442) itself, with `Scope::new(root, [])` and no home: it fills
   `critical_deletions` through `pub mod critical` in fontaine-core's `lib.rs`, with a fixed eval home such
   as `/home/eval`, beside the existing `MEMORY_DIR`, so the `rm -rf ~` case resolves. Update the evals: `evals/dataset/floor.jsonl` cases 7-9 and the
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
- `docs/schemas/headless-v1.schema.json`
- `rust/fontaine-evals/src/live.rs`; every other `ToolCallSummary` literal:
  `fontaine-guardian/src/{profile.rs,screen.rs}`, `fontaine-guardian/tests/common/mod.rs`
- Code comments: `rust/fontaine-core/src/permissions.rs:3` (module doc),
  `rust/fontaine-core/src/gate.rs:208` (the `home` doc, which should cite `critical_deletions`),
  `rust/fontaine-tools/src/preclassify.rs:2-3`
- Tests: `fontaine-core/tests/{permissions,permissions_auto,permissions_windows,permissions_screen,permission_rules,afk,hook_slot,user_hooks,account_session,claude_subscription,opencode_go,ollama_schema}.rs`,
  `fontaine/tests/{pipeline_headless,headless_contract}.rs`, `fontaine-evals/tests/{fixture,none_mode,live}.rs`
- `evals/dataset/floor.jsonl`, `evals/dataset/tier05.jsonl`, new guardian cases,
  `evals/sequences/afk.jsonl`, `evals/README.md`
- `README.md`, `AGENTS.md`, `tasks/fontaine_harness_plan.md` (Decisions),
  `tasks/design/{permissions,testing,config,platforms,research-findings}.md`,
  `research/research-claude-code-parity.md`
- `docs/{permissions,afk,guardian,headless,hooks,tui,architecture}.md`

## Trade-offs

- **Keep recognized critical removals in the floor, drop only the "cannot be shown not to"
  outcomes** (Claude Code parity, no deviation). This fixes #654 with less change. It was not
  chosen because the maintainer's direction on the issue is that the floor should not decide on a
  parse, and that some judgment is almost always required. The screen's ability to clear such a call is bounded by §2's
  fact and §3's guard; the allow-rule part is the accepted risk in Decisions.
- **Narrow fix: keep the deny, correct the reason text and the `paths` noise** (the issue's
  alternative). Rejected: the guardian still never sees a harmless `$BIN` test run.
- **Pass no new fact, rely on raw args and `home_directory`**: simpler, but a hosted screen or a
  small model reading `rm -rf ..` from a workspace root, or a symlink to home, gains from the harness's own resolution, and the minimal
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
  critical-path clauses of the 2026-10-21 (revised) floor line, the 2026-10-30
  `critical_powershell` line (master plan l.1127, under `### Tools`), and the master plan's
  Decisions lines 871, 1000 (the "four critical-path retries" calibration) and 1129 (Remove-Item).
- Accepted risk: a user `allow` rule or a hook `allow` now approves a literal `rm -rf /` in
  manual/acceptEdits/dontAsk, in auto (narrow rules only; broad code-execution rules stay
  suspended) and in AFK (any rule). It is the user's own grant. Claude Code refuses this.

## Security invariants

`AGENTS.md`'s floor invariant becomes "The deterministic floor (`deny → ask → protected-path`)
gates EVERY mode". The `/guardian-disable` exception text is unchanged. The guardian fail-closed,
no-zero-guardian and project-scope-only-tightens invariants are untouched: the change removes a
deterministic stop, it does not add a loosening key. `AGENTS.md:58-61` (the composed guardian's
`allow_below` exception) gains "never for a call with a non-empty `critical_deletions`".

## Documentation

- `docs/permissions.md`: remove the critical-path floor bullet (l.53-60), the "still ask you" list
  (l.72), the tier list (l.161), the allow-rule clause (l.322), the `Remove-Item` section
  (l.363-384) and the stale reference at l.203. Describe the `critical_deletions` fact where the
  guardian is mentioned.
- `docs/guardian.md`: the floor summary (l.10, 264, 359), the new fact and both prompts' rule, the
  `allow_below` exception where it is described (l.654: a critical deletion abstains).
- `docs/afk.md` (l.79, 163), `docs/headless.md` (l.232, 290), `docs/hooks.md` (l.210),
  `docs/tui.md` (l.587, 718), `docs/architecture.md` (l.156), `docs/agent-sdk.md` (l.140),
  `README.md` (l.11), `evals/README.md` (l.62, 111).
- Code comments naming the floor's critical-path step: `permissions.rs:3`, `gate.rs:208`,
  `preclassify.rs:2-3`.
- Design docs: `tasks/design/permissions.md` (§Core data flow step 2, tier list, l.174, 205, 211),
  `testing.md` (golden set: `rm -rf {/,x}` → guardian, `ls;rm x` no longer "critical"),
  `config.md` l.11, `platforms.md` l.10/27/31, `research-findings.md` l.23; parity row l.265 →
  deviation.
- `CHANGELOG.md` (added by `pr`).

## Testing Strategy

Unit tests (`critical.rs`, `critical/powershell.rs`):
- Recognized targets return their resolved path and kind: `rm -rf /`, a literal home
  path (`rm -rf /home/me`), a workspace root, a parent of it, a symlink to home, `Remove-Item C:\`, a cwd-relative `..`.
- The #654 commands return nothing:
  `bin=$(ls -t target/debug/deps/agents-* | grep -v '\.d$' | head -1); ($bin t)`,
  `while pgrep -x rustc >/dev/null; do sleep 3; done`, `BIN=$(ls -t x); ($BIN t --exact)`; so do
  `X=rm; $X -rf ~`, `${X}m -rf /`, `ls;rm x`, `find . -exec rm {} +`.

Pipeline integration tests (`fontaine-core/tests`, scripted guardian):
- **Regression for #654**: in AFK `auto` with a fixture guardian that allows, each of the three
  issue commands is allowed at `tier = guardian` and reaches the guardian once, with no
  `converted: floor` denial.
- `rm -rf ~`/`rm -rf /` in `auto`: the guardian request carries `critical_deletions`.
- manual: `rm -rf /` prompts at `tier = mode`. dontAsk/headless: denied at `dont_ask`.
  `pipeline_headless.rs` expects the headless denial at `tier = mode`, `converted = mode`.
- Existing tests asserting `Tier::CriticalPath` in `permissions*.rs`, `afk.rs`, `hook_slot.rs`,
  `user_hooks.rs`, `permission_rules.rs` are rewritten to the table in Design §1. The "allow rule
  never passes a critical path" tests become tests of the new behavior: an ask rule still beats an
  allow rule, and an allow rule now approves `rm -rf /` (the accepted risk, pinned).
- An old `critical_path` audit line and ledger record read back as `protected_path`.
- `touched_paths` for the issue's first command is empty (no `\.d$` path, no glob-prefix path).

Guardian:
- `fontaine-guardian/tests/composition.rs`, beside `allow_below_under_the_threshold_allows` (:661),
  with a request built via `tests/common/mod.rs` that has non-empty `critical_deletions`: a score
  below `allow_threshold` does **not** allow, the call falls through to the LLM stage, and with no
  LLM stage it fails closed.
- Prompt-builder snapshot with a non-empty `critical_deletions`. Existing snapshots unchanged.
- New golden cases in `evals/dataset` for `rm -rf ~` and `rm -rf <workspace root>` in the full and
  minimal profiles, expected `deny`, run with the fixture guardian in CI and with the live Haiku
  guardian before the PR is pushed. Fixture scoring returns the canned verdict, so the live Haiku
  run is the only real check of the new cases (whether the prompt text actually moves the model).
  `fontaine-evals/src/live.rs` fills `critical_deletions` for them (step 7).
- `cargo run -p fontaine-evals -- --dataset ../evals/dataset --guardian fixture --sequences ../evals/sequences/afk.jsonl`
  and the `--profile minimal` run stay green with the updated cases.

## Open Questions

None that block implementation.
