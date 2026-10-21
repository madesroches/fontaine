# Research — LlamaFirewall (full deep-dive)

Date: 2026-10-20. Fills the gap flagged in `analysis-05` amendment A2 ("the plan cites
GuardAgent/CaMeL/AgentDojo but never mentions LlamaFirewall").
Primary sources: arXiv:2505.03574 (the paper), github.com/meta-llama/PurpleLlama/tree/main/LlamaFirewall
(README + LICENSE + commit history via GitHub API), meta-llama.github.io/PurpleLlama/LlamaFirewall
(official docs), HuggingFace model pages.

## 1. Executive summary

LlamaFirewall is Meta's **open-source (MIT), in-process Python guardrail framework** for LLM
agents, launched May 2025, used in production at Meta. It is a **policy engine orchestrating
four kinds of scanners** over messages/traces: PromptGuard 2 (injection classifier),
AlignmentCheck (LLM chain-of-thought auditor — the component closest to Fontaine's guardian),
CodeShield (static analysis of generated code), and regex/custom scanners.

**Bottom line for Fontaine**: it is the strongest open prior art and the benchmark baseline we
will be compared against — but it is a *detector library for agent builders*, not a
*permission engine inside a harness*, and the project has been in **maintenance mode since
launch** (see §6). It complements Fontaine more than it competes with it: adopt its models and
its AgentDojo numbers; the harness-native adjudication gate remains unclaimed.

## 2. Identity and positioning

- Repo: `meta-llama/PurpleLlama` → `LlamaFirewall/` (pip package `llamafirewall`, Python ≥3.10).
- **Code license: MIT** (verified from LICENSE file). Model weights are separate and
  **Llama Community-licensed + gated on HuggingFace** (verified: `meta-llama/Llama-Firewall-AlignCheck-8B`
  returns 401 without access grant; PromptGuard 2 models require HF access request;
  `Llama-Prompt-Guard-2-86M/22M` updated Apr 29, 2025).
- Paper: Chennabasappa, Nikolaidis, Song, Molnar, … Saxe (Meta AI Security), arXiv:2505.03574,
  May 2025. Explicit framing: "a real-time guardrail monitor to serve as a **final layer of
  defense**"; analogized to **Snort/Zeek/Sigma** — a shared, community-extensible security
  foundation, not a product.
- Design stance vs proprietary systems (paper §2.4): provider-embedded safety (OpenAI moderation,
  Constitutional AI) offers "limited visibility, auditability, and customizability" — *the exact
  same argument Fontaine makes against Claude Code's closed classifier.* We should cite them.

## 3. Architecture

A `LlamaFirewall` object is a **policy engine**: scanners are registered **per message role**
(`Role.USER`, `Role.ASSISTANT`, tool messages), and each scan returns:

```python
ScanResult(decision=ALLOW|BLOCK, reason="prompt_guard"|"agent_alignment"|..., score=0.95)
```

- `scan(message)` — single message.
- `scan_replay(trace)` — full conversation trace scanned incrementally; a trace is flagged if
  any scanner fires at any point (this is also how they simulate real-time deployment for evals).
- Custom scanners: any developer "who can write a regular expression or an LLM prompt" can add
  one (docs: Adding a Custom Scanner).
- Integration examples shipped: **OpenAI Agents SDK guardrails** (`demo_openai_guardrails.py`)
  and **LangChain/LangGraph** (`langchain_agent.py`). No MCP integration, no terminal coding
  agent integration, **no Rust bindings**, not a proxy — it lives in-process in *your* runtime.
- Model logistics: scanners auto-download guard models from HuggingFace (`llamafirewall configure`
  helper); **AlignmentCheck by default requires `TOGETHER_API_KEY`** (hosted Llama 4 Maverick) —
  the flagship reasoning scanner is cloud-dependent out of the box.

## 4. The scanners in detail (with the numbers)

### 4.1 PromptGuard 2 — injection classifier
- BERT-family: **mDeBERTa-base 86M** (multilingual) and **DeBERTa-xsmall 22M** (English, low
  latency). Runs on CPU or GPU; designed for real-time inline scanning.
- Trained only on *explicit jailbreak* patterns (instruction overrides, role reassignment, token
  injection); energy-based loss for OOD precision; tokenization fix against fragmented-token /
  whitespace adversarial evasion.
- Direct-jailbreak benchmarks (paper Table): 86M — AUC .98 EN / .995 multilingual,
  **recall 97.5% @ 1% FPR**, 92.4ms per 512-token classification (A100). 22M — AUC .995 EN,
  recall 88.7% @1% FPR, **19.3ms**. (v1 comparison: recall 21.2% @1% FPR — the improvement is
  mostly false-positive reduction.)
- AgentDojo: as sole defense, ASR .176 → **.075** at ~3% utility cost.
- Known limit (and our plan already cites the research): specialized detectors like this catch
  ~0–2% of *realistic agent* injections beyond explicit jailbreak phrasing; it's a cheap first
  layer, not a judge.

### 4.2 AlignmentCheck — the guardian-shaped one
- **Few-shot chain-of-thought auditor**: compares the agent's reasoning/action sequence against
  the user's stated objective; flags goal hijacking, injection-induced drift, misleading tool
  output. First open-source guardrail to audit an LLM's chain of thought in real time for
  injection defense (their claim).
- Scans **assistant-role messages** (reasoning + tool invocations + plans), i.e. it audits the
  trace, incrementally — it is a **drift detector**, not a pre-execution per-tool-call gate.
- Backing model matters enormously (paper Appendix C ablation): with **Llama 4 Maverick or
  Llama 3.3 70B** it reaches **>80% recall at <4% FPR** on Meta's in-house goal-hijacking
  benchmark; smaller models degrade sharply. A **fine-tuned `Llama-Firewall-AlignCheck-8B`**
  was released (gated, Llama license).
- AgentDojo: AlignmentCheck alone ASR .176 → **.029** (−84%) with utility .477 → .431;
  **combined with PromptGuard 2: ASR → .0175 (−90%), utility .427**. Beats Spotlighting,
  paraphrasing, and CaMeL-style defenses on this benchmark (paper §4.3.2).
- Status in docs/paper: explicitly **"experimental"**; latency is acknowledged as the production
  problem (semantic reasoning overhead); planned future work includes distillation.

### 4.3 CodeShield — static analysis of generated code
- Semgrep + regex rules, 8 languages, 50+ CWEs. Two-tier: ~60ms pattern tier resolves ~90% of
  inputs; escalation tier ~300ms. CyberSecEval3: **precision 96% / recall 79%** on insecure
  code completions.
- Origin: released with Llama 3, folded into LlamaFirewall.

### 4.4 Regex + custom scanners
- Configurable signature matching / simple LLM-prompt scanners — the "Sigma rules" tier.

## 5. LlamaFirewall vs Fontaine's guardian — the real differences

| Dimension | LlamaFirewall | Fontaine guardian |
|---|---|---|
| What it is | detector library inside *your* agent runtime | permission engine inside *our* harness |
| Deployment point | scanners on message roles, trace streaming | `GuardedTool` choke point: **before execution** of every non-read-only tool call |
| Decision | ALLOW / BLOCK + score | Allow / **Deny / Escalate-to-human** + rule label + explanation fed back to the agent + confidence thresholding |
| Policy language | regex + few-shot prompts + custom scanners | prose rule hierarchy (environment / hard_deny / soft_deny / allow), precedence semantics, trust asymmetry (projects can only tighten) |
| Adjudication context | messages/trace | `AdjudicationRequest`: task intent, FONTAINE.md, tool args, cwd, **git remotes at session start, pre-session file inventory, prior denials** |
| Deny UX | none (library returns a verdict) | inline `[Rule]` + explanation, `/denied` retry list, loop breaker, "teach the rules" flow |
| Fail-closed | not a framework concept | every non-verdict outcome = Deny (code, not LLM) |
| Judge model | default **Together-hosted** Llama 4 Maverick; gated 8B fine-tune; small models degrade | any rig provider incl. **fully local** Ollama/vLLM, independently configurable |
| Language | Python, in-process | Rust; harness-native |
| Modes | none | manual / acceptEdits / plan / auto / dontAsk / bypass |

**Conceptual takeaway**: AlignmentCheck answers *"has this agent drifted from the user's goal?"*
(monitoring). Fontaine's guardian answers *"may this specific action run, under these rules, in
this environment?"* (authorization). Monitoring can halt a trace; only a pre-execution gate can
deny one tool call, explain why to the agent, and let the session continue adapted. Different
job — but the public will conflate them, so the plan must articulate this crisply.

## 6. Project health (verified via GitHub API, Oct 2026)

- Commit history on `LlamaFirewall/` since ~Mar 2026: **exclusively `meta-codesync[bot]` chores**
  — dependency vuln bumps (postcss, lodash, uuid, minimatch…), lint cleanups ("Remove unused type
  error suppressions", "Enable Pyrefly"). Last substantive feature work dates to the 2025 launch.
- No MCP scanner, no Rust port, no coding-agent integration shipped in 15+ months. The paper's
  own future-work list (multimodal, distillation, broader threat coverage incl. **unsafe
  tool-use**, better agent benchmarks) has not materialized publicly.
- AlignmentCheck still labeled experimental; still defaults to a hosted API.
- Ecosystem uptake is real but third-party: ONNX ports (`shisa-ai/promptguard2-onnx`,
  `gravitee-io/*-onnx`), ungated mirrors (`project-free-llama/Prompt-Guard-2-86M`),
  Mozilla.ai `any-guardrail` wraps PromptGuard 2, gateway products (Gravitee) embed it.
  **PromptGuard 2 has become infrastructure; LlamaFirewall-the-framework has not.**

Implication for `analysis-04` KILL #5: the "LlamaFirewall becomes the standard integration"
scenario is **weaker than feared** — Meta shipped components, not a standard, and stopped
investing. The residual risk is the *models* becoming the default scanners everyone embeds
(which we should embrace, not fight), and Meta reviving the framework if agent security becomes
a Llama-platform priority.

## 7. Concrete recommendations for the Fontaine plan

1. **Plan research section**: add a LlamaFirewall subsection next to GuardAgent/CaMeL. Frame:
   same insight (reasoning-model oversight beats lexical filters — their AgentDojo table is the
   best public evidence), different seat (library/monitoring vs harness/authorization).
   Cite their anti-proprietary argument (§2.4 of the paper) — it *is* our pitch, from Meta.
2. **Adopt PromptGuard 2 as the default Phase-6 tool-result scanner** (replaces the hand-rolled
   "second cheap-model pass"): 19–92ms CPU/GPU latency, 97.5% recall @1% FPR on explicit
   jailbreaks, free. Rust options, in order of pragmatism: (a) ONNX Runtime via the `ort` crate
   with the community ONNX ports or our own export; (b) tiny sidecar Python process; (c) keep the
   cheap-LLM pass as fallback. Note the **weight-license wrinkle** for the air-gapped "fully
   open" story: Meta weights are Llama-Community-licensed and gated — mirror/ONNX ports exist
   but the cleanest claim is "bring your own scanner model; PromptGuard 2 supported if licensed".
3. **Eval alignment**: report Fontaine guardian results in AgentDojo-comparable terms
   (ASR reduction × utility cost) alongside our golden dataset, using their table
   (no-defense .176 / PromptGuard .075 / AlignmentCheck .029 / combined .0175) as the published
   baseline to beat. This is the cheapest credibility transfer available to us.
4. **Prompt mining (legally clean)**: LlamaFirewall code is MIT — their AlignmentCheck
   system/user prompts (paper Appendix C.6) and few-shot examples are reusable prior art for
   `guardian_prompt.md` hardening, with attribution.
5. **Don't**: depend on the `llamafirewall` Python package at runtime (maintenance-mode,
   Python-in-Rust-harness, Together-API default). Depend on its *models and numbers*, not its
   framework.

## 8. Source index

- Paper: arxiv.org/abs/2505.03574 (v1, 2025-05-06; CC BY 4.0) — benchmarks: §4.1–4.4, Tables 2–3.
- Docs: meta-llama.github.io/PurpleLlama/LlamaFirewall (About; AlignmentCheck scanner page;
  Getting Started — Together API requirement, HF model download flow, OpenAI/LangChain examples).
- Repo: github.com/meta-llama/PurpleLlama/tree/main/LlamaFirewall (README; MIT LICENSE;
  commit history via api.github.com — maintenance-mode evidence, latest commits 2026-08-18).
- Models: huggingface.co/meta-llama/Llama-Prompt-Guard-2-86M and -22M (gated, updated Apr 2025);
  meta-llama/Llama-Firewall-AlignCheck-8B (gated — 401 verified);
  mirrors: project-free-llama/Prompt-Guard-2-86M, shisa-ai/promptguard2-onnx,
  gravitee-io/Llama-Prompt-Guard-2-*-onnx; wrapper: docs.mozilla.ai/any-guardrail.
- Blog: ai.meta.com/blog/ai-defenders-program-llama-protection-tools/ (launch, "AI Defenders").
