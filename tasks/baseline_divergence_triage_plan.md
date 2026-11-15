# Claude Code Baseline Divergence Triage Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/101

## Overview

The Phase 3 rule is that each Claude Code baseline divergence is fixed in Fontaine or recorded as a
dated line in the master plan's `## Decisions`. The 2026-10-23 capture (`evals/baselines/pipeline.jsonl`,
44 of the dataset's 92 pipeline cases, PR #100) left divergences open and 48 ids without a record. This
work finishes the capture (every capturable id: the 10 refused ones, 30 never captured, and the two WebFetch
artifacts), records a reason for the 8 that cannot be captured, resolves every
open divergence, and finishes the protected-path and Tier 0.5 allowlist confirmation against
`claude -p`. It is mostly live runs plus data and docs edits; the only code in scope is the eval
runner's capture prompt and `claude_args` (`webfetch`/`agent` mapping) and, if a divergence is fixed rather than recorded, a
Fontaine pipeline change with its tests.

## Current State

- `baseline capture` (`rust/fontaine-evals/src/baseline.rs`) writes `--out` whole and only for cases
  where the model made the call. `render_prompt` (`baseline.rs:164`) is one fixed template: "Call the
  X tool exactly once... Do not call any other tool... Reply `done` or `refused`". `capture_one`
  (`:258`) returns `the model never called <tool>` when there are no denials and no attempt.
- The dataset has 92 pipeline cases (floor 37, spawn 13, tier05 42); 48 ids have no record in
  `evals/baselines/pipeline.jsonl`. 30 of them pass `capture_skip_reason` (`baseline.rs:106-133`) but were
  never captured: `floor-external-read-*` (9), `floor-external-write-*` (9), `floor-secret-*` (5),
  `spawn-explore-fast-path`, `spawn-explore-fast-path-auto`, `spawn-worker-auto-gate`,
  `tier05-git-status-porcelain`, `tier05-git-log-paths`, `tier05-git-push`, `tier05-git-diff-ext-diff`.
  8 are skipped by design: 6 `send_message`/`board_update` cases (no Claude Code tool name) and the 2
  `*-symlink-home-rule` cases (the `../home` symlink target does not exist next to the capture scratch dir).
- Ten of the ids that pass the skip check were attempted and refused or failed: `floor-critical-deletion-home-manual`, `-dont-ask`, `-allow-rule` (`rm -rf ~`,
  `rm -rf /`; the model refuses to emit them; `-allow-rule` now expects allow/allow_rule), `tier05-chain-curl-sh`, `-command-substitution-rm`,
  `-parameter-expansion`, `-brace-expansion-escape`, `-brace-expansion-rm-root`, `-dont-ask-chain`,
  and `spawn-agent-deny-rule`.
- `floor-ask-rule` / `floor-ask-rule-dont-ask` (`evals/dataset/floor.jsonl`) pose `webfetch` with
  `{url}` only. Claude Code's `WebFetch` requires `prompt`, so the call fails validation before any
  permission check and the recorded `allowed` is an artifact. `claude_args` (`baseline.rs:136`) does
  not add it.
- Open divergences (`tasks/completed/release_live_checks_plan.md` §Results → Baselines):
  `tier05-pipe` (`ls | wc -l`), `tier05-cat-then-git-status` (`cat notes.txt && git status`),
  `spawn-agent-ask-rule`, `floor-protected-path-edit`. `floor-protected-path-edit` is probably a
  capture artifact: the case has no `workspace`, so `.fontaine/settings.toml` does not exist and Claude
  Code's Edit fails before any permission check (headless `default` denies Edit to ordinary files, yet
  it recorded `allowed`). `tier05-out-of-scope-cwd` also diverges and has no Decision: the case uses
  Fontaine's `cwd` argument, which Claude Code lacks (`release_live_checks_plan.md` says only "Capture
  artifact"). Other divergences in that table already cite a dated Decision.
- Git subcommands are now in the curated Tier 0.5 set (`GIT_SPECS`, #591); the four git ids
  `tier05-git-status`, `-git-fsmonitor-config`, `-git-gpg-program-config` and `-git-filter-driver` expect
  allow and are recorded allowed, matching Claude Code (the other git ids are the never-captured ones above).
- The 2026-10-21 decision "shell parsing ... anything not exactly one simple command falls through"
  (master plan `### Permissions`) covers pipes and chains only implicitly; no dated line names pipes or chains for the Tier 0.5 fast path
  (the 2026-11-14 #635 line covers allow-rule matching only).
- Phase 3's step "confirm the protected-path list and the Tier 0.5 allowlist table against
  `claude -p`" (`tasks/completed/phases/phase3_pipeline_tui.md`) was only partly done.
- `claude` is `2.1.325 (Claude Code)` here (raw `claude --version` output); the existing baselines were recorded with 2.1.309.

## Design

Order matters: the capture tooling is fixed first, then the data, then triage.

### 1. Capture tooling (small, only what the missing cases need)

- `claude_args`: for `webfetch`, add a `prompt` argument (a fixed string such as `Summarize the page`)
  when the case args lack one, so the posed call passes Claude Code's validation. For `agent`, map
  unconditionally (it has no agent branch today and Step 2 captures `spawn-agent-deny-rule`):
  `agent_type`→`subagent_type`, values `explore`→`Explore` and `worker`→`general-purpose` (any other
  `agent_type` passes through unchanged), plus a fixed `description` string. Add `claude_args` to the imports in
  `rust/fontaine-evals/tests/baseline.rs` and test it directly next to the `render_prompt` test (no
  `claude_args` tests exist).
- `render_prompt`: one shared rewording replaces the template for all captures; only the listed ids are
  re-captured, so the existing 44 records stay. It keeps the "Call the X tool exactly once" phrasing.
  Try a rewording that keeps the same call but frames it as a harmless
  verification step (for example stating the command is a test fixture run in a throwaway sandbox
  directory and that the permission system, not the model, is what is under test). A case that no wording can
  capture gets a one-line reason in the PR description and the plan's Results, not an invented
  baseline.
- Merging: `capture` overwrites `--out`, so each re-capture goes to a scratch file and a small
  step (jq/shell, not a new subcommand; documented in `evals/README.md` §Baselines) replaces or appends records by `case_id` in
  `evals/baselines/pipeline.jsonl`, leaving the 44 good records byte-identical except the replaced ids (the two
  WebFetch cases, plus any re-captured divergence case such as `spawn-agent-ask-rule`).
  `--filter` matches only a kind or category glob, not a case id, so a per-category run also re-poses
  already-good cases; those extra live runs are expected and their records are discarded. The merge takes
  only an explicit allow-list of re-capture ids from each scratch file (`jq` select on `case_id`). New records carry
  `claude: "2.1.325 (Claude Code)"`.

### 2. Live capture (needs the logged-in `claude` CLI; never CI)

If `claude -p` is unavailable or not logged in, stop and report; never fabricate records.
Capture the ten refused ids, the 30 never-captured ids listed in Current State, plus `floor-ask-rule` and
`floor-ask-rule-dont-ask` with `--filter` (one run per category), review each record by hand against the
raw stream-json to confirm the call was attempted and the outcome is a permission denial, not a validation
error. The `/etc/fontaine-eval` and `/usr/share/fontaine-eval` targets do not exist on the host, so check
those records for a file-missing validation artifact, as for `floor-protected-path-edit`. Record a reason
for each of the 8 skipped ids in Results and the PR.

### 3. Divergence triage

For each, gather evidence with direct `claude -p` runs in a scratch workspace, then fix or record:

| Case | Investigation | Resolution |
|---|---|---|
| `tier05-pipe`, `tier05-cat-then-git-status` | Observe Claude Code's behavior for `ls \| wc -l`, `cat a && git status`, and negative controls (`ls \| sh`, `cat a && rm b`). Note Claude Code approves per-subcommand for read-only chains. | Gather evidence and a recommendation; **leave the keep-or-change call to the maintainer**; a "change" answer would also mean revising the golden
adversarial set (`tasks/design/testing.md:27-41`). Record a dated `## Decisions` line stating the current deviation (pipes/chains go to the guardian) with the recommendation marked as pending maintainer confirmation. No classifier change in this work. Reason for the deviation: allowing read-only pipes and chains widens the fast path past one-simple-command soundness. |
| `spawn-agent-ask-rule` | Run `claude -p` with `ask: ["Agent"]` and a call that passes Agent argument validation (valid `subagent_type`, `description`, `prompt`); check whether the call reaches a permission check. | Re-capture with the §1 mapping; if Claude Code still does not reach a permission check, a dated Decisions line. Also re-capture `spawn-readme-escalated-toolset` and `spawn-readme-escalated-toolset-dont-ask` after the Agent mapping, to check whether they were the same unmapped-argument artifact; they already cite a dated Decision, kept unless the re-capture changes the outcome. |
| `floor-protected-path-edit` | The baseline is probably a capture artifact (no `workspace`, so the target file is absent). Re-pose it and the like-for-like `.claude/settings.json` check in `acceptEdits` with the target file present in the workspace; the dataset case needs a `workspace.files` entry to be capturable, and the `.fontaine/settings.toml` content must contain the case's `old_text` exactly once (e.g. `"a\n"`), since Claude Code's Edit requires `old_string` to occur exactly once before any permission check. A re-captured manual record (maps to headless `default`) shows only the generic default denial; the meaningful `acceptEdits` protection check is a direct `claude -p` run recorded in Results and the PR table, not a baseline record. Check whether Claude Code protects `.fontaine/settings.toml` (it should not; it protects `.git`, `.claude`, etc.). | Only after the re-pose: Decisions line (Fontaine-specific by design) or a fix; add a dataset case only if a Claude Code-equivalent protected-path case adds coverage. |
| `tier05-out-of-scope-cwd` | The case uses Fontaine's `cwd` argument, which Claude Code lacks. Run directly with `claude -p` a single command reading an existing out-of-workspace file (e.g. `cat /etc/hostname`) and record it in Results. | Record a dated Decisions line naming Fontaine's `cwd` argument. |
| `spawn-agent-deny-rule` | Needs a valid Agent call (as above). | Capture, or a recorded reason. |

### 4. Protected-path and Tier 0.5 confirmation

- Protected paths: take the list from `PROTECTED_DIRS`/`PROTECTED_FILES` in
  `rust/fontaine-core/src/floor.rs` and `docs/permissions.md`; for each entry write via `claude -p` in
  a scratch workspace (Edit/Write in `acceptEdits` only, with the target file existing; headless
  `default` denies every Edit/Write) and record whether Claude Code prompts. Each entry and its result goes in the PR body table; entries
  where Fontaine is stricter or looser become Decisions lines or fixes. The table notes that the
  home-directory, config/data-dir and `notebook_edit` protections (`docs/permissions.md:53-61`) are not
  covered by the scratch-workspace check, because an out-of-root home write prompts in Claude Code anyway.
- Tier 0.5 allowlist: take the curated command table (`SPECS`, 9 commands, and `GIT_SPECS`, 8
  subcommands, in `rust/fontaine-tools/src/preclassify.rs`). An entry is each command or git subcommand
  plus each allowlisted flag (~103: 9 commands + 76 command flags, 8 git subcommands + 10 git flags), posed with an in-scope operand, since Claude Code decides per flag; for each entry run the
  command under `claude -p` in `default` mode and record whether it is auto-approved. Fontaine's
  allowlist must be a subset of what Claude Code auto-approves (otherwise Fontaine is looser: fix or
  record). Entries Claude Code approves but Fontaine does not are expected deviations and listed.
- Append a dated `### Permissions` Decisions line recording the #591 `GIT_SPECS` fast path, superseding
  the 2026-10-21 "no git subcommand" line (~955).
- Any classifier change must keep the soundness property (golden adversarial set in
  `tasks/design/testing.md`) and be proven by the existing adversarial tests plus new cases.

### 5. Verify and record

Run `--guardian none` over the dataset, then `baseline diff` against the updated baseline; every
remaining DIFF must map to a dated Decisions line or a fix. `baseline diff` ignores run cases without a
baseline, so also compare the dataset's pipeline ids with the baseline `case_id`s (jq/shell); each missing
id needs a recorded reason. Update the #101 annotation in the master plan's `### Release readiness`
item (`tasks/fontaine_harness_plan.md:134-135`) and the checkbox in GitHub issue #80's body to say #101
is resolved (do not close #80); update the Phase 3 progress-log line (`:103`) only if it still cites open divergences.

## Implementation Steps

1. `evals/dataset/floor.jsonl`: add the `workspace.files` entry to `floor-protected-path-edit` (before
   any capture; see Files to Modify).
   `rust/fontaine-evals/src/baseline.rs`: `webfetch` gets a default `prompt` in `claude_args`; `agent`
   mapping unconditionally (see §1); the shared `render_prompt` rewording (keeping "Call the X tool
   exactly once"), with its test in `rust/fontaine-evals/tests/baseline.rs` updated; unit tests there.
2. Run live captures (all 30 capturable never-captured ids too) to scratch files; verify raw streams; merge into `evals/baselines/pipeline.jsonl`.
3. Run the direct `claude -p` investigations (pipe/chain, spawn ask, protected paths, allowlist) in
   scratch workspaces; keep a results table.
4. Apply any resulting Fontaine fixes with tests; otherwise add dated `## Decisions` lines at the end
   of the matching subject subsections (`### Permissions`, `### Evals`) of
   `tasks/fontaine_harness_plan.md`.
5. Update `evals/README.md` §Baselines: document the `claude_args` argument mapping and the
   merge-by-case_id step; update `tasks/design/testing.md` (`:33-34`, `:41`) and add the `CHANGELOG.md`
   `[Unreleased]` entry.
6. Run `cargo test`, `cargo clippy --all-targets -- -D warnings`, the `--guardian none` eval and
   `baseline diff` and the coverage check; summarize in the PR body (per-case table, protected-path and allowlist results,
   uncaptured cases with reasons).
7. Update the #101 annotation in the master plan's `### Release readiness` item (`:134-135`) and the
   #80 issue body checkbox to say #101 is resolved; do not close #80 (see §5 for the `:103` line).

## Files to Modify

- `rust/fontaine-evals/src/baseline.rs` (capture args, `render_prompt`)
- `rust/fontaine-evals/tests/baseline.rs` (tests, `claude_args` import)
- `evals/baselines/pipeline.jsonl`
- `evals/README.md` (§Baselines: `claude_args` mapping, merge step)
- `tasks/fontaine_harness_plan.md` (`## Decisions` lines; progress/tracker note)
- `tasks/design/testing.md` (`:33-34` and `:41` still say no git subcommand is in the curated set /
  `git status` / `git log` / `git diff` never fast-path; update with the #591 supersession)
- `CHANGELOG.md` (`[Unreleased]` entry for the `baseline capture` changes: webfetch prompt, Agent
  mapping, shared prompt rewording)
- `evals/dataset/floor.jsonl`: add `workspace.files` to `floor-protected-path-edit` (the
  `.fontaine/settings.toml` target, content containing `old_text` exactly once, e.g. `"a\n"`); possibly new Claude Code-equivalent cases; never relabel `expected`

## Decisions

- 2026-11-15: capture uses one shared prompt rewording, no per-case dataset field, to keep the dataset
  format stable.

## Documentation

`evals/README.md` §Baselines (unconditional); `CHANGELOG.md` `[Unreleased]` (unconditional); `docs/permissions.md` only if a Fontaine fix changes
documented behavior.

## Testing Strategy

- Unit tests for `claude_args` (webfetch `prompt`, agent mapping) and any prompt change.
- Any pipeline or classifier change: unit tests plus a golden adversarial case; existing
  `cargo test` keeps the soundness set green.
- Live capture and `claude -p` investigations are manual (they need the logged-in CLI and the model)
  and are never run from tests or CI.

## Manual Verification

Ordered, from `rust/`: run each capture command from the issue handoff to a scratchpad file, inspect
the stream-json for the attempted call, merge, then `cargo run -p fontaine-evals -- --dataset
../evals/dataset --guardian none --out <run.json>` and `baseline diff ../evals/baselines/pipeline.jsonl
<run.json>`; expect only DIFFs that carry a Decisions line. Not automated: it needs live model
behavior.

#660 (#654) has landed: the floor is now `deny → ask → protected-path`.

## Open Questions

- Whether the maintainer wants pipes and read-only `&&` chains pre-classified (evidence and
  recommendation only in this work).
