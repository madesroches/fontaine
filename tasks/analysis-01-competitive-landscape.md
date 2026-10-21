# Analysis 01 — Competitive Landscape: Coding Agent Harnesses

Date: 2026-10-20. Companion to `tasks/fontaine_harness_plan.md`. Sources: web research, links inline.

## Scope of this file

Who competes with Fontaine **as a terminal AI coding agent**, and what their permission/safety
story is. The guardrail-runtime market (LlamaFirewall, proxies, MCP scanners) is covered in
`analysis-02`; that market is where the "does this make Fontaine useless" threat actually lives.

## The field (Oct 2026)

### Tier 1 — Closed platform harnesses (the feature originators)

**Claude Code (Anthropic)** — the product Fontaine's signature feature replicates.
- Auto mode is now the **default** for Pro/Max/Team (since Aug 14, 2026). A server-side
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

**Codex CLI (OpenAI)** — shipped the same pattern three weeks before Claude Code's default flip.
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

**OpenCode** — ~209k GitHub stars, terminal + desktop + IDE, any model. (opencode.ai;
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

**Claw Code (instructkr)** — clean-room Rust+Python reimplementation of Claude Code's
architecture; one of the fastest repos to ~100k stars after Claude Code's source leaked via npm.
(github.com/instructkr/claw-code; thursdai.news/topics/coding; arxiv.org/html/2609.00006v1
"Harness Engineering" — a source-code study of eleven coding agents including Claw Code)
- Today: ~20–25% of Claude Code's functional surface — core loop, SSE client, six tools,
  **three-tier static permissions (Allow/Deny/Prompt)**; no MCP, no subagents, no hooks.
  (eigent.ai/blog/claw-code)
- Provider-agnostic ambition, but **no auto-mode classifier reimplementation** found in research.
- Threat level: proves the *harness* is commoditized and cloneable in weeks. If this community
  decides to clone auto mode too, they beat us to "open Claude Code parity" without the rig
  dependency.

**Crush (Charmbracelet)** — ~26k stars, Go-native, model-agnostic, polished TUI. Same category,
static permissions. (arxiv Harness Engineering paper)

**Long tail**: yavchn.com ranks **167 agent harnesses** weekly; lib.rs lists multiple Rust CLI
agents (including a `claw-code` crate). The harness layer is saturated and free.

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
| Claw Code | ✓ | ✓ (ambition) | ✗ | static 3-tier | ✗ |
| Crush | ✓ | ✓ | ✗ | static | ✗ |
| **Fontaine** | ✓ | ✓ | **✓** | **LLM guardian, prose rules, fail-closed** | ✗ (v1, out of scope) |

## Landscape conclusion

1. **The harness is not the product.** 167 harnesses; OpenCode alone has 209k stars. Competing on
   "another terminal coding agent in Rust" is competing on zero.
2. **The guardian pattern is validated but captive.** Both closed platforms run LLM adjudication as
   a default/flag; neither exposes it. Every open harness still uses static allow/deny/ask lists —
   the exact thing Anthropic's own data (89% vs 13.6%) says humans and static rules fail at.
3. **The empty cell in the table is real**: open + any-model + local LLM guardian + inspectable
   prose rules. Fontaine targets precisely that cell, and today nothing occupies it.
4. **The cell is undefended.** It's empty because nobody shipped it yet, not because it's hard to
   enter. OpenCode's plugin API, Goose's extensions, and Claw Code's momentum are all one PR away
   from filling it. Time-to-market and eval quality are the only durable claims. (See analysis-04.)
