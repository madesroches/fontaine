# Analysis 01 — Competitive Landscape: Coding Agent Harnesses

Date: 2026-10-20. Companion to `tasks/fontaine_harness_plan.md`. Sources: web research, links inline.

## Scope of this file

Who competes with Fontaine **as a terminal AI coding agent**, and what their permission/safety
story is. The guardrail-runtime market (LlamaFirewall, proxies, MCP scanners) is covered in
`analysis-guardian-market`; that market is where the "does this make Fontaine useless" threat actually lives.

## The field (Oct 2026)

### Tier 1 — Closed platform harnesses (the feature originators)

**Claude Code (Anthropic)** — the product Fontaine's signature feature replicates.
- Auto mode is now the **default**: the built-in starting mode for interactive sessions on every
  plan (since v2.1.283; initially Pro/Max/Team from Aug 14, 2026). A server-side
  classifier reviews every action beyond read-only tools and in-project file
  edits; Anthropic reports **89% of dangerous commands
  caught vs 13.6% for human reviewers** (study of 1,053 paid testers).
  (anthropic.com/engineering/claude-code-auto-mode; explainx.ai/blog/claude-code-auto-mode-default-pro-max-team-august-2026)
- Six permission modes, prose-based classifier config (environment / hard_deny / soft_deny /
  allow), fail-closed semantics, denial-reason feedback loop. (code.claude.com/docs/en/permission-modes)
- Also publishes an **OS sandbox engine** (`@anthropic-ai/sandbox-runtime`, "srt") — *extracted
  from Claude Code's internal implementation* and available as a standalone open-source library;
  Claude Code's own isolation is built on Seatbelt (macOS) / bubblewrap (Linux) plus a network
  proxy — defense-in-depth underneath the classifier.
  (anthropic.com/engineering/claude-code-sandboxing; simonwillison.net/2026/May/30/how-we-contain-claude/)
- Limits for our purposes: classifier is **closed, server-side, Anthropic-models-only**. You cannot
  inspect, extend, or run it locally. That is precisely Fontaine's opening.

**Codex CLI (OpenAI)** — `--approve-for-me` surfaced the pattern at the CLI the same week Claude Code announced its default flip (v0.147.0, Aug 7, 2026); OpenAI had already released Auto-review in Codex in late April 2026 (alignment.openai.com/auto-review, Apr 30, 2026: "Last week, we released Auto-review in Codex").
- `--approve-for-me` (v0.147.0, Aug 7, 2026): a **separate reviewer agent** answers approval
  requests at the sandbox boundary. "Auto-review replaces manual approval at the sandbox boundary
  with a separate reviewer agent." (codex-docs.com/en/docs/sandboxing/auto-review;
  alignment.openai.com/auto-review/)
- Free for ChatGPT sign-ins (help.openai.com — auto-review checks are free and don't count toward
  plan usage); "stop for human approval roughly 200x less often" (alignment.openai.com/auto-review).
- **Critical data point**: PromptArmor demonstrated the auto-review agent **approving a malicious
  npm install with elevated privileges via indirect prompt injection**, and separately got Codex to
  exfiltrate connector data. (promptarmor.com/resources/agentic-auto-review-approves-malware)
  → The guardian-agent pattern is real but **breakable**; see analysis-devils-advocate.
- Limits: the reviewer's **model** is OpenAI-only and never runs locally. The reviewer's
  implementation and policy are open source in the Codex repository itself — alignment.openai.com/auto-review:
  "Auto-review is open source in the Codex repository"; the policy lives in-tree as `policy_template.md` /
  `policy.md`, alongside `codex-rs/core/src/guardian/` (review.rs, prompt.rs, decision.rs) — but it only ever
  runs on OpenAI-hosted models.

**Read**: both majors have validated the pattern and shipped it — as the default in Claude Code,
as an opt-in reviewer mode in Codex (Settings → Permissions; `approvals_reviewer = "auto_review"`).
The pattern itself is no longer novel — and with Codex's reviewer code and policy open-sourced in-tree, even inspectability is partly claimed. What remains unclaimed is the **inspectable, locally-runnable, model-agnostic** version.

### Tier 2 — Open, model-agnostic harnesses (the direct competitors)

**OpenCode** — ~219k GitHub stars, terminal + desktop + IDE, any model. (opencode.ai;
morphllm.com/comparisons/opencode-vs-goose)
- Permission system is **static config**: `allow` / `deny` / `ask` per tool, plus a newer
  "policies" layer. (opencode.ai/docs — Permissions, Policies)
- No built-in LLM guardian and no built-in sandbox (users wrap it in bubblewrap/Docker/microVMs).
  (agent37.com/blog/opencode-sandbox)
- **Plugin ecosystem exists** — e.g. `opencode-allowlist` auto-approves file access from config.
  A "guardian plugin" for OpenCode is a weekend project for someone. ← biggest structural threat,
  expanded in analysis-devils-advocate.
- Community friction signal: multiple reports of permissions not behaving as configured
  (github.com/anomalyco/opencode/issues/45059; reddit r/opencodeCLI "Cannot figure out
  permissions").

**Goose (Block/AAIF)** — Rust, Apache-2.0, 15+ providers, 70+ MCP extensions, CLI + desktop +
embeddable server (consolidated into `goose serve`). (developersdigest.tech/blog/github-trending-goose-2026-06-07)
- Safety story: per-tool confirmation prompts and MCP trust settings — human-in-the-loop, plus
  `smart_approve`, a best-effort LLM read/write classification "interpreted by your LLM provider"
  — not a dedicated local guardian.
- Same threat profile as OpenCode: extensible enough that a guardian could be added upstream.

**Claw Code (ultraworkers, formerly instructkr)** — clean-room Rust reimplementation of Claude
Code's architecture; one of the fastest repos to ~100k stars (now ~195k, MIT) after Claude Code's
source leaked via npm; transferred from instructkr to ultraworkers.
(github.com/ultraworkers/claw-code; thursdai.news/topics/coding; arxiv.org/html/2609.00006v1
"Harness Engineering" — a source-code study of eleven coding agents that also catalogs Claw
Code in its ecosystem survey)
- **Retired (per its README, Oct 2026)**: "not the serious production project — closer to a museum
  exhibit", an agent-managed exhibit; its active successors are **LazyCodex**
  (code-yeongyu/lazycodex) and **Gajae-Code** (Yeachan-Heo/gajae-code). The crates.io `claw-code`
  crate is a deprecated stub renamed to `agent-code`.
- Dated snapshot (eigent.ai, Apr 1 2026): ~20–25% of Claude Code's functional surface — core loop,
  SSE client, six tools,
  **three-tier static permissions (Allow/Deny/Prompt)**; no MCP, no subagents, no hooks.
  (www.eigent.ai/blog/claw-code)
- Provider-agnostic ambition, but **no auto-mode classifier reimplementation** found in research.
- Threat level: proves the *harness* is commoditized and cloneable in weeks. The lineage's active
  successors (LazyCodex / Gajae-Code) carry that clone speed — if they decide to clone auto mode
  too, they beat us to "open Claude Code parity" without the rig
  dependency.

**Crush (Charmbracelet)** — ~29k stars, Go-native, model-agnostic, polished TUI. Same category,
static permissions. (arxiv Harness Engineering paper)

**Long tail**: github.com/RyanAlberts/best-of-Agent-Harnesses is a "ranked list of 167 AI agent
harnesses … rescored weekly"; lib.rs lists multiple Rust CLI
agents (the `claw-code` crate is a deprecated stub — renamed `agent-code`). The harness layer is saturated and free.

### Tier 3 — Local-first Rust agents

r/coolgithubprojects (Oct 2026): "local-first AI coding agent in Rust… Ollama, llama.cpp" — the
exact pitch Fontaine's model-agnostic story competes with, minus the guardian. Dozens of these.

## Landscape summary table

| Product | Open | Any model | Local guardian model | Permission engine | OS sandbox |
|---|---|---|---|---|---|
| Claude Code | ✗ | ✗ (Anthropic) | ✗ (server-side classifier) | **LLM classifier** + prose rules + static rules | ✓ (Seatbelt/bubblewrap; srt extracted from it) |
| Codex CLI | ✓ (Rust) | ✗ (OpenAI) | ✗ (OpenAI-hosted model; reviewer code + policy OSS) | **LLM reviewer** at sandbox boundary | ✓ |
| OpenCode | ✓ | ✓ | ✗ | static allow/deny/ask + policies | ✗ |
| Goose | ✓ | ✓ | ✗ | human prompts / trust settings + `smart_approve` (LLM classification via your provider) | ✗ |
| Claw Code (retired; lineage: LazyCodex / Gajae-Code) | ✓ | ✓ (ambition) | ✗ | static 3-tier | ✗ |
| Crush | ✓ | ✓ | ✗ | static | ✗ |
| **Fontaine** | ✓ | ✓ | **✓** | **LLM guardian, prose rules, fail-closed** | ✗ (v1, out of scope) |

## Landscape conclusion

1. **The harness is not the product.** 167 harnesses; OpenCode alone has 219k stars. Competing on
   "another terminal coding agent in Rust" is competing on zero.
2. **The guardian pattern is validated but captive.** Both closed platforms run LLM adjudication as
   a default/flag. Claude Code's classifier is truly closed and server-side; Codex exposes the
   reviewer's code and policy in its open-source repo, but not the judge model, which only runs on
   OpenAI's servers. Open harnesses default to static allow/deny/ask lists — Goose's `smart_approve`
   (best-effort LLM read/write classification interpreted by your LLM provider) is the partial
   exception — the exact thing Anthropic's own data (89% vs 13.6%) says humans and static rules
   fail at. What remains unclaimed is a guardian that is inspectable **and** locally runnable
   **and** model-agnostic at once.
3. **The empty cell in the table is real**: open + any-model + local LLM guardian + inspectable
   prose rules. Fontaine targets precisely that cell, and today nothing occupies it.
4. **The cell is undefended.** It's empty because nobody shipped it yet, not because it's hard to
   enter. OpenCode's plugin API, Goose's extensions, and the claw-code lineage's momentum (now
   active as LazyCodex / Gajae-Code) are all one PR away
   from filling it. Time-to-market and eval quality are the only durable claims. (See analysis-devils-advocate.)
