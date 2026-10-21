# Analysis 03 — Differentiation: What Fontaine Is, and Why It's Not Trivial

Date: 2026-10-20. Companion to `tasks/fontaine_harness_plan.md`.

## The one-sentence claim

**Fontaine is the only open, model-agnostic terminal coding agent whose permission engine is an
LLM guardian the user can inspect, configure in prose, run locally, and audit — i.e., Claude Code's
auto mode as an open standard rather than an Anthropic service.**

## Differentiation axes (each verified against analysis-01/02)

### 1. Open guardian vs closed classifier — the auditability argument
- Claude Code's classifier and Codex's reviewer are closed, server-side, tuned on vendor telemetry.
  Users cannot see the prompt, the rules, or the failure modes; they can only trust the 89% number.
- PromptArmor's demonstrations (auto-review approving malicious npm installs; connector data
  exfiltration) show **closed guardians fail silently and unaccountably**.
- Fontaine's guardian is: a versioned `guardian_prompt.md` in the repo, prose rule lists the user
  owns, a verdict schema with `rule` + `explanation` + `confidence` on every decision, and an
  eval harness the user can run against *their* guardian model. **Auditable adjudication is a
  product property no closed classifier can offer** — it's the "Signal vs. SMS" argument applied
  to agent permissions.

### 2. Model-agnostic judgment (including local/air-gapped)
- Claude Code auto mode requires Anthropic's servers; Codex's requires OpenAI's.
- Fontaine runs the guardian on **any rig-supported model** — including Ollama/LM Studio/vLLM.
  An air-gapped or data-sovereignty-constrained team gets *no* LLM-adjudicated permissions from
  either major vendor today. This is a segment with real buyers (regulated industries, defense-
  adjacent, EU public sector) and zero incumbents.

### 3. Prose intent-aware rules vs static allow/deny/ask
- Every open harness (OpenCode, Goose, Claw Code, Crush) gates with **pattern/config lists**.
  Anthropic's own data: humans rubber-stamp 93% of prompts, catch 13.6% of dangerous ones —
  static lists are the same failure mode in config form.
- Prose rules ("force-push allowed only when the user specifically asked for it"; "never send repo
  contents to third-party review APIs") express **intent-conditional policy** that regex cannot,
  and that rule engines (Invariant/Snyk, OPA-style) express only awkwardly. The plan correctly
  adopts Claude Code's proven precedence semantics (hard_deny > soft_deny > allow > specific
  user intent) and trust asymmetry (projects may only tighten).

### 4. Guardian-native architecture vs bolted-on guardrail
- LlamaFirewall et al. are libraries an agent runtime integrates; the integration must be written
  and maintained per harness. Fontaine's choke point is architectural: **every** tool — built-in,
  Fontaine-added, or MCP-imported — is registered wrapped in `GuardedTool`, and a test fails if any
  registered tool lacks a risk class. The gate cannot be forgotten.
- Proxy/gateway guardrails (LiteLLM+Lakera etc.) see text traffic, not `{tool, args, cwd, git
  remotes, pre-session files, prior denials}`. The `AdjudicationRequest` is structurally richer
  than anything a proxy can construct. This is the deepest technical difference: **judgment
  quality is a function of adjudication context**, and only the harness has it.

### 5. The hybrid gate (deterministic fast path + LLM judgment)
- Read-only in-workspace calls skip the guardian (deterministic scope/symlink validation — code,
  not regex-on-args); everything else is adjudicated; fail-closed on every non-verdict outcome
  (error, timeout, unparseable, low-confidence-on-destructive).
- This combination — deterministic where possible, judgment where necessary, denial where uncertain
  — is not shipped as a coherent whole anywhere open. Claude Code has it; it's closed.

### 6. Denial UX as a first-class product surface
- Denial reason fed back to the agent (it adapts), `/denied` retry list, consecutive-denial loop
  breaker, "teach the classifier" moments surfaced through `/permissions`. Open harnesses treat
  denial as a dead-end error; Fontaine treats it as a conversation. This is UX depth, and UX depth
  is why people switch harnesses at all.

## Why it's not trivial (honest engineering assessment)

The loop is easy — Claw Code proved a harness skeleton is weeks of work. The non-trivial 20%:

1. **Adjudication quality is an empirical problem, not a code problem.** The plan's eval harness
   (~150+ golden cases across benign/attack/ambiguous classes) *is* the product. Anthropic tuned
   theirs on fleet telemetry; Fontaine must earn trust via a public, reproducible eval scorecard
   per guardian model. Nobody open-source has published one for tool-call adjudication. First
   credible one wins the category's mindshare.
2. **The guardian is itself an attack surface.** Injection-hardening a judge (no tools, structured
   output only, untrusted-delimiting, verdict validation by code, tool-result exclusion mirroring
   Claude Code's classifier inputs) is subtle; PromptArmor-class bypasses are the expected failure
   mode and the red-team bar is public. Getting this *demonstrably* right is real work with real
   credibility payoff.
3. **Fail-closed correctness under concurrency**: batched tool calls adjudicated in parallel with
   independent tool_results, cache invalidation on rule/version edits, timeout paths, loop
   breakers — the failure modes are interleaved-state bugs, and the plan's cassette-replay CI is
   the right (non-obvious) answer.
4. **Scope validation**: workspace containment with symlink resolution, session-start trust
   boundary (remotes added mid-session untrusted), pre-session file inventory for destruction
   checks — deterministic code that must be right, because the read-only fast path's safety rests
   entirely on it.
5. **Latency economics**: a guardian on the critical path of every risky call. Cache + fast default
   model + read-only skip make it tolerable; making it *feel* instant is craft the closed vendors
   spent a year on.

## What is NOT differentiating (stop claiming these)

- "Rust terminal coding agent" — 167 harnesses; saturated.
- "Works with any model" — OpenCode/Goose already own this claim at massive scale.
- "Built on rig / reuses crates" — an implementation virtue, invisible to users.
- "MCP support" — table stakes.
- Permission-mode names/UX — deliberately cloned from Claude Code (fine for muscle memory, but
  it's parity, not differentiation).

## Positioning statement (draft)

> Every closed agent has a secret judge. Fontaine gives you the judge: an open, model-agnostic
> guardian that reads your task intent and your prose rules, explains every denial, fails closed,
> and runs on hardware you control — with a public eval scorecard you can verify yourself.

**The moat is thin but real, and it is three things**: (1) the published eval/scorecard + prompt
asset (community-standard potential), (2) the architectural choke point done rigorously, (3) being
first open in the cell. None of these survive passive maintenance; all of them compound if the
eval harness becomes the thing other projects adopt to *test their own* guardians. See analysis-05.
