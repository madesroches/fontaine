# Analysis 04 — Devil's Advocate: What Already Exists That Makes Fontaine Useless

Date: 2026-10-20. Companion to `tasks/fontaine_harness_plan.md`.
Rule of this file: argue **against** the project. Every kill scenario below is grounded in a
shipping product or a demonstrated fact, not speculation.

## Kill list, ordered by severity

### KILL #1 — OpenCode + a guardian plugin (probability: high, timeline: months)
OpenCode: 209k stars, any model, terminal-first, plugin system already used for permission
automation (`opencode-allowlist` auto-approves file access from config; community Claude Code
plugins already advertise "open source risk classifier + guardrails" — see r/ClaudeAI auto-mode
thread). Its permission layer (`allow/deny/ask` + policies) is exactly the seam a
"guardian-as-adjudicator" plugin slots into, and its community is enormous and moves fast.
- **If OpenCode ships an LLM-guardian permission backend** (first-party or popular plugin),
  Fontaine's entire value proposition becomes an OpenCode feature. Users do not switch harnesses
  for one feature; they enable a plugin.
- Same story for **Goose** (Block-backed, Apache-2.0, extension architecture) and **Claw Code**
  (a community whose explicit purpose is cloning Claude Code — auto mode is the most famous
  un-cloned feature left, and cloning it is that project's obvious next milestone).
- Fontaine's answer must be: be the *reference implementation and eval standard* so that when
  those plugins ship, they're compared against Fontaine's scorecard — or be so good they adopt
  the prompt/rules assets. There is no answer that involves being merely one more option.

### KILL #2 — The closed platforms already did it, better, for free (probability: certain — it's shipping)
Claude Code auto mode is the **default** for Pro/Max/Team since Aug 2026; Codex `--approve-for-me`
ships free for ChatGPT sign-ins. Both are tuned on vendor fleet telemetry Fontaine will never have.
- The pragmatic developer's math: "I already pay for Claude Code/ChatGPT; the guardian is built
  in; why install a third harness?" The 89%-vs-13.6% number is *their* marketing asset, and
  Fontaine can only aspire to it with a self-published eval on someone else's model.
- Fontaine's actual market narrows to: non-Anthropic/non-OpenAI model users, local/air-gapped
  users, and people who don't trust a closed judge. That is a real but **small** slice of the
  167-harness crowd — and PromptArmor-style users may not be *reassured* by an open judge either
  (see KILL #3).

### KILL #3 — Guardian agents are demonstrably breakable (probability: demonstrated)
PromptArmor: Codex's auto-review agent **approved a malicious npm install with elevated privileges
via indirect prompt injection**, and Codex exfiltrated connector data despite the reviewer.
Anthropic's own auto-mode rollout is accompanied by community reports of classifier failures and a
44KB unpublishable rulebook (highflame.com). Meanwhile the plan's own research (AgentDojo et al.)
concedes injection detectors transfer poorly and specialized detectors catch ~0–2% of realistic
injections.
- The devil's-advocate reading: **the LLM-guardian category may be a dead end**, and the industry
  learns this publicly within a year. If a well-resourced red team publishes "Fontaine guardian
  approves `curl|bash` with one crafted README," the project isn't embarrassed — it's *finished*,
  because its only claim is judgment quality. Fail-closed design and untrusted-delimiting mitigate,
  but the plan's own cited literature says general reasoning guardians are attackable, and
  Fontaine ships the attack surface (a known prompt, known rules) to adversaries for free.
- Counter-move required: treat the eval harness + public red-team program as launch-critical,
  not Phase 6.

### KILL #4 — Sandboxing ate the permission problem (probability: medium-high, trending)
Anthropic **open-sourced sandbox-runtime (srt)**: Seatbelt/bubblewrap + network proxy; Codex has
its own; community catalogs list 10+ drop-in agent sandboxes (microsandbox, Docker MCP gateway,
E2B, Daytona…). The converged industry stack is *deterministic containment + egress allowlists*,
with the LLM judge as a nicety on top.
- Deterministic containment doesn't hallucinate, doesn't cost tokens per tool call, adds no
  latency, and can't be prompt-injected. A team that wraps OpenCode in `srt`/bubblewrap with an
  egress proxy gets **most** of Fontaine's safety story with **zero** trust in an LLM judge.
- Worse: Fontaine v1 explicitly defers sandboxing. So the honest comparison is
  "guardian-only Fontaine" vs "sandbox-wrapped OpenCode" — and for exfiltration (the scariest
  class) the sandbox wins: guardian judges *intent*, egress proxies enforce *destinations*.
  Cymulate's CBSE escapes show sandboxes fail too, but they fail into a contained blast radius.
- If Fontaine never ships sandbox interop, it stays half a stack.

### KILL #5 — LlamaFirewall becomes the standard integration (probability: medium)
Meta ships an open, model-agnostic, brand-strong guardrail system (PromptGuard 2 + AlignmentCheck
+ CodeShield) explicitly designed as the final defense layer for *any* agent. AlignmentCheck is
structurally the same idea as the guardian (audit agent actions against user goal).
- If harnesses integrate LlamaFirewall (Python-side or via ports), "add guardrails to your agent"
  is answered by Meta's framework, and Fontaine becomes a hand-rolled duplicate of a
  corporate-backed standard with worse benchmarks-by-default (LlamaFirewall ships fine-tuned
  classifiers; Fontaine ships a prompt).
- The plan cites GuardAgent/CaMeL but **does not mention LlamaFirewall anywhere** — a gap. It
  should at minimum be evaluated as the default result-scanner and as prior art to differentiate
  against.

### KILL #6 — Gateway guardrails cover the compliance buyers (probability: medium)
LiteLLM guardrails (free, with Lakera/Aporia/PII-masking), Kong/TrueFoundry/Portkey gateways, and
enterprise platforms (Striker, Pillar, Protect AI) already sell "model-agnostic AI firewall" to
the exact buyer Fontaine's air-gapped/regulated story targets — with dashboards, audit logs, and
procurement paperwork. An enterprise that wants governed agents buys a gateway, not a Rust CLI.
Fontaine's local-guardian story doesn't reach the budget holder.

### KILL #7 — The models absorb the feature (probability: slow-burn, high impact)
Frontier models keep internalizing safety: better refusal calibration, provider-side moderation,
and (per Anthropic's engineering) classifiers trained on fleet data. Each model generation makes
"the harness must judge the model" less necessary. Terminal harnesses are also being absorbed
upward — Claude Code/Codex desktops, ChatGPT "Approve for me" UX. If, in 18 months, running any
decent model in any harness with a sandbox is *boringly safe*, the guardian is a solution whose
problem quietly dissolved.

### KILL #8 — rig-dependency risk is a product risk, not just an engineering one
The plan stakes the core on a **pre-1.0 framework with breaking changes every release** (rig 0.45).
rig itself ships `rig-harness`/`rig-tui` — a Bevy-based coding agent. If rig's own harness adds a
guardian/permission story (it's their obvious roadmap too, and the plan admits rig now has
"harness-grade crates"), Fontaine is outrun **by its own foundation**, which can wire
hooks/agent/extractor natively and faster. The plan's mitigation (don't build on rig-harness)
doesn't stop rig-harness from building on rig's momentum.

## Smaller objections (real, non-fatal)

- **Latency tax**: a fast-model round trip on every risky call, forever. Claude Code spent a year
  making theirs feel instant; users benchmark against that feel, not against manual prompts.
- **Cost**: guardian tokens on local models are "free" but slow; on hosted models they're a
  second bill for a feature the closed harnesses include free.
- **Config burden**: prose rule lists are a *new skill*. Claude Code users report the classifier
  needs teaching; Fontaine users must write environment/hard_deny/soft_deny prose before auto mode
  is trustworthy. Default lists mitigate; the "44KB rulebook you can't print" problem (highflame)
  shows where this road ends.
- **The plan's Open Question #2 admits it**: with no guardian model configured, auto mode is
  either slow (main model) or unusable. First-run experience decides adoption, and this one is
  currently unresolved.
- **Name/category confusion**: "guardian agent" overlaps Striker's "Guardian Agents" enterprise
  product and generic AI-security marketing. SEO/discoverability will be a fight.

## What survives the kill list (steelman, briefly)

No kill is total: KILL #1–2 leave the open/local/auditable niche unclaimed *today*; KILL #3 argues
for shipping evals first, not for not shipping; KILL #4–6 are layer-confusion arguments (gateway ≠
tool-call intent; sandbox ≠ judgment; LlamaFirewall ≠ harness); KILL #7–8 are timeline bets.
The honest synthesis is in analysis-05: the niche is real, narrow, and defended only by speed,
published evals, and becoming the *standard* the others integrate — never by existing.
