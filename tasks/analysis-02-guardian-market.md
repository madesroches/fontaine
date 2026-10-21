# Analysis 02 — The Guardian/Guardrail Runtime Market

Date: 2026-10-20. Companion to `tasks/fontaine_harness_plan.md`.

## Scope of this file

Products and frameworks whose job is **adjudicating or filtering what an AI agent may do** —
the category Fontaine's guardian belongs to. Question answered here: *can an existing guardrail
product be plugged into an existing harness, making a guardian-native harness redundant?*

## Segment A — Open-source agent guardrail frameworks

**LlamaFirewall (Meta / Purple Llama)** — the most direct conceptual competitor.
(Deep-dive with primary sources and all numbers: `research-llamafirewall.md`.)
- MIT-licensed (code and examples; the PurpleLlama root license covers model weights only) **in-process Python library** (not a proxy, not
  a harness); used in production at
  Meta. Open-source guardrail system for building secure AI agents; four scanner tiers:
  - **PromptGuard 2** — 86M/22M DeBERTa-family classifiers for explicit jailbreak/injection
    detection (model-card recall @1% FPR **97.5% (English)**, AUC 0.98 EN / 0.995 multilingual;
    19–92ms; weights under the **Llama 4 Community License** — commercial use permitted — with HF
    repos **gated** (license acceptance required); ONNX ports and mirrors exist),
  - **AlignmentCheck** — a few-shot **chain-of-thought auditor** of the agent's reasoning trace:
    a *drift monitor* (halt-the-run), not a pre-execution per-tool-call gate. Experimental;
    needs a big backing model (Llama 4 Maverick / Llama 3.3 70B) and **defaults to a hosted
    Together API**; it remains a prompting method — no small open AlignCheck model was released
    (Meta published only the eval dataset `facebook/llamafirewall-alignmentcheck-evals`, MIT).
    AgentDojo: ASR .176→.029 alone,
    →.0175 combined with PromptGuard 2,
  - **CodeShield** — Semgrep/regex static analysis of generated code (8 languages, 50+ CWEs),
  - regex/custom scanners.
- **But**: it is a **Python middleware library** for agent builders — not a coding harness, no
  permission-mode UX, no deny/escalate-to-human verdicts, no prose intent rules, no tool-call
  choke point shipped for terminal coding agents. Integrations shipped: OpenAI Agents SDK and
  LangChain only. It guards *your* agent runtime; using it with OpenCode/Goose means writing
  that integration yourself.
- **Project health (verified Oct 2026 via commit history)**: maintenance mode since the late-April 2025
  launch (paper posted May 6) — maintenance chores since March 2026 (dep-vuln bumps by `JinpengMiao`;
  lint/type-sync via Meta's `generatedunixname*` fbcode-sync accounts; last functional change Jan 2026;
  last commit Aug 18, 2026). No MCP support, no Rust port, promised future work (distillation, unsafe tool-use
  coverage) not shipped. PromptGuard 2 *the model* became widely-embedded infrastructure;
  LlamaFirewall *the framework* did not become a standard.
- Threat assessment (revised down from first pass): AlignmentCheck + a harness plugin is still
  the assembly path to Fontaine's core value, but Meta has shown no appetite to walk it in 15+
  months. The lasting competitive pressure is **comparison, not substitution**: their AgentDojo
  table is the published baseline Fontaine's eval harness must beat, and their paper's
  anti-proprietary argument (§2.4) is our pitch from Meta's mouth — cite it.

**NVIDIA NeMo Guardrails / Guardrails AI** — programmable rails (dialog + action), rule/flow based
with optional LLM self-check calls. Enterprise-flavored, Python, framework-agnostic. Same shape of
threat: powerful rails engine, zero coding-agent UX. (Commonly cited alongside LlamaFirewall in
open-guardrail roundups, e.g. reddit r/WebAfterAI "Open-source guardrails and agent security:
5 tools".)

**GuardAgent (research)** — arXiv:2406.09187, ICML 2025: LLM guarding an LLM's tool actions;
98%/83% guardrail accuracy on benchmarks. Validates the concept; ships as research code, not a
product. Already cited in the plan.

## Segment B — MCP-boundary enforcement & scanning

**Invariant Guardrails → Snyk (Snyk Labs acquired/partnered; mcp-scan became Snyk Agent Scan)**
- Invariant Guardrails: rule-based, contextual policy enforcement deployed **between the agent and
  the MCP server** — "state-of-the-art guardrailing for MCP and agentic AI."
  (invariantlabs.ai/guardrails; github.com/invariantlabs-ai/invariant)
- Snyk Agent Scan (successor to mcp-scan): open-source Apache-2.0 scanner; discovers MCP configs
  and skills, flags rug-pull/tool-description changes. (labs.snyk.io; pipelab.org/blog/
  best-ai-agent-security-tools-2026; pillar.security enterprise roundup, Oct 2, 2026)
- **Fit with Fontaine**: rule-based (policy language), proxy-position — it cannot express
  "force-push is fine when the user specifically asked for it." Complementary, not equivalent.
  Also noteworthy: Fontaine could itself route MCP traffic through such a scanner.

**Agent Wall** — open-source firewall for MCP-based agents (r/node, 2026). Same niche, younger.

## Segment C — Gateway/proxy-layer guardrails (model-agnostic, harness-agnostic)

**LiteLLM Proxy guardrails** — free guardrail hooks with providers (Lakera, Aporia, LLM Shield
PII-masking, etc.) applied to all traffic through the proxy.
(docs.litellm.ai/docs/guardrail_providers; github.com/BerriAI/litellm/discussions/5321)
**Kong / TrueFoundry / Portkey AI gateways** — same category, enterprise.
(pillar.security/blog/ai-gateway-guardrails-litellm-kong-truefoundry-2026; truefoundry.com/guardrail)
- **Key limitation vs Fontaine**: gateways see **model traffic** (prompts/completions), not
  structured tool-call semantics with workspace state. They can scan text for injection/PII; they
  cannot adjudicate "is *this tool call with these args* consistent with the user's task intent,
  given this repo's remotes and pre-session files." Fontaine's guardian sees `ToolCallSummary`,
  session facts, git trust boundary — context a proxy fundamentally lacks.
- **Key strength vs Fontaine**: works with *every* existing client (Claude Code included, via
  proxy-routed endpoints), zero harness switch.

## Segment D — Enterprise agent security platforms

Lakera (Check Point), Protect AI, HiddenLayer, Striker, Pillar, Prompt Security, etc. — runtime
monitoring, agent-SPM, governance dashboards. (striker.ai blog; knowbe4.com blog;
giskard.ai/knowledge/ai-guardrails-comparison-2026; folio3.com roundup)
- Sales motion is CISO/enterprise, SaaS, per-seat. Not competitors for a dev's terminal, but they
  define the vocabulary ("agent firewall," "runtime enforcement") Fontaine will be compared in.
- PromptArmor is the interesting one: offensive research shop that keeps **publishing bypasses of
  the closed guardians** (Codex auto-review approving malware; Codex connector exfiltration).
  Their existence proves demand for *auditable* adjudication — a closed classifier that approves
  malware is worse than an open one the user can test. (promptarmor.com/resources)

## Segment E — Sandboxing (the deterministic alternative)

- **Anthropic srt / sandbox-runtime** — open source; Seatbelt/bubblewrap + network proxy. Now the
  reference; community gists catalog 10+ options (wincent gist "List of coding agent sandboxes
  2026-05"; michaellivs.com/blog/sandbox-comparison-2026).
- Cymulate's **Configuration-Based Sandbox Escape (CBSE)** research shows sandboxes get escaped too
  (code execution inside the Claude Code sandbox via injection/malicious repo).
  (cymulate.com/blog/the-race-to-ship-ai-tools-left-security-behind-part-1-sandbox-escape/)
- Positioning consequence: sandbox = containment (blast radius), guardian = intent judgment.
  Anthropic runs **both**. Fontaine v1 ships only the guardian and defers sandboxing — the plan
  should be honest that a guardian-only product is half of the industry-converged stack.
  Cheapest credible move: integrate/interoperate with `srt` or bubblewrap post-v1 rather than
  build.

## Market map conclusion

| Layer | Product examples | Sees tool-call intent + repo state? | Coding-agent UX? | Open? |
|---|---|---|---|---|
| Harness-native guardian | Claude Code classifier, Codex reviewer | ✓ | ✓ | ✗ |
| **Harness-native guardian (open)** | **— empty — (Fontaine's cell)** | ✓ | ✓ | ✓ |
| Guardrail framework/library | LlamaFirewall, NeMo, Guardrails AI | partial (needs integration) | ✗ | ✓ |
| MCP boundary | Invariant/Snyk, Agent Wall | MCP calls only, rule-based | ✗ | ✓ |
| Gateway/proxy | LiteLLM, Kong, TrueFoundry + Lakera etc. | ✗ (text traffic) | ✗ | mixed |
| OS sandbox | srt, bubblewrap, microsandbox, E2B | ✗ (deterministic containment) | ✗ | ✓ |
| Enterprise platform | Striker, Pillar, Protect AI… | telemetry-level | ✗ | ✗ |

**Nobody sells the open harness-native guardian.** The nearest substitutes require assembly
(LlamaFirewall + a harness plugin) or operate at a layer that structurally cannot judge tool-call
intent (proxies, sandboxes). That assembly risk is quantified in analysis-04.
