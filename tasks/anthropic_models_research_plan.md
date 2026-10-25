# Claude Models on Third-Party Hosts Research Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/157

## Overview

Issue #157 asks for a research document, `research/research-anthropic-models.md`, on how Fontaine
can get the most out of Claude models when they are reached through Microsoft Foundry on Azure or
Databricks Model Serving instead of Anthropic's own API. The deliverable is a write-up and
follow-up GitHub issues, not code. This plan fixes the method, the document's structure and the
evidence standard so the study can be carried out and reviewed. Its shape is modeled on the
issue's questions and on `research/research-claude-code-parity.md`, with Claude Code as the
reference harness; #154 DeepSeek, #155 Qwen and #156 Kimi are sibling studies.

## Current State

- `rust/fontaine-core/src/providers/anthropic.rs` wraps rig's `providers::anthropic` client
  (`rig-core =0.45.0`) with an API key and an optional base URL override. It has no `thinking_off`
  override, no beta-header handling and no cloud-host variants.
- `providers/opencode_go.rs` already serves `claude-*` ids over the Anthropic Messages wire through
  rig's `Anthropic` client (`wire()`, line ~153). Its `thinking_off` leaves Claude ids alone
  because Claude thinks only when asked (`opencode_go.rs:172`). This is the one existing precedent
  for Claude behind a non-Anthropic gateway.
- `AccountClient::thinking_off` (`providers/mod.rs:158`) is how the guardian and scanner force a
  tool call on models that would otherwise think. `models.rs:111-134`, `agent.rs:140`, `scanner_stage.rs:58` (in `fontaine-core/src`) and
  `fontaine-evals/src/live.rs:119` apply it.
- rig-core 0.45.0 ships `providers/anthropic` (with `cache_control`, thinking, signature and
  streaming support in `completion.rs`, `options.rs`, `streaming.rs`) and `providers/azure`. It has
  no Databricks provider, so Databricks would go through rig's OpenAI-compatible client or need a
  new kind.
- Related open issues that own implementation, not this study: #55 (account kinds `bedrock`,
  `vertex`, `azure`), #53 (reasoning effort and thinking budget), #13 (prompt caching), #56 (usage
  accounting), #34 (tool search / deferred loading), #21 (images and PDFs). #154-#156 are the
  sibling studies and their documents do not exist yet, so there is no template to copy; the
  closest precedent for layout is `research/research-claude-code-parity.md` and
  `research/research-llamafirewall.md`.
- No Azure or Databricks credentials are present in the environment (no `AZURE_*` or
  `DATABRICKS_*` variables), so live requests cannot be assumed.

## Design

The study has four parts, matching the issue's questions. Each part ends in rows for the central
support list.

### 1. Hosts and wires

For each of Foundry-on-Azure and Databricks Model Serving, from the host's published docs and
Anthropic's docs for third-party platforms, record:

- the API shape for Claude (Anthropic Messages, OpenAI-compatible chat, or the host's own) and the
  exact base URL pattern and model-id pattern;
- authentication (API key, Entra ID, Databricks token or OAuth) and what rig's existing clients
  cover versus what needs a new account kind beyond #55;
- which `anthropic-beta` headers and request fields it passes through, rejects or silently drops;
- release lag for new Claude models and features compared with Anthropic's API.

### 2. Model-specific support matrix

One row per item in the issue's list: thinking and signature round-tripping, effort and
thinking-budget, `cache_control` and TTLs, usage accounting, context-window sizes and long-context
beta, parallel and fine-grained-streaming tool use, tool search, server-side tools, context
management (clearing old tool results), images and PDFs, and sampling parameters that conflict
with thinking. Columns: what Claude Code does (from Claude Code's docs, and observed with `claude -p`
where it can be observed), what Fontaine does today (from the code paths above), Azure support,
Databricks support, evidence, needed-for-correctness versus optimisation, and the Fontaine issue
it feeds. Support values are limited to `yes`, `no`, `partial`, `untested` and `unknown`, each with
its evidence; a claim from docs alone is marked as docs-only.

### 3. Guardian and screen

For each host, which Claude models are available and viable as a guardian or screen model. The
guardian needs: thinking off or forced tool choice working (compare `thinking_off` handling),
structured tool output, acceptable latency, and fail-closed behaviour on host errors. Findings feed
the guardian profile guidance in `docs/guardian.md` only as follow-up issues, not edits here.

### 4. What breaks

For each feature Claude Code depends on that a host lacks, a recommended Fontaine behaviour from a
fixed set: fall back, warn, refuse. Any proposed deviation from Claude Code semantics becomes a
dated line in the master plan's `## Decisions` (`tasks/fontaine_harness_plan.md`).

### Evidence standard

- Every entry carries a source: a docs URL with access date, or an observed request recorded in
  the document's appendix (method, redacted request, response status and relevant fields).
- Live requests are run only where an account is available (the environment has none today). For
  each host without one, the document lists exactly what could not be tried and why. Negative
  findings are recorded like positive ones.
- Credentials never enter the repo or the document; requests in the appendix are redacted.
- No benchmark or scorecard.

### Document layout

`research/research-anthropic-models.md`: summary and the central list first, then the host
sections, the matrix, guardian findings, the breakage table, untried items, and an appendix of
observed requests. The central list's entry format is fixed by the issue: what it is, evidence,
correctness versus optimisation, Azure support, Databricks support, issue fed or filed.

## Implementation Steps

1. Read the sibling-study issues (#154-#156) and the issues named in Current State, and the
   existing research docs, to fix the document's tone and layout. (`research/`)
2. Collect Anthropic's and each host's docs for the Claude wire, auth, beta headers and features;
   record URLs and access dates.
3. Check rig 0.45.0's Anthropic and Azure clients against the matrix to say what Fontaine can
   reach without new code (`~/.cargo/registry/.../rig-core-0.45.0/src/providers/{anthropic,azure}`).
4. Observe Claude Code's behaviour with `claude -p` for the matrix rows that docs leave unclear
   (run manually, never from tests or CI).
5. If an account on either host is available, run real requests per matrix row and record the
   results; otherwise write the untried list.
6. Write `research/research-anthropic-models.md` to the layout above.
7. File follow-up GitHub issues for each central-list entry that needs new work and has no owning
   issue, linking existing issues where one fits; add the issue number to each entry.
8. Add dated `## Decisions` lines to `tasks/fontaine_harness_plan.md` for any proposed deviation
   from Claude Code semantics.

## Files to Modify

- `research/research-anthropic-models.md` (new)
- `tasks/design/research-findings.md` (one line pointing to the new document)
- `tasks/fontaine_harness_plan.md` (`## Decisions`, only if a deviation is proposed)
- `CHANGELOG.md` (one line under Unreleased)

## Trade-offs

- Cover Azure and Databricks equally rather than Azure only: the issue names both, and Databricks
  is the one with no rig client, so skipping it would hide the largest integration gap.
- Docs-only claims are kept but marked, rather than dropped: no account is guaranteed, and a marked
  claim still orders the follow-up work.

## Decisions

- AFK: 2026-10-24: Assume no Azure Foundry or Databricks account is available for live requests (none in the environment; real accounts and spend are the user's call), so the study is docs-based and live-request rows are marked `untested` and listed in the untried section.

## Documentation

`README.md` and `docs/` do not change; research documents live in `research/` and are indexed
by one line pointing to the new document in `tasks/design/research-findings.md` (the master plan's
design-doc list links only `tasks/design/*.md`).

## Testing Strategy

None: the deliverable is a document and filed issues. Reviewability comes from the evidence
standard: every central-list entry cites a source, and every `yes`/`no` in the matrix is backed by
docs or an observed request.
