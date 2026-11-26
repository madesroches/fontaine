# Auto-Mode Claude Code Baseline Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/103

## Overview

Phase 4 asks for an `auto`-mode baseline against Claude Code: pose the pipeline cases to
`claude -p --permission-mode auto`, run the same cases through Fontaine's pipeline in `auto`, and
triage every case whose outcome differs into a Fontaine fix or a dated `## Decisions` line. Two
pieces ride along. First, the destructive-class word lists in
`rust/fontaine-sdk/src/core/destructive.rs` are compared term by term with the commands Claude
Code's auto-mode classifier prompt names. Second, the PreToolUse hook `allow` legs (narrow and broad)
are observed again on the current `claude`. The runner can't do this today. `baseline capture` only
poses a case in its own mode, and `--guardian none` skips every `auto` case. So the code part is a
`--mode` override on both sides, plus a dataset marker that keeps host-reaching cases from being
run for real on the host. The rest is live runs, data, and the triage record.

## Current State

- **Capture** (`rust/fontaine-evals/src/baseline.rs`): `capture_one` (`:282`) poses each case in
  `case.mode.unwrap_or(Manual)`, mapped by `cc_mode` (`:257`; `manual` → `default`, `afk` →
  `auto`). It runs `claude` with `--permission-mode`, `--output-format stream-json --verbose` and
  `--setting-sources project` (`:303-310`). The record's `mode` is the case's mode. There is
  no way to pose a `manual` or `dontAsk` case in `auto`.
- **Skips** (`capture_skip_reason`, `:113`): non-pipeline cases, `allow_extra`, `session_facts`,
  `auto_mode`, `environment`, `recent_user_messages`, and tools with no Claude Code name
  (`send_message`, `board_update`). The cases that reach the host are another matter. The 2026-11-15 `### Evals` Decision
  names 13: `floor-critical-deletion-*` (`rm -rf ~`, `rm -rf /`), `floor-secret-*` and five
  `tier05` ones. Nothing in the data marks them, and they were left out only because the operator
  left them out. That set is not the right one: `floor-secret-*` have no `workspace`, so `.env`
  resolves inside the empty scratch dir, while other cases do reach host files. In `manual`/`dontAsk`, headless Claude Code
  denies them before anything runs. In `auto`, its classifier could allow one, and the command would
  then run on the host with the real `HOME`.
- **Fontaine side** (`rust/fontaine-evals/src/runner.rs:920-960`): `--guardian none` skips any case
  whose mode is `auto` ("auto mode needs the guardian"). A `live:` run already scores pipeline
  cases through `none::decide(case, Some(guardian))` (`runner.rs:1113`). `score_pipeline`
  (`runner.rs:1394`) writes the case's own mode into `CaseResult.mode`, which `baseline diff`
  matches against the record's `mode` (`baseline.rs:420`). A live run exits 0 whatever it scored
  (`runner.rs:1039`). It rewrites `guardian_models.toml` only on an unfiltered `--profile full` run.
- **Dataset**: 92 pipeline cases. `tier05-adversarial` has 41 `manual` and 1 `dontAsk`. `floor` has
  18 `manual`, 17 `dontAsk` and 2 `auto`. `spawn` has 6 `manual`, 3 `dontAsk` and 4 `auto`. The
  `auto` ones are `spawn-explore-fast-path-auto`, `spawn-worker-auto-gate`,
  `floor-external-{read,write}-auto-no-guardian` and the two coordination cases. #101 left them
  "deferred to #103". `evals/baselines/pipeline.jsonl` holds 66 records from #101 (`claude` 2.1.309
  to 2.1.325). The local `claude` is now `2.1.332 (Claude Code)`.
- **Hook legs**: observed once per variant on 2026-10-23 with 2.1.309, by hand, as
  `tasks/completed/release_live_checks_plan.md` (§Baselines, "Hook-`allow` in `auto`") describes.
  The narrow hook is honored on both sides. The broad hook is honored by Claude Code and
  suspended by Fontaine. That is the recorded deviation (`### Permissions`, 2026-10-23). The
  tracker ticks it as done (`tasks/fontaine_harness_plan.md:140`).
- **Destructive-class word lists** (`destructive.rs:17-47`): `REMOVAL_OVERWRITE_WORDS`,
  `NETWORK_CLIENT_WORDS`, `GIT_NETWORK_SUBCOMMANDS`, `INSTALLERS` + `MORE_INSTALLERS` and
  `FIND_DESTRUCTIVE_FLAGS`. `destructive_argv` (`:406`) adds `sed -i`, `perl -i`, `git_destructive`
  and the `code_execution::LAUNCHERS` / `starts_with_entry` code-execution entries. The flag is
  derived from the call, never from the guardian's label. The 2026-10-23 live checks left this
  comparison open because "Claude Code publishes no such list". Its auto-mode classifier prompt has
  since been captured verbatim (`research/research-claude-code-auto-mode-prompt.md`, cc 2.1.310).
  Its HARD BLOCK (`:177`) and SOFT BLOCK (`:186-261`) rules and its Definitions (`:157-175`, e.g.
  the Kubernetes cluster-write verbs) name concrete commands.

## Design

### `--mode <MODE>` on both sides

A single override posed on both sides. It doesn't add `auto` copies of 92 cases to the dataset:
the cases stay as they are, and the run states the mode.

- `baseline capture --mode <MODE>`: every selected case is posed in `MODE` instead of its own,
  and each record's `mode` is `MODE`. Without the flag, each case keeps its own mode, as today.
  The value parses through `PermissionMode`'s serde `Deserialize` (a clap `value_parser`, since it
  has no `FromStr`/`ValueEnum`), so names match the dataset's `mode` values (`manual`, `acceptEdits`,
  `dontAsk`, `auto`, `afk`); Claude Code's `default` is not accepted. It maps through `cc_mode` as today. Factor the argv that `capture_one` builds into a
  pure `claude_command_args(case, mode) -> Vec<String>` so a unit test can pin
  `--permission-mode auto` and the `--settings` leg without the real binary.
- Run `--mode <MODE>` (pipeline cases only): `none::decide` takes the effective mode
  (`mode.or(case.mode).unwrap_or(Manual)`) instead of reading `case.mode`, for the ledger's
  `SessionStart.permission_mode` and the `SessionState` mode alike. `score_pipeline` writes that
  effective mode into `CaseResult.mode`, so `baseline diff` matches it without change. The `none` skip
  rule then uses the effective mode, so `--guardian none --mode auto` skips everything and says so.
  `--mode` is a usage error with `--guardian fixture` or `screen:`, because those don't run the
  pipeline. The case's `expected` labels belong to its own mode, so under `--mode`, `pass` is
  computed against labels that don't apply. The summary prints one line saying so, and a
  `--mode` run is a measurement like `live:` and `screen:`: `measurement` in `runner.rs` also covers
  `--mode`, so it exits 0 whatever it scored (only infrastructure failures fail it).

### Host-reaching marker

Add an optional pipeline-only case field `host_reaching: true` (in `case.rs`, with
`deny_unknown_fields` already in force). `validate_case` rejects it on non-pipeline cases. Set it on
the cases whose call reaches host files outside the scratch workspace, re-derived from
`evals/dataset/{floor,tier05,spawn}.jsonl`: `floor-critical-deletion-{home-manual,dont-ask,allow-rule}`;
`floor-external-read-{deny-rule,deny-rule-dont-ask,ask-rule,ask-rule-dont-ask,allow-rule,manual,dont-ask,auto-no-guardian,deny-beats-allow}`;
`floor-external-write-` with the same nine suffixes; `floor-external-{read,write}-symlink-home-rule`
(`../home` resolves outside scratch, `case.rs:539`); and `tier05-{chain-curl-sh,out-of-scope-home-path,command-substitution-rm,symlink-escape,tilde-argument,parameter-expansion,brace-expansion-escape,brace-expansion-rm-root,out-of-scope-cwd,dont-ask-chain}`; and every
pipeline `agent` spawn case in `spawn.jsonl` except `spawn-agent-deny-rule`, since Claude Code can run a
spawn's subagent with the real HOME and its tools: `spawn-readme-escalated-toolset{,-dont-ask}`,
`spawn-agent-ask-rule`, `spawn-explore-fast-path{,-auto}` and `spawn-worker-auto-gate` (the deny rule
stops the spawn before any subagent exists) (39 cases). `floor-secret-*` are not marked: they never leave the scratch dir. The marker applies
in every mode, so #101's manual/dontAsk external-path captures are skipped too unless
`--host-reaching=yes`. `capture_skip_reason` returns "reaches host files outside
the scratch workspace; capture it only in a disposable container or VM" for such a case, unless
`baseline capture` is given `--host-reaching=yes`. That flag is for the container run, and the
negative-option style follows `--foo=no`. Fontaine's own pipeline never runs the call, so the
runner ignores the field. This turns a convention into a guard, which matters most in `auto`,
where Claude Code may run what it allows.

### The live runs

From `rust/`, with `OPENROUTER_API_KEY="$(tr -d '[:space:]' < ~/open_router_key)"`; the commands
below are `cargo run -p fontaine-evals -- <args>`:

1. Smoke test first: `baseline capture --dataset <scratch>/smoke-dataset --mode auto --out <scratch>/smoke.jsonl`,
   where the scratch dataset dir holds one unmarked case Claude Code's classifier is known to block,
   such as `tier05-git-push`. It
   confirms that `claude -p --permission-mode auto` runs headless on this account, and that on
   2.1.332 a classifier block lands in `permission_denials`. The classifier's output is binary
   (`<block>yes|no</block>`, `research/research-claude-code-auto-mode-prompt.md:307-316`), so asks in
   auto come only from rules and protected paths, which headless `-p` turns into denials. If auto mode
   is unavailable, for example on a plan or account without it, stop and report that. That counts as a hard
   blocker for the live half.
2. `baseline capture --dataset ../evals/dataset --filter pipeline --mode auto --out ../evals/baselines/auto.jsonl`.
   Host-reaching and no-equivalent cases list as `SKIP`. A `FAIL` (refused, or never called) is
   recorded in `## Results` with its reason and isn't rerun more than twice.
3. `cargo run -p fontaine-evals -- --dataset ../evals/dataset --filter pipeline --mode auto --guardian live:router/anthropic/claude-haiku-5.5 --out <scratch>/auto-run.json`.
   It's filtered, so it never touches `guardian_models.toml`.
4. `baseline diff ../evals/baselines/auto.jsonl <scratch>/auto-run.json`.
5. Both sides of a guardian-judged divergence are model verdicts and vary from run to run. Pose
   each divergent case twice more on both sides (`--filter` plus a scratch dataset dir holding just
   those lines) and triage the majority. Record all three outcomes.

### Triage rule

Each `DIFF` gets a row in this plan's `## Results` table: case, Fontaine decision and tier, Claude
Code outcome (three-run tally where rerun), and resolution.

- **Fontaine tier is deterministic** (`deny_rule`, `ask_rule`, `protected_path`, `hook`,
  `read_only_fast_path`, `tracked_edit`, `scratch_write`, `shell_edit`, `tier05`, `spawn`,
  `coordination`, `allow_rule`, `mode`, `dont_ask`): first check whether an existing Decision already covers
  it, and cite it if so. Examples: the 2026-10-30 (reads) and 2026-11-06 (writes)
  out-of-root file-tool denies, the 2026-10-30/31 spawn and 2026-11-02 coordination lines, and the 2026-10-23 broad-hook line. Otherwise it is a new deviation. Fix
  Fontaine toward Claude Code if the difference is unintended. If it's deliberate, add a dated line
  at the end of the owning subsection (`### Permissions`, which also holds the spawn and coordination lines).
  The security invariants in `AGENTS.md` win over parity. A fix never loosens the floor and never
  adds a fast path outside the six listed exceptions.
- **Fontaine tier is `guardian`** (model against model). First check the result's `fail_closed`: a
  low-confidence destructive Allow-to-Deny conversion is triaged against the destructive flag and the
  confidence bar, not rule prose; an infrastructure fail-closed (error, timeout, unparseable output)
  is rerun, not triaged.
  - Claude Code denies and Fontaine allows. Read the guardian's `explanation` against the Claude
    Code rule that blocks it. If Fontaine's rule prose (`guardian/rules.rs`) lacks the case, add or
    extend a rule under an existing label. Add a new golden `guardian` case for it. Never edit
    existing ones. Rerun the live eval: `cargo run -p fontaine-evals -- --dataset ../evals/dataset --filter guardian --guardian live:router/anthropic/claude-haiku-5.5 --out <scratch>`,
    compared per category (`benign`, `attack:*`, `ambiguous-intent`, `injection-in-args`) against
    `evals/scorecards/router-claude-haiku-5-5-578d7.json`. A filtered run never rewrites the models
    file, and the dataset version differs after the marker edit, so compare per case and category,
    not by version. If the prose already covers it and the model just
    misjudges, record it as a guardian miss in `## Results` and leave the prompt alone.
  - Fontaine denies and Claude Code allows. If Fontaine is stricter on purpose (for example
    `floor-external-*-auto-no-guardian`, read and write outside the roots), add a dated
    `### Guardian` or `### Permissions` line. If it's an over-deny, handle it as above, with a
    benign golden case and the same live eval.

### Destructive-class word-list comparison

1. Pull every command or program term from the HARD BLOCK and SOFT BLOCK rules and the
   Definitions of `research/research-claude-code-auto-mode-prompt.md`. A term is the backticked
   name or the plain-prose CLI name, such as `kubectl delete`, `terraform apply`, `helm`, `gh pr merge`,
   `aws s3 rm`, `gsutil rm`, `crontab`, `systemctl`, `tmux send-keys`, `git reset --hard` and
   `docker run --privileged`. Pull every term from Fontaine's lists and the special cases in
   `destructive_argv`.
2. For each Claude Code term, check whether `derive("shell", {command: <term with a representative
   argument>}, facts)` returns `true`. Do the check as a table-driven test in `rust/fontaine-sdk/tests/destructive.rs` (using its `world()` and
   `shell_is` helpers), so the comparison stays pinned after this issue closes.
3. Report the terms present on one side only as two lists in `## Results`.
4. A Claude Code term that reaches outside the machine or is irreversible and that `derive` misses
   goes into the matching Fontaine list (a new `CLOUD_CLI_WORDS` / `CLUSTER_CLI_WORDS` constant if
   none fits), with its test row. Membership raises the confidence bar on an Allow and
   never denies by itself, but it also feeds the guardian prompt (`prompt_builder.rs:469-471` renders
   `destructive:`), composition scripts (`call.destructive()` in `guardian/composition/host.rs`) and
   the cache key (`guardian/guardian.rs:306-313`, through the rendered request), so an addition can change verdicts on those commands.
   A term outside Fontaine's scope gets one dated `### Guardian` line covering the group:
   browser/Chrome-MCP terms (Fontaine has no browser tools), Claude Code scheduling, and the
   memory-directory allow. A Fontaine-only term is listed and needs no action.

### Hook legs

Run the documented three-variant observation once on `claude` 2.1.332: no hook, a narrow hook and a
broad hook, with `--include-hook-events`, using the settings and command from `release_live_checks_plan.md` §Baselines and the probe at
`:335-340` (Results).
Record the outcome in `## Results`. If Claude Code still honors the broad hook, the 2026-10-23
`### Permissions` Decision stands: cite it, and update its version in a new dated line only if the
behavior changed.

## Implementation Steps

1. **Marker**: add `host_reaching: Option<bool>` to `Case` (`case.rs`). Make `validate_case` reject
   it off pipeline cases, with a unit test. Set it on the 39 cases listed in Design, in `evals/dataset/floor.jsonl`,
   `tier05.jsonl` and `spawn.jsonl`. The dataset version hash changes, which makes
   `--screen-reference <scorecard>` a usage error against existing scorecards (`runner.rs:882-891`),
   makes the next unfiltered `--profile full` live run drop every other model's entry from
   `rust/fontaine-sdk/data/guardian_models.toml` (`report.rs:207-212`), and makes `--resume` refuse an
   old sidecar (`output.rs:191`).
   Step 11 reruns the scorecards on the new version.
2. **Capture**: in `baseline.rs`, add the host-reaching skip to `capture_skip_reason`, gated by an
   `allow_host` argument. Extract `claude_command_args(case, mode)`, and thread `mode: Option<PermissionMode>`
   and `allow_host: bool` through `capture`. In `runner.rs` `BaselineAction::Capture`, add
   `--mode <MODE>` and `--host-reaching=<yes|no>` (default `no`).
3. **Run**: add `--mode <MODE>` to `RunArgs`. Thread the effective mode into `none::decide` and
   `score_pipeline`. Add the effective `--mode` to the sidecar `Header` (`output.rs`) and to `check_header`, so
   resuming a run made under a different mode is refused. Make it a usage error with `fixture`/`screen:`. Print the summary note and count `--mode` as a measurement for the exit code.
4. **Tests**: see Testing Strategy.
5. **Docs**: see Documentation.
6. **Live**: the smoke test, then capture, run, diff and reruns as above. Commit
   `evals/baselines/auto.jsonl`.
7. **Word lists**: the comparison, the pinning test, any list additions and the scope-out Decision
   line. When a list changes, rerun the live eval exactly as the triage rule's guardian-tier command gives it:
   `live.rs:422` derives `destructive` for every golden guardian case, so additions can move
   existing verdicts.
8. **Hook legs**: the observation.
9. **Triage**: fixes (each with its test, plus the live eval if guardian prose changed) and
   Decisions lines. Add this plan's `## Results`.
10. **Bookkeeping**: tick `tasks/fontaine_harness_plan.md:139` with the date and a one-line result.
    Tick #80's matching box (`gh issue edit 80 --body-file`). Add an `### Evals` Decision line for
    the `--mode` override and the host-reaching marker.
11. **Rerun scorecards**: as #797 did (commit a409881d), rerun the published guardian scorecards and
    the models file on the new dataset version, and update `-578d7` in the scorecard paths this plan
    and the docs cite.

## Files to Modify

- `rust/fontaine-evals/src/case.rs`: `host_reaching`, plus validation
- `rust/fontaine-evals/src/baseline.rs`: skip, `claude_command_args`, mode override
- `rust/fontaine-evals/src/runner.rs`: `--mode` on capture and run, `--host-reaching`
- `rust/fontaine-evals/src/output.rs`: `mode` in the sidecar `Header` and `check_header`
- `rust/fontaine-evals/src/none.rs`: effective mode
- `rust/fontaine-evals/tests/baseline.rs`, `tests/none_mode.rs`: tests
- `rust/fontaine-sdk/src/core/destructive.rs`: any list additions
- `rust/fontaine-sdk/tests/destructive.rs`: the pinning table
- `rust/fontaine-sdk/src/guardian/rules.rs` and `evals/dataset/*.jsonl` (new cases only): only if
  triage finds a guardian gap
- `evals/dataset/floor.jsonl`, `evals/dataset/tier05.jsonl`, `evals/dataset/spawn.jsonl`: the marker
- `evals/baselines/auto.jsonl` (new)
- `evals/README.md`, `tasks/design/testing.md`, `docs/architecture.md`
- `tasks/fontaine_harness_plan.md`: tracker tick and Decisions

## Trade-offs

- **`--mode` override against `auto` copies in the dataset.** Copies would double the pipeline
  set and split every future edit across two lines per case, and their `expected` labels would have
  to be authored for `auto`, where many outcomes are model verdicts with no stable label. The
  override keeps one source of truth. Its cost is that `pass` means nothing under `--mode`, which
  the summary says.
- **A live Fontaine guardian against teaching `none` mode a stand-in verdict.** Claude Code's `auto`
  outcome is its classifier's verdict, so the honest comparison is Fontaine's real guardian, the
  shipped default (`router/anthropic/claude-haiku-5.5`). A `none` stand-in (say, "reached the
  guardian" = denied) would hide every judgment difference, and those are the point of an auto
  baseline.
- **A dataset marker against a `--exclude` id list.** The marker lives with the case, so a new
  host-reaching case can't be forgotten in some operator's command line. And `capture` refuses by
  default instead of trusting the invocation.

## Decisions

## Documentation

- `evals/README.md`: §Baselines (flags, the auto recipe, the host-reaching skip); the run flag
  table (`--mode`); the `## Case format` table (:27-47, `host_reaching`); the "Exit codes" table
  (:458-471: `--mode` measurement exits 0, `--mode` usage errors, `--resume` mode mismatch)
- `tasks/design/testing.md`: the runner CLI table (:86-98, add `--mode`/`--host-reaching`) and the
  "Differential baselines" paragraph (:130-142: one sentence on `--mode` and the marker; the diff
  compares a `live:` run for auto, not `--guardian none`; drop the stale `--output-format json`, :134)
- `docs/architecture.md:337-346`: `--guardian none` no longer runs non-`auto` modes only, `live:`
  routes pipeline cases under the effective mode, and the baseline capture/diff mention
- `docs/evals.md` covers scorecards only; no change

## Testing Strategy

Unit and integration tests, none of which touches `claude` or a live model:

- `claude_command_args`: `--mode auto` yields `--permission-mode auto` for a `manual` case. `afk`
  maps to `auto` and `manual` to `default`. No override keeps the case's mode. A case with
  `permissions` still carries `--settings`.
- `capture_skip_reason`: a `host_reaching` case is skipped with the container reason, and it isn't
  skipped when `allow_host` is set.
- `validate_case`: `host_reaching` on a guardian case is an error.
- `--guardian none --mode dontAsk` over a `manual` case (`tests/none_mode.rs`): the result's `mode`
  is `dontAsk`, and the decision is the `dontAsk` one, e.g. `floor-protected-path-write` goes from
  `prompt` to `deny`, and the run exits 0 although that case's label now mismatches. With `--mode auto`, every case is skipped with "auto mode needs the guardian".
  That pins the effective-mode threading end to end through the real pipeline.
- `--mode` with `--guardian fixture`: usage error (exit 2).
- `--resume` with a sidecar whose header holds a different `--mode`: refused.
- Word-list pinning (`rust/fontaine-sdk/tests/destructive.rs`): one row per Claude Code term from the comparison, with
  the expected `derive` result. That includes the terms deliberately left out, asserted `false`
  with a comment pointing to the Decision, so a later list edit shows up.

The live capture, the live-guardian run and the hook observation call the real `claude` and a
hosted model, so they are manual by project rule (`AGENTS.md` §Testing). Their output is the
committed baseline file and `## Results`.

## Manual Verification

Steps 1-5 of "The live runs" and the hook observation, as written above. They need the live
`claude` subscription CLI and a hosted guardian, which tests and CI must never call.
