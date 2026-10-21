# Analysis 05 — Strategic Verdict and Recommendations

Date: 2026-10-20. Synthesis of the four analysis docs (`research/analysis-*.md`), applied to
`tasks/fontaine_harness_plan.md`.

## Verdict

**Proceed — but re-scoped.** The plan targets a genuinely empty cell (open × any-model ×
harness-native LLM guardian × auditable prose rules): no product occupies it as of Oct 2026.
However, the cell is narrow, undefended by architecture alone, and the analysis-devils-advocate kill list shows
the window is measured in **months, not years**. Fontaine wins only under one strategy:
**be the open reference implementation and eval standard for guardian-adjudicated tool calls**,
not "another Rust coding agent with a cool feature."

Scorecard:

| Question | Answer |
|---|---|
| Is the market validated? | Yes — Anthropic and OpenAI both shipped LLM adjudication as default/flag in Aug 2026 |
| Is the open cell empty? | Yes — no open, locally-runnable, prose-configurable, per-verdict-explaining judge exists (Goose `smart_approve` is best-effort LLM read/write classification: no prose rules, no per-verdict explanation, no fail-closed gate; Codex Guardian's reviewer code + policy are open in-tree, but the reviewer model is closed/OpenAI-only and never runs locally) |
| Is the cell defensible long-term? | No — one OpenCode/Goose/claw-code-lineage plugin fills it (successors LazyCodex / Gajae-Code) |
| Is the differentiation non-trivial? | Partially — choke-point architecture + eval harness are real work; the harness itself is commodity |
| Biggest existential risk | KILL #1 (absorption as a plugin) and KILL #3 (public guardian bypass before trust is established) |
| Recommendation | Build, with the five plan amendments below; adopt kill criteria |

## Required plan amendments (concrete, in priority order)

### A1. Promote the eval harness from Phase 6 to Phase 0/1 — it is the product
The plan currently lands evals last. Invert this: the **public guardian scorecard** (catch rate
per attack class × false-deny rate on benign commands × per-model results) is the only asset that
(a) differentiates from closed classifiers, (b) survives being cloned, (c) makes Fontaine the
thing other harnesses' guardian plugins get compared against. Ship the golden dataset + runner
before or alongside the TUI; publish results per guardian model; invite red-team PRs to the
dataset. This is also the direct mitigation for KILL #3: a bypass discovered *by Fontaine's own
public eval* is a credibility event; the same bypass discovered by PromptArmor is a kill event.

### A2. Answer LlamaFirewall explicitly (plan gap) — RESOLVED by `research-llamafirewall.md`
The plan's research section cited GuardAgent/CaMeL/AgentDojo but never mentioned Meta's
LlamaFirewall — the most-cited open guardrail system and the nearest conceptual prior art.
Full deep-dive completed (primary sources: arXiv:2505.03574, repo + commit history, HF models);
fold these into the plan:
- Add a research subsection positioning against PromptGuard 2 / AlignmentCheck / CodeShield:
  AlignmentCheck is a trace **drift monitor** (experimental, hosted-Together-API default,
  commits predominantly maintenance chores since ~March 2026 — launched late April 2025, paper May 6) — Fontaine's guardian is a pre-execution
  **authorization gate** with deny/escalate verdicts and prose intent rules. Different seat;
  cite their paper's anti-proprietary-guardrails argument (§2.4) — it is our pitch from Meta.
- Adopt **PromptGuard 2** as the default tool-result scanner (Phase 6, hardening) via ONNX Runtime
  (`ort` crate) or a sidecar, instead of a hand-rolled cheap-model pass: recall @1% FPR
  97.5% (English) per model card + paper §4.1; AUC .98 EN per paper (card prints .998) / .995 multilingual, 19–92ms.
  Caveat to document: PromptGuard 2 weights are **Llama-4-Community-licensed** (commercial use
  permitted; the HF repos *are* gated — license acceptance required) — the air-gapped
  "fully open" story must surface that at first enable and keep a "bring your own scanner model"
  interface (Llama Guard 4 is under the same license — not Apache 2.0 — and too heavy for
  hot-path scanning; ONNX ports and mirrors exist but don't change the license).
- Report Fontaine's eval results in **AgentDojo-comparable terms** (ASR × utility) against their
  published baseline: no-defense .176 / PromptGuard .075 / AlignmentCheck .029 / combined .0175.
- Mine their AlignmentCheck prompts (paper Appendix C.6, CC BY 4.0; the repo code+examples are
  MIT) as attributed prior art for `guardian_prompt.md`.
- Do **not** take a runtime dependency on the `llamafirewall` Python package (maintenance
  chores since ~March 2026 — a commit-history reading, not a Meta declaration —, Python-in-Rust, hosted-API default). Depend on its models and numbers, not its framework.

### A3. Add sandbox interop to the roadmap (not v1 build, but v1.1 slot)
Guardian-only is half the converged stack (KILL #4). Cheapest credible move: a documented
integration path with Anthropic's open `sandbox-runtime`/bubblewrap — run the agent process
contained, guardian judging intent inside the containment. Even a `docs/sandboxing.md` + config
flag that launches under `srt`/`bwrap` converts "half a stack" into "defense in depth, open on
both layers." Explicitly frame: sandbox = blast radius, guardian = intent, egress proxy =
destinations; Fontaine orchestrates all three, owns one.

### A4. Decide the rig-harness contingency now
KILL #8: rig ships its own harness/TUI and is the natural home for a first-party guardian. Add a
decision trigger to the plan: if rig-agent/rig-harness ships a permission-adjudication API,
Fontaine either (a) upstreams the guardian into rig and refocuses on the TUI + evals + rules
standard, or (b) forks the seam. Option (a) is likely the *win*: "the guardian that rig adopted"
is exactly the reference-implementation strategy. Keep `fontaine-guardian` dependency-light so
it can be lifted out of the harness and embedded elsewhere (including into OpenCode/Goose via
adapters — a deliberate library-ization, turning KILL #1 into a distribution channel:
*fontaine-guardian as a crate/MCP-adjudicator other harnesses can embed*).

### A5. Name the niche and build for it
Don't market "a Rust Claude Code alternative" (saturated, KILL #2). Market the three segments the
closed guardians structurally cannot serve:
1. **Local/air-gapped & data-sovereignty users** — Ollama/vLLM agent *and* guardian, nothing
   leaves the machine. Zero incumbents.
2. **Non-Anthropic/non-OpenAI model users** — Gemini, OpenRouter, open-weight models with real
   agent permissions.
3. **Auditors & the trust-but-verify crowd** — the PromptArmor readership: people who want the
   judge's prompt, rules, verdicts, and scorecard in writing.
Quickstart, README, and demo should lead with #1 (it's the only one with no alternative at all).

## Kill criteria (pre-committed, to avoid sunk-cost drift)

Abandon or pivot fontaine-the-harness (keeping fontaine-guardian-the-library) if any of:
- OpenCode, Goose, or the claw-code lineage's active successors (LazyCodex / Gajae-Code) ship a
  first-party LLM-guardian permission backend **with
  published evals** before Fontaine's eval harness is public.
- A public red-team bypass of Fontaine's guardian cannot be closed within one release cycle
  (the category's trust bar, per KILL #3).
- rig adopts a permission-adjudication API and refuses upstream collaboration (KILL #8 becomes
  fatal rather than convertible).
- Eval results show the best local guardian models cannot clear a credible bar (e.g. <60% catch
  on the seeded attack classes at <10% benign false-deny) — meaning the "any model" promise is
  hollow for the feature that matters.

## Sequencing summary (what changes in the plan)

| Plan phase | Amendment |
|---|---|
| Phase 0 | + eval golden-dataset scaffolding & CI runner skeleton (A1) |
| Phase 1 | + rig spike unchanged (A2 landed in `tasks/design/research-findings.md` Research Findings + Phase 6) |
| Phase 4 | guardian ships **with** its eval scorecard, published (A1) |
| Phase 5 | + sandbox interop spike: run under srt/bubblewrap (A3); + guardian crate extraction seam (A4) |
| Phase 6 | result-scanner default = PromptGuard 2 (A2); evals already public by now, not starting here |

## Bottom line

The plan's engineering is sound and its central bet — that intent judgment beats static lists,
proven at fleet scale by Anthropic's 89%/13.6% — is correct. The commercial risk is not that
Fontaine can't be built; it's that it can be **plugin-ized by larger ecosystems before it
establishes itself as the standard**. Every recommendation above funnels into one defense:
publish the judge (prompt + rules + verdicts + scorecards), make the judge embeddable, and pair
it with open containment. "The open guardian standard" is a position a plugin can't take from
you; "a harness with auto mode" is a position anyone can take in a weekend.
