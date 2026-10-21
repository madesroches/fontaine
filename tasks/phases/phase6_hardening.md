# Phase 6 — Guardian hardening: tool-result scanner

**Status:** not started · **Depends on:** [Phase 4](phase4_guardian.md) (evals already public;
Phase 5 for the subagent-result path) · **Master:** [fontaine_harness_plan.md](../fontaine_harness_plan.md)

## Design (excerpt owned by this phase)

### Result scanning (the tool-output side of the guardian)

Like Claude Code's server-side probe, a scanner pass over incoming tool *results* that flags
injection-looking content ("ignore previous instructions…", credential-shaped strings) and
annotates the result before the main agent reads it. Default scanner: **PromptGuard 2 86M via
ONNX (`ort` crate)** (per the LlamaFirewall findings — user decision 2026-10-21); the cheap-LLM
pass is the fallback / BYO option. PromptGuard 2's HF downloads are gated and Llama 4 Community
License-encumbered — that caveat is surfaced at first enable; a BYO-scanner interface serves
license-clean setups. Off by default for latency; configurable.

Context for the choice: `research/research-llamafirewall.md` §7.2 (PG2 recall @ 1% FPR **97.5%**
English; 19–92 ms — a cheap first layer, not a judge; AgentDojo evidence that small detectors
catch little beyond explicit jailbreak phrasing, which is why this scans results *for the main
agent's benefit* and never substitutes for the guardian). Subagent reports flow through this same
path — see the Phase 5 plan.

## Steps

1. Tool-result injection scanner: default scanner is **PromptGuard 2 86M via ONNX (`ort` crate)**
   (a sidecar process and the cheap-LLM pass are the fallback / BYO options), off by default,
   configurable (`[guardian] scan_tool_results`). Surface the gated-download / Llama 4 Community
   License caveat at first enable; expose a BYO-scanner interface for license-clean setups.
2. Docs — result scanning in `docs/guardian.md`: what the scanner flags, how the annotation
   reaches the agent, why off-by-default (latency), and that scanned results carry into the
   subagent-report path; write the gated-download / Llama 4 Community License caveat up beside
   the in-app first-enable notice; `docs/configuration.md` for `scan_tool_results`, the BYO
   scanner interface, and the cheap-LLM fallback; append a CHANGELOG entry.

## Composite guardian — roadmap direction (Phase 6+)

Not a build item in this phase. The tier-tree direction (typed-decision screen tier, per-domain
policy subtrees, strictest-wins composition, benchmarked-tiers-only gating) is specified in the
master plan's §Composite guardian; v1's shipped spine — deterministic tiers + single LLM guardian
+ this phase's optional PG2 result scanner — is already the 3-deep tree that section describes.

## Files created or modified

`rust/fontaine-guardian/src/scanner.rs`; `[guardian] scan_tool_results` config plumbing
(`rust/fontaine-core/src/config.rs`); `docs/{guardian,configuration}.md`.

## Tests owned by this phase

- Scanner unit tests with fixture payloads (injection-looking text and credential shapes flagged;
  benign results unflagged; annotation present before the agent reads the result).
- BYO-scanner interface: a test scanner plugs in via the trait; default-off means zero behavior
  change until enabled.
- Subagent reports (ordinary tool results from Phase 5) pass the scanner when enabled: an
  injection-bearing report is flagged before the parent reads it.
- License-notice path: first-enable notice fires exactly once per model download.

## Definition of done

- Scanning demonstrable in a live session behind the config flag, with the license caveat
  surfaced; off remains the default.
- `docs/guardian.md` scanning section complete.
- CHANGELOG entry appended.
- Log the phase in the master plan's Progress log.
