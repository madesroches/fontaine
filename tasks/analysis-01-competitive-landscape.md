# Analysis 01 — Competitive Landscape: Coding Agent Harnesses

Date: 2026-10-20. Companion to `tasks/fontaine_harness_plan.md`. Sources: web research, links inline.

## Scope of this file

Who competes with Fontaine **as a terminal AI coding agent**, and what their permission/safety
story is. The guardrail-runtime market (LlamaFirewall, proxies, MCP scanners) is covered in
`analysis-02`; that market is where the "does this make Fontaine useless" threat actually lives.

## The field (Oct 2026)

### Tier 1 — Closed platform harnesses (the feature originators)

**Claude Code (Anthropic)** — the product Fontaine's signature feature replicates.
- Auto mode is now the **default**: the built-in starting mode for interactive sessions on every
  plan (since v2.1.283; initially Pro/Max/Team from Aug 14, 2026). A server-side
  classifier reviews every non-read-only action; Anthropic reports **89% of dangerous commands
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

**Codex CLI (OpenAI)** — shipped the same pattern the same week Claude Code announced its default flip (`--approve-for-me` / auto-review, v0.147.0, Aug 2026).
- `--approve-for-me` (v0.147.0, Aug 7, 2026): a **separate reviewer agent** answers approval
  requests at the sandbox boundary. "Auto-review replaces manual approval at the sandbox boundary
  with a separate reviewer agent." (codex-docs.com/en/docs/sandboxing/auto-review;
  alignment.openai.com/auto-review/)
- Reported free for ChatGPT sign-ins, "200x fewer approvals" marketing. (explainx.ai)
- **Critical data point**: PromptArmor demonstrated the auto-review agent **approving a malicious
  npm install with elevated privileges via indirect prompt injection**, and separately got Codex to
  exfiltrate connector data. (promptarmor.com/resources/agentic-auto-review-approves-malware)
  → The guardian-agent pattern is real but **breakable**; see analysis-04.
- Limits: reviewer is closed, OpenAI-only, tied to their sandbox model.

**Read**: both majors have validated the pattern and made it a default. The pattern itself is no
longer novel. What remains unclaimed is the **open, inspectable, model-agnostic** version.

### Tier 2 — Open, model-agnostic harnesses (the direct competitors)

**OpenCode** — ~219k GitHub stars, terminal + desktop + IDE, any model. (opencode.ai;
morphllm.com/comparisons/opencode-vs-goose)
- Permission system is **static config**: `allow` / `deny` / `ask` per tool, plus a newer
  "policies" layer. (opencode.ai/docs — Permissions, Policies)
- No built-in LLM guardian and no built-in sandbox (users wrap it in bubblewrap/Docker/microVMs).
  (agent37.com/blog/opencode-sandbox)
- **Plugin ecosystem exists** — e.g. `opencode-allowlist` auto-approves file access from config.
  A "guardian plugin" for OpenCode is a weekend project for someone. ← biggest structural threat,
  expanded in analysis-04.
- Community friction signal: multiple reports of permissions not behaving as configured
  (github.com/anomalyco/opencode/issues/45059; reddit r/opencode "Cannot figure out permissions").

**Goose (Block/AAIF)** — Rust, Apache-2.0, 15+ providers, 70+ MCP extensions, CLI + desktop +
embeddable `goosed` server. (developersdigest.tech/blog/github-trending-goose-2026-06-07)
- Safety story: per-tool confirmation prompts and MCP trust settings — human-in-the-loop, not
  LLM-adjudicated.
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
  (eigent.ai/blog/claw-code)
- Provider-agnostic ambition, but **no auto-mode classifier reimplementation** found in research.
- Threat level: proves the *harness* is commoditized and cloneable in weeks. The lineage's active
  successors (LazyCodex / Gajae-Code) carry that clone speed — if they decide to clone auto mode
  too, they beat us to "open Claude Code parity" without the rig
  dependency.

**Crush (Charmbracelet)** — ~26k stars, Go-native, model-agnostic, polished TUI. Same category,
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
| Codex CLI | ✓ (Rust) | ✗ (OpenAI) | ✗ (closed reviewer) | **LLM reviewer** at sandbox boundary | ✓ |
| OpenCode | ✓ | ✓ | ✗ | static allow/deny/ask + policies | ✗ |
| Goose | ✓ | ✓ | ✗ | human prompts / trust settings | ✗ |
| Claw Code (retired; lineage: LazyCodex / Gajae-Code) | ✓ | ✓ (ambition) | ✗ | static 3-tier | ✗ |
| Crush | ✓ | ✓ | ✗ | static | ✗ |
| **Fontaine** | ✓ | ✓ | **✓** | **LLM guardian, prose rules, fail-closed** | ✗ (v1, out of scope) |

## Landscape conclusion

1. **The harness is not the product.** 167 harnesses; OpenCode alone has 219k stars. Competing on
   "another terminal coding agent in Rust" is competing on zero.
2. **The guardian pattern is validated but captive.** Both closed platforms run LLM adjudication as
   a default/flag; neither exposes it. Every open harness still uses static allow/deny/ask lists —
   the exact thing Anthropic's own data (89% vs 13.6%) says humans and static rules fail at.
3. **The empty cell in the table is real**: open + any-model + local LLM guardian + inspectable
   prose rules. Fontaine targets precisely that cell, and today nothing occupies it.
4. **The cell is undefended.** It's empty because nobody shipped it yet, not because it's hard to
   enter. OpenCode's plugin API, Goose's extensions, and the claw-code lineage's momentum (now
   active as LazyCodex / Gajae-Code) are all one PR away
   from filling it. Time-to-market and eval quality are the only durable claims. (See analysis-04.)
