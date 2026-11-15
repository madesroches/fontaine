# Claude Code Baseline Divergence Triage Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/101

## Overview

The Phase 3 rule is that each Claude Code baseline divergence is fixed in Fontaine or recorded as a
dated line in the master plan's `## Decisions`. The 2026-10-23 capture (`evals/baselines/pipeline.jsonl`,
44 of 54 pipeline cases, PR #100) left divergences open and 10 cases without a baseline. This work
finishes the capture (re-pose the 10 missing cases and the two WebFetch artifacts), resolves every
open divergence, and finishes the protected-path and Tier 0.5 allowlist confirmation against
`claude -p`. It is mostly live runs plus data and docs edits; the only code in scope is the eval
runner's capture prompt (if rewording needs it) and, if a divergence is fixed rather than recorded, a
Fontaine pipeline change with its tests.

## Current State

- `baseline capture` (`rust/fontaine-evals/src/baseline.rs`) writes `--out` whole and only for cases
  where the model made the call. `render_prompt` (`baseline.rs:164`) is one fixed template: "Call the
  X tool exactly once... Do not call any other tool... Reply `done` or `refused`". `capture_one`
  (`:258`) returns `the model never called <tool>` when there are no denials and no attempt.
- The ten uncaptured cases: `floor-critical-path-home`, `-dont-ask`, `-beats-allow` (`rm -rf ~`,
  `rm -rf /`; the model refuses to emit them), `tier05-chain-curl-sh`, `-command-substitution-rm`,
  `-parameter-expansion`, `-brace-expansion-escape`, `-brace-expansion-rm-root`, `-dont-ask-chain`,
  and `spawn-agent-deny-rule`.
- `floor-ask-rule` / `floor-ask-rule-dont-ask` (`evals/dataset/floor.jsonl`) pose `webfetch` with
  `{url}` only. Claude Code's `WebFetch` requires `prompt`, so the call fails validation before any
  permission check and the recorded `allowed` is an artifact. `claude_args` (`baseline.rs:136`) does
  not add it.
- Open divergences (`tasks/completed/release_live_checks_plan.md` §Results → Baselines):
  `tier05-pipe` (`ls | wc -l`), `tier05-cat-then-git-status` (`cat notes.txt && git status`),
  `spawn-agent-ask-rule`, `floor-protected-path-edit`. Other divergences in that table already cite a
  dated Decision.
- The 2026-10-21 decision "shell parsing ... anything not exactly one simple command falls through"
  (master plan `### Permissions`) covers pipes and chains only implicitly; no dated line names them.
- Phase 3's step "confirm the protected-path list and the Tier 0.5 allowlist table against
  `claude -p`" (`tasks/completed/phases/phase3_pipeline_tui.md`) was only partly done.
- `claude` is 2.1.325 here; the existing baselines were recorded with 2.1.309.

## Design

Order matters: the capture tooling is fixed first, then the data, then triage.

### 1. Capture tooling (small, only what the missing cases need)

- `claude_args`: for `webfetch`, add a `prompt` argument (a fixed string such as `Summarize the page`)
  when the case args lack one, so the posed call passes Claude Code's validation. Unit-test it next to
  the existing `claude_args` tests.
- `render_prompt`: keep the template for existing cases. Add an optional dataset field is *not*
  warranted yet; first try a rewording that keeps the same call but frames it as a harmless
  verification step (for example stating the command is a test fixture run in a throwaway sandbox
  directory and that the permission system, not the model, is what is under test). Only if one
  shared reworded template still leaves cases uncaptured, add a `--prompt-style` flag to `capture`
  rather than a per-case field, so the dataset format is unchanged. A case that no wording can
  capture gets a one-line reason in the PR description and the plan's Results, not an invented
  baseline.
- Merging: `capture` overwrites `--out`, so each re-capture goes to a scratch file and a small
  documented step (jq/shell, not a new subcommand) replaces or appends records by `case_id` in
  `evals/baselines/pipeline.jsonl`, leaving the 44 good records byte-identical except the two
  WebFetch replacements. New records carry `claude: 2.1.325`.

### 2. Live capture (needs the logged-in `claude` CLI; never CI)

If `claude -p` is unavailable or not logged in, stop and report; never fabricate records.
Capture the ten missing ids plus `floor-ask-rule` and `floor-ask-rule-dont-ask` with `--filter`
(one run per category), review each record by hand against the raw stream-json to confirm the call
was attempted and the outcome is a permission denial, not a validation error.

### 3. Divergence triage

For each, gather evidence with direct `claude -p` runs in a scratch workspace, then fix or record:

| Case | Investigation | Resolution |
|---|---|---|
| `tier05-pipe`, `tier05-cat-then-git-status` | Observe Claude Code's behavior for `ls \| wc -l`, `cat a && git status`, and negative controls (`ls \| sh`, `cat a && rm b`). Note Claude Code approves per-subcommand for read-only chains. | Gather evidence and a recommendation; **leave the keep-or-change call to the maintainer**. Record a dated `## Decisions` line stating the current deviation (pipes/chains go to the guardian) with the recommendation marked as pending maintainer confirmation. No classifier change in this work. |
| `spawn-agent-ask-rule` | Run `claude -p` with `ask: ["Agent"]` and a call that passes Agent argument validation (valid `subagent_type`, `description`, `prompt`); check whether the call reaches a permission check. | If Claude Code prompts and Fontaine also does, fix the capture args (`claude_args` for `agent`: map `agent_type`→`subagent_type`, add `description`) and re-capture; else a Decisions line. |
| `floor-protected-path-edit` | Check whether Claude Code protects `.fontaine/settings.toml` (it should not; it protects `.git`, `.claude`, etc.). Pose the equivalent Claude Code protected path (`.claude/settings.json`) for a like-for-like check. | Decisions line: Fontaine-specific by design; add a dataset case only if a Claude Code-equivalent protected-path case adds coverage. |
| `spawn-agent-deny-rule` | Needs a valid Agent call (as above). | Capture, or a recorded reason. |

### 4. Protected-path and Tier 0.5 confirmation

- Protected paths: take the list from the floor implementation and `docs/permissions.md`; for each
  entry write via `claude -p` in a scratch workspace (Edit/Write in `default` and `acceptEdits`) and
  record whether Claude Code prompts. Each entry and its result goes in the PR body table; entries
  where Fontaine is stricter or looser become Decisions lines or fixes.
- Tier 0.5 allowlist: take the curated command table from the pre-classifier; for each entry run the
  command under `claude -p` in `default` mode and record whether it is auto-approved. Fontaine's
  allowlist must be a subset of what Claude Code auto-approves (otherwise Fontaine is looser: fix or
  record). Entries Claude Code approves but Fontaine does not are expected deviations and listed.
- Any classifier change must keep the soundness property (golden adversarial set in
  `tasks/design/testing.md`) and be proven by the existing adversarial tests plus new cases.

### 5. Verify and record

Run `--guardian none` over the dataset, then `baseline diff` against the updated baseline; every
remaining DIFF must map to a dated Decisions line or a fix. Tick the item in #80 and update the Phase
3 / release-readiness entry in the master plan.

## Implementation Steps

1. `rust/fontaine-evals/src/baseline.rs`: `webfetch` gets a default `prompt` in `claude_args`; `agent`
   argument mapping if investigation shows it is needed; unit tests.
2. Run live captures to scratch files; verify raw streams; merge into `evals/baselines/pipeline.jsonl`.
3. Run the direct `claude -p` investigations (pipe/chain, spawn ask, protected paths, allowlist) in
   scratch workspaces; keep a results table.
4. Apply any resulting Fontaine fixes with tests; otherwise add dated `## Decisions` lines at the end
   of the matching subject subsections (`### Permissions`, `### Evals`) of
   `tasks/fontaine_harness_plan.md`.
5. Update `evals/README.md` §Baselines if the capture flags or the webfetch/agent mapping changed.
6. Run `cargo test`, `cargo clippy --all-targets -- -D warnings`, the `--guardian none` eval and
   `baseline diff`; summarize in the PR body (per-case table, protected-path and allowlist results,
   uncaptured cases with reasons).
7. Tick the item in #80 (`gh issue comment` / edit by the maintainer's process; do not close #80).

## Files to Modify

- `rust/fontaine-evals/src/baseline.rs` (capture args, tests)
- `evals/baselines/pipeline.jsonl`
- `evals/README.md` (only if flags/mapping change)
- `tasks/fontaine_harness_plan.md` (`## Decisions` lines; progress/tracker note)
- Possibly `evals/dataset/*.jsonl` only to add a Claude Code-equivalent case, never to relabel `expected`

## Trade-offs

- Recording the pipe/chain deviation rather than extending Tier 0.5: allowing read-only pipes and
  chains widens the fast path past one-simple-command soundness; the issue reserves the call for the
  maintainer.
- A shared capture-prompt rewording (or one flag) rather than per-case prompt fields: keeps the
  dataset format stable.

## Decisions

## Documentation

`evals/README.md` §Baselines if flags change; `docs/permissions.md` only if a Fontaine fix changes
documented behavior.

## Testing Strategy

- Unit tests for `claude_args` (webfetch `prompt`, agent mapping if added) and any prompt/flag change.
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

## Open Questions

- Whether the maintainer wants pipes and read-only `&&` chains pre-classified (evidence and
  recommendation only in this work).
