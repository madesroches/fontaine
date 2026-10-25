# Research — Claude models on Microsoft Foundry and Databricks

Date: 2026-10-24. Issue: #157. Sibling studies: #154 (DeepSeek), #155 (Qwen), #156 (Kimi).
Reference harness: Claude Code (documented behaviour, plus `claude -p` where cheap).

Purpose: find what Fontaine should add to get the most out of Claude models when they are reached
through Microsoft Foundry on Azure or Databricks Model Serving, and whether each host exposes it.

Method and limits. This is a docs-based study. No Azure or Databricks account (and no `AZURE_*`,
`DATABRICKS_*` or `ANTHROPIC_*` variable) exists in the environment, so **no request was sent to
either host**. Every host claim is therefore one of:

- `docs-only`: stated by a docs page (URL and access date in the sources);
- `source`: read from rig-core 0.45.0 or Fontaine code (a statement about our side only);
- `unknown`: the docs are silent or contradict each other;
- `untested`: a request would settle it, and the untried list names it.

Support values are `yes`, `no`, `partial`, `untested`, `unknown`. A `yes` or `no` from docs alone is
always marked `docs-only`; none is backed by an observed request. All sources were accessed
2026-10-24. No credentials, benchmark or scorecard appear here.

## 1. Summary

- **Foundry is the easy host.** It serves the real Anthropic Messages API at
  `https://<resource>.services.ai.azure.com/anthropic/v1/messages` and says it supports most Claude
  features, with `anthropic-beta` taken in the header (docs-only). The gap is on our side: rig's
  `providers::azure` is Azure OpenAI Chat Completions, and rig's Anthropic client authenticates
  with `x-api-key` only, while Foundry also wants `api-key` or an Entra `Authorization: Bearer`
  token. Reusing rig's Anthropic wire with a new auth seam is enough; no new wire is needed (#165).
- **Databricks needs a new account kind** (#166). Its Claude endpoints are `databricks-claude-*`,
  token-authenticated, and reachable on an OpenAI-compatible Chat route and an Anthropic Messages
  route. The Messages route is documented under two different paths on two pages, and its beta
  header, thinking and caching pass-through is undocumented. Chat-route thinking comes back as
  OpenAI-style `reasoning` summaries, not Anthropic thinking blocks.
- **The one correctness risk found is the guardian.** The guardian, screen and scanner force a tool
  call. Anthropic documents Opus 5.5, Sonnet 5.5, Fable 5.1 and Mythos 5.1 as rejecting a forced
  `tool_choice` with a 400, and rig suppresses the forced choice only when the model id matches its
  catalog exactly. A Foundry deployment named anything but the model id, or any Databricks
  `databricks-claude-*` name, would miss that match (source; untested). The failure is fail-closed
  (every call denied), but the model is unusable as a guardian (#167, #168).
- **Thinking is always on or adaptive on current models**, so what matters is not disabling it
  (guardian) and round-tripping signed blocks (agent). Both hosts list adaptive thinking; the
  signature and binding behaviour on each host is untested.
- **The optimisations map to existing issues**: effort (#53), cache TTL and prefix stability
  (#13), cache accounting (#56), tool search (#34), images and PDFs (#21). Foundry supports each
  of them per its docs; Databricks documents prompt caching, images, effort and reasoning, and is
  silent on the rest.
- **Six issues filed**: #165 Foundry account kind, #166 Databricks account kind, #167 host model
  facts, #168 guardian/scanner on Claude, #169 per-host feature and beta gating, #170 live
  verification when accounts exist. No deviation from Claude Code semantics is proposed, so no
  `## Decisions` line was added to the master plan.

## 2. Central list: Claude-model support Fontaine should add

Entry format: what it is; evidence; correctness (C) or optimisation (O); Azure; Databricks; issue.
"Azure" means Foundry; "hosted on Azure" and "hosted on Anthropic" are Foundry's two hosting
options (section 3.1).

1. **Messages-wire account for Foundry (api-key, bearer, Entra).** The route and auth that make
   Claude reachable at all. Evidence: Anthropic's and Microsoft's Foundry pages (docs-only);
   rig `AnthropicConfig` sends `x-api-key` only (source). **C.** Azure: yes (docs-only).
   Databricks: n/a. Issue: #165 (plumbing owner #55).
2. **Databricks account kind on the Messages route with token auth.** Evidence: Databricks
   Messages page (two path forms, below), supported-models page (docs-only). **C.** Azure: n/a.
   Databricks: partial (docs-only; path ambiguity). Issue: #166.
3. **Host model name to Claude model facts.** rig keys `rejects_forced_tool_choice`, default
   `max_tokens`, thinking binding and context window on the exact model id; deployment names and
   `databricks-` prefixes miss it. Claude Code has the same problem and offers `modelOverrides`
   (docs-only). **C.** Azure: deployment name is user-chosen (docs-only). Databricks: fixed
   `databricks-claude-*` names (docs-only). Issue: #167.
4. **Guardian and scanner: forced tool choice and thinking-off per Claude model.** Evidence:
   Anthropic thinking docs; Fontaine `Guardian`/`LlmScanner` (source); `thinking_off` is `None` for
   the Anthropic kind. **C** (safe but unusable without it). Azure: model behaviour is the same
   (docs-only; Foundry's thinking table differs for Opus 5.5, section 4). Databricks: model
   behaviour documented as "reasoning cannot be disabled" for Opus 5.5 and Fable 5.1. Issue: #168.
5. **Per-host feature and beta gating with retry-without.** Evidence: Foundry's unsupported list;
   Claude Code's retry behaviour (docs-only). **C** (a rejected field is a 400 on every turn).
   Azure: partial (docs-only). Databricks: unknown. Issue: #169.
6. **Thinking-block round trip, including signatures and block binding.** Thinking blocks must be
   returned unmodified with tool results; newer models bind them to the system prompt, tools and
   earlier messages and 400 or drop on a mismatch. rig already round-trips signatures and, on its
   Anthropic dialect only, sends the `thinking-binding-controls-2026-08-01` beta (source). **C.**
   Azure: untested (not documented for Foundry). Databricks: Messages route unknown; Chat route
   uses a different `reasoning` shape (docs-only). Issue: #166 and #169 (the binding beta must not
   be sent to a host that rejects it); live check in #170.
7. **Effort and adaptive thinking.** `output_config.effort` with `thinking: {type: "adaptive"}`;
   the budget (`budget_tokens`) is a 400 on current models. Claude Code: `/effort`, `--effort`.
   **O** (default effort is acceptable). Azure: yes (docs-only; Foundry lists per-model levels).
   Databricks: partial (docs-only; Opus 5.5 `low` to `max`, Haiku 5.5 `low` to `high`). Issue: #53.
8. **Prompt caching: `cache_control`, 5m and 1h TTL, stable prefix.** Thinking or effort changes
   invalidate cached message blocks (and tools or system on some models). **O**, but a large one.
   Azure: yes, both TTLs and automatic caching (docs-only). Databricks: partial (`cache_control`
   ephemeral documented on text, reasoning, image and tool blocks; TTL and usage fields not
   documented). Issue: #13.
9. **Cache and usage accounting.** Cache read and write token fields and the 5m/1h split.
   Claude Code reports them (observed, Appendix A). Azure: yes (docs-only; Foundry says `usage` is
   identical across platforms; its rate-limit headers are absent). Databricks: unknown (cache
   fields undocumented; a community post says `ai_query` hides them). **O.** Issue: #56.
10. **Context window and compaction sizing.** Current models are 1M with no beta header; older
    ones 200K. Claude Code assumes 200K for an unrecognised Foundry deployment name. **C** for
    compaction sizing (too small a window compacts early; too large overflows). Azure: yes
    (docs-only). Databricks: unknown (docs say "matches the provider's values", no figures).
    Issue: #167 (and `autocompact_window` already exists as an override).
11. **Fine-grained tool streaming.** `eager_input_streaming: true` per tool; the old beta header
    still works. rig sets it per tool on its Anthropic dialect and uses the header for gateway
    dialects (source). **O.** Azure: yes (docs-only). Databricks: unknown. Issue: #169.
12. **Tool search and deferred loading.** `tool_search_tool_*_20251119`. **O.** Azure: yes, both
    hosting options (docs-only). Databricks: unknown. Issue: #34.
13. **Server-side tools (web search, web fetch, code execution).** **O.** Azure: partial: hosted
    on Anthropic yes; hosted on Azure only `web_search_20250305` and `web_fetch_20250910`, no code
    execution (docs-only; other versions return 400). Databricks: unknown. Issue: none; Fontaine
    brings its own tools and should not depend on these.
14. **Context management (clearing old tool results).** Beta header
    `context-management-2025-06-27`; server-side compaction is a separate beta. **O.** Azure:
    partial (docs-only; beta on all platforms). Databricks: unknown. Issue: none; Fontaine
    compacts itself and the cache cost of clearing is a trade-off to decide later (see #13).
15. **Images and PDFs.** **O** (feature, not correctness). Azure: yes; hosted on Azure accepts
    base64 or URL images and PDFs as base64 or URL (docs-only). Databricks: partial: images yes,
    base64 only (URLs not supported), model-dependent (Sonnet 5.5 and Fable 5 are text-only);
    PDFs unknown (docs-only). Issue: #21.
16. **Sampling parameters.** `temperature`, `top_p`, `top_k` return 400 on Opus 5.5, Opus 5,
    Opus 4.8, Opus 4.7, Sonnet 5.5, Sonnet 5, Fable and Haiku 5.5, thinking or not (Anthropic);
    Databricks repeats it for Sonnet 5. rig's catalog marks these models `sampling: never`
    (source). **C** if Fontaine ever sends sampling. Azure: same model rules (docs-only).
    Databricks: documented for Sonnet 5. Issue: none; verify Fontaine sends none (not checked here).
17. **Feature verification on the hosts.** Everything marked `untested` or `unknown`. Issue: #170.

## 3. Hosts and wires

### 3.1 Microsoft Foundry on Azure

- **API shape.** Anthropic Messages, plus `count_tokens`. Base URL
  `https://<resource>.services.ai.azure.com/anthropic`, request URL `.../anthropic/v1/messages`.
  Requests carry `anthropic-version: 2023-06-01`. (docs-only)
- **Model id.** The `model` field is the *deployment name*, which defaults to the model id
  (`claude-opus-5-5`) and cannot be changed after creation. (docs-only)
- **Hosting options.** "Hosted on Azure" (inference on Azure; Opus 5.5, Opus 5, Opus 4.8,
  Sonnet 5.5, Sonnet 5, Haiku 5.5, Haiku 4.5; Global and US Data Zone Standard) and "Hosted on
  Anthropic" (all models, Global Standard only; Fable 5/5.1, Opus 4.5-4.7, Sonnet 4.x are only
  here). Sonnet 5.5 is Global only. (docs-only)
- **Auth.** API key as `api-key` or `x-api-key`; Entra ID as `Authorization: Bearer` with scope
  `https://ai.azure.com/.default`; Mythos models take Entra only. (docs-only) rig's Anthropic
  client sends `x-api-key` and has no bearer path; `Messages::encode_with` lets a platform build
  its own envelope (source). So a key works with a base URL override today (untested); Entra and
  bearer need new code (#165).
- **Beta headers.** `anthropic-beta` is taken in the header "as the Claude API does", comma
  separated (Anthropic beta-headers page); individual beta support per host varies and "not every
  platform accepts every beta" (docs-only). Foundry lists `fine-grained-tool-streaming-2025-05-14`
  and `context-management-2025-06-27` as headers it takes. An unknown beta is a 400 on the Claude
  API (docs-only); Foundry's behaviour for unknown values is `unknown`.
- **Not supported (by design, 400):** Admin, Compliance, Models and Message Batches APIs, Managed
  Agents, advisor tool, server-side `fallbacks`, the `computer_toolset_20260801` and
  `browser_toolset_20260801` toolsets; on hosted-on-Azure also code execution, newer web
  search/fetch versions, Agent Skills, programmatic tool calling, Files API. (docs-only)
- **Other differences.** No `anthropic-ratelimit-*` response headers; correlation ids come back
  as `request-id` and `apim-request-id`; a ZDR subscription is refused for Covered Models.
  Requests with `context-1m-2025-08-07` above 200K on Sonnet 4.5 were rejected from 2026-05-01.
  (docs-only)
- **Release lag.** `unknown` as a number of days: the pages carry no launch dates. What the docs
  do show: Fable 5.1 and 5 are Preview and Anthropic-hosted only on Foundry; Opus 4.7, 4.6 and
  Sonnet 4.6 are Anthropic-hosted only; the Microsoft pages were updated 2026-09-21/22 and list
  Opus 5.5, Sonnet 5.5 and Haiku 5.5. Claude Code's Foundry default for the `opus` alias is Opus
  4.6 unless pinned, and it warns that Foundry has no startup model check. (docs-only)

### 3.2 Databricks Model Serving

- **API shape.** Two routes. (a) OpenAI-compatible Chat Completions (`choices`, `finish_reason`,
  `tool_calls`; "differs from the Anthropic API"). (b) An Anthropic Messages route "only
  compatible with Anthropic pay per token foundation models and external models".
  (docs-only)
- **Messages path: contradiction.** One page shows
  `https://<workspace>/serving-endpoints/anthropic/v1/messages` with `databricks-claude-*` names;
  the current Anthropic Messages page shows `https://<workspace>/ai-gateway/anthropic/v1/messages`
  with `system.ai.claude-*` model names. Which one a given workspace serves is `unknown`;
  #166 must probe.
- **Model ids.** Endpoint names `databricks-claude-fable-5-1`, `-fable-5`, `-opus-5-5`, `-opus-5`,
  `-opus-4-8`, `-opus-4-7`, `-opus-4-6`, `-opus-4-5`, `-opus-4-1`, `-sonnet-5-5`, `-sonnet-5`,
  `-sonnet-4-6`, `-sonnet-4-5`, `-haiku-5-5`, `-haiku-4-5`. (docs-only)
- **Auth.** A Databricks token as `Authorization: Bearer` (the Python example); one curl example
  uses basic auth with user `token`. Whether `x-api-key` is accepted is `unknown`. rig's client
  would need a bearer seam as for Foundry.
- **Beta headers and pass-through.** The Messages page documents `model`, `max_tokens`,
  `messages` and `output_config.format` (structured outputs on Opus 5.5, Sonnet 5.5, Haiku 5.5)
  and says nothing on beta headers, extended thinking or caching: all `unknown` on that route.
  The Chat route documents `cache_control` on text, reasoning, image and tool blocks, and a
  `reasoning` block with `summary_text` (optional `signature`) and `summary_encrypted_text`.
- **Not stated:** pay-per-token vs provisioned availability per Claude model, regions, rate
  limits, context window figures, deprecation dates. Fable 5/5.1 retain prompts 30 days.
- **Release lag.** `unknown` as a number; the supported-models page lists Fable 5.1, Opus 5.5,
  Sonnet 5.5 and Haiku 5.5, i.e. the same generation as Foundry's hosted-on-Anthropic list.

### 3.3 What rig 0.45.0 covers

- `providers::anthropic` (source): `cache_control` including top-level automatic caching and
  1h markers with ordering checks, thinking with signature and redacted-thinking round trip,
  streaming, adaptive thinking, per-model catalog facts (forced-choice rejection, binding,
  sampling), typed options (`context_management`, `task_budget`, `fallbacks`, `speed`,
  `inference_geo`, MCP connector, each paired with its beta). `AnthropicConfig` has
  `with_base_url`, `with_beta`, `with_version`; headers are `x-api-key`, `anthropic-version`,
  `anthropic-beta`. It sends `thinking-binding-controls-2026-08-01` and `block_binding` only when
  the dialect is Anthropic's own.
- `providers::azure` (source): Azure OpenAI Chat Completions options only (logprobs, data
  sources); it is the wrong wire for Claude on Foundry.
- No Databricks provider. A Databricks Chat route could go through rig's OpenAI-compatible
  client, but would lose Anthropic thinking blocks and cache fields.
- Fontaine's `anthropic` kind takes an API key and an optional base URL; `opencode-go` already
  serves `claude-*` ids over the Messages wire via the same client and leaves `thinking_off` unset
  for them (source). That is the precedent for Claude behind a non-Anthropic gateway.

## 4. Model-specific support matrix

CC = Claude Code. FT = Fontaine today (source). Az = Foundry. DB = Databricks. N/C = need:
C correctness, O optimisation. All Az and DB values are docs-only unless stated.

| Row | CC | FT | Az | DB | N/C | Feeds |
|---|---|---|---|---|---|---|
| Adaptive thinking | Always adaptive on 4.7+ models; `CLAUDE_CODE_DISABLE_ADAPTIVE_THINKING` only for Opus/Sonnet 4.6 | rig sends none unless asked; `thinking_off` unset for Claude | yes | partial (reasoning listed for Fable 5.1, Haiku 5.5, Opus/Sonnet 4.x-5.5; Sonnet 5.5 and Sonnet 5 not in the Reasoning list) | C | #53 |
| Thinking `disabled` | n/a | not sent | partial; table: Haiku 5.5, Sonnet 5.5, Sonnet 5, Opus 4.8 yes; Opus 5.5 and Opus 5 yes only at effort high or below; Fable no | partial; Haiku 5.5 yes; Opus 5.5 and Fable 5.1 "cannot be disabled" | C (guardian) | #168 |
| Conflict: Opus 5.5 `disabled` | | | Microsoft table says yes (high or below); Anthropic table says 400 | Databricks says cannot disable | | unknown which holds on a host; #170 |
| `budget_tokens` | Only for older models | not sent | no on Fable, Opus 5.x, Sonnet 5, 4.7, 4.8; yes on Haiku 5.5, Sonnet 5.5, Opus/Sonnet 4.6, Haiku 4.5 per Microsoft; Anthropic says 400 for the 5.5 ones | unknown | O | #53 |
| Signature / thinking block round trip | Handled by SDK | rig round-trips signed blocks (source); Fontaine persists history with rig memory | untested | Messages route unknown; Chat route has `summary_text.signature` | C | #166, #170 |
| Block binding / `thinking-binding-controls-2026-08-01` | n/a | sent only on Anthropic dialect (source) | unknown (pass-through of this beta undocumented) | unknown | C | #169 |
| Effort | `/effort`, `--effort`, `CLAUDE_CODE_EFFORT_LEVEL`; retries without it if rejected | none yet | yes; `xhigh` equals `max`; Opus 4.6 and Sonnet 4.6 have no `xhigh` | partial; Opus 5.5 `low`..`max`, Haiku 5.5 `low`..`high`, Fable 5.1 `low`..`max` | O | #53 |
| `cache_control` 5m | on by default | rig supports; Fontaine does not enable it | yes | partial (Chat-route docs; Messages route unknown) | O | #13 |
| `cache_control` 1h TTL | opt-in via `ENABLE_PROMPT_CACHING_1H`; observed 1h writes on first-party | rig supports | yes (Anthropic lists Foundry) | unknown | O | #13 |
| Automatic (top-level) caching | n/a | rig supports | yes | unknown | O | #13 |
| Cache usage fields | shown in `claude -p --output-format json` | not mapped by name | yes; `usage` identical to Claude API | unknown | O | #56 |
| Context window | 1M by default for recognised deployment names on Foundry; 200K otherwise | catalog per model id; `autocompact_window` override | 1M for current models, 200K for 4.5-era and Haiku 4.5; 128K output | unknown | C | #167 |
| Long-context beta | none needed for 1M (Sonnet 4.5's 1M beta retired 2026-04-30) | none | n/a | unknown | O | none |
| Parallel tool calls | default | unaffected | yes (Messages API) | unknown | O | none |
| Fine-grained tool streaming | env switch | rig per tool | yes | unknown | O | #169 |
| Tool search / deferred loading | retries without `defer_loading` on rejection | none | yes | unknown | O | #34 |
| Web search / fetch | server tools | none | partial | unknown | O | none |
| Code execution | server tool | none | no on hosted-on-Azure; yes on Anthropic-hosted | unknown | O | none |
| Context editing (clear tool results) | uses it; retries without on rejection | none | partial (beta) | unknown | O | none |
| Server-side compaction | beta | none (Fontaine compacts itself) | partial (beta) | unknown | O | none |
| Images | yes | not wired | yes (base64, URL) | partial (base64 only; model-dependent) | O | #21 |
| PDFs | yes | not wired | yes (base64, URL) | unknown | O | #21 |
| Structured outputs | n/a | typed extraction via forced `submit` tool | yes (`output_config.format`) | partial (Messages route, 3 models; Chat `response_format` is a 400 on those) | O | #168 |
| Forced `tool_choice` | n/a | guardian, screen, scanner force it | no on Opus 5.5, Sonnet 5.5, Fable 5.1, Mythos 5.1 (Anthropic); otherwise yes with adaptive thinking | unknown (Chat route function calling is OpenAI-style) | C | #167, #168 |
| `temperature`, `top_p`, `top_k` | Not sent on those models | guardian sends none | no on the listed models (400) | no on Sonnet 5 (stated) | C | none |
| `count_tokens` | optional | not used | yes | unknown | O | #56 |
| Batch API | n/a | none | no | no (not the same product) | n/a | none |
| Rate-limit headers | read for display | not used | absent | unknown | O | none |

## 5. Guardian and screen

Needs from the host: thinking off or a working forced tool call, a structured verdict, bounded
latency, and fail-closed on error. Findings (docs-only unless stated):

- **Models that cannot be forced**: Opus 5.5, Sonnet 5.5, Fable 5.1, Mythos 5.1 answer a forced
  `tool_choice` with a 400 on every request. rig then does not force its own output tool
  (source), but only for catalog-matched ids. Whether the guardian then still gets a tool call
  back from a non-forced request is untested. As a screen or guardian they are the wrong default.
- **Models that can think off *and* be forced**: Haiku 5.5 (`disabled` at effort high or below),
  Sonnet 5 and Opus 4.8 (`disabled` and forced tool use accepted with adaptive thinking),
  Haiku 4.5 (thinking is off unless requested). On Foundry, Haiku 4.5, Haiku 5.5, Sonnet 5 and
  Opus 4.8 are available hosted on Azure; Haiku 4.5 is also on Databricks.
- **Latency**: no latency figures are published for either host; `unknown`. Foundry hosted on
  Azure adds no stated overhead. Not measured; the live eval harness (#163 pattern) is the tool.
- **Fail-closed**: unchanged. A 400 from the host, a timeout or an unparseable reply is a Deny
  (source: guardian). The risk is usability, not safety.
- **Thinking blocks as injection surface**: the guardian sends one stateless request with no
  history, so signatures and binding do not apply to it.
- Output goes to `docs/guardian.md` guidance only after measurement (#168, #170).

## 6. What breaks, and the recommended behaviour

Behaviours: fall back, warn, refuse. No deviation from Claude Code semantics is proposed (Claude
Code also disables features per host); no `## Decisions` line added.

| Missing or different | Recommendation |
|---|---|
| Beta header or field rejected by a host (400 naming it) | fall back: retry once without it, remember for the session (Claude Code does this for effort, `context_management`, tool search). #169 |
| Hosted-on-Azure lacks code execution, newer web tools, skills, Files API | warn once if a user config enables them; Fontaine does not use them by default. #169 |
| Forced `tool_choice` rejected (Opus 5.5, Sonnet 5.5, Fable 5.1) | refuse as a guardian/scanner model at startup with a clear warning; the guardian still fails closed. #168 |
| Thinking cannot be disabled and the caller needs it off | refuse for guardian use; fall back to a non-forced request only if measured to work. #168 |
| Unknown deployment name (no model facts) | warn and ask for a model mapping; assume 200K context like Claude Code. #167 |
| Prompt caching or cache fields unavailable (Databricks Messages route) | fall back to uncached requests, show unknown cache usage. #13, #56 |
| Image URLs unsupported (Databricks) | fall back to base64. #21 |
| Databricks Messages path ambiguity | refuse with an error naming both documented paths until probed. #166 |
| Entra or bearer token expires mid-session (about an hour on Foundry) | warn and refresh via the credential chain; refuse on failure. #165 |
| Sampling parameters on models that 400 them | never send them. none |

## 7. Untried

Nothing was sent to Foundry or Databricks. No account, key, resource or workspace was available,
and creating one (Azure Marketplace subscription, billed per Claude Consumption Unit; a Databricks
workspace) is a spend decision for the maintainer. To be run under #170:

1. Foundry: `x-api-key` / `api-key` / bearer against `/anthropic/v1/messages`; deployment-name
   routing; unknown `anthropic-beta` value (400 or ignore); `thinking: disabled` on Opus 5.5.
2. Foundry: thinking through a tool call with signatures, on a hosted-on-Azure and an
   Anthropic-hosted deployment; `thinking-binding-controls-2026-08-01` pass-through.
3. Foundry: `cache_control` 5m, 1h and top-level; cache usage fields and `cache_creation` split.
4. Foundry: forced `tool_choice` on Opus 5.5, Sonnet 5.5, Haiku 5.5; guardian structured verdict.
5. Foundry: `context-management-2025-06-27`, `eager_input_streaming`, tool search, images, PDFs,
   `count_tokens`.
6. Databricks: which Messages path a workspace serves; `x-api-key` vs Bearer; the same thinking,
   caching, beta, forced-choice and sampling checks on the Messages route; PDF input; usage
   fields; cross-region and pay-per-token availability per model; context window figures.
7. Latency of Haiku 5.5, Haiku 4.5, Sonnet 5 and Opus 4.8 as guardian on each host.
8. Release lag: no dated source found; would need the hosts' launch dates for one model release.
9. Claude Code against Foundry or Databricks (not tried; no endpoint).

## Appendix A. Observations

- Claude Code 2.1.310, `claude -p "reply with only the word ok" --output-format json`, run
  2026-10-24 against Anthropic's first-party API through the existing login (no host involved).
  Relevant fields of the response `usage`: `cache_creation_input_tokens` (14251),
  `cache_read_input_tokens` (11333), `cache_creation.ephemeral_1h_input_tokens` (14251),
  `ephemeral_5m_input_tokens` 0, `inference_geo` "global", `output_tokens_details.thinking_tokens`
  0. Shows Claude Code writes 1h-TTL cache entries on first-party and exposes the split. It says
  nothing about Foundry or Databricks. No request body was captured; no credential recorded.
- `claude --help` mentions Bedrock, Vertex and Foundry as provider options.

## Appendix B. Sources (accessed 2026-10-24)

Anthropic:
- https://platform.claude.com/docs/en/build-with-claude/claude-in-microsoft-foundry
- https://platform.claude.com/docs/en/build-with-claude/overview
- https://platform.claude.com/docs/en/build-with-claude/thinking
- https://platform.claude.com/docs/en/build-with-claude/prompt-caching (first 100K of 162K
  characters read)
- https://platform.claude.com/docs/en/build-with-claude/context-windows
- https://platform.claude.com/docs/en/build-with-claude/context-editing
- https://platform.claude.com/docs/en/agents-and-tools/tool-use/fine-grained-tool-streaming
- https://platform.claude.com/docs/en/api/beta-headers

Claude Code:
- https://code.claude.com/docs/en/microsoft-foundry
- https://code.claude.com/docs/en/llm-gateway
- https://code.claude.com/docs/en/llm-gateway-protocol (partly read via search of the saved page)
- https://code.claude.com/docs/en/env-vars (first 100K of 185K characters read)

Microsoft:
- https://learn.microsoft.com/en-us/azure/foundry/foundry-models/how-to/use-foundry-models-claude
- https://learn.microsoft.com/en-us/azure/foundry/foundry-models/concepts/claude-models

Databricks:
- https://docs.databricks.com/aws/en/machine-learning/foundation-model-apis/supported-models
  (and the `generative-ai/foundation-models` mirror)
- https://docs.databricks.com/aws/en/generative-ai/foundation-models/anthropic-messages
- https://docs.databricks.com/aws/machine-learning/model-serving/score-foundation-models
- Search result listing `/serving-endpoints/anthropic/v1/messages` on
  https://docs.databricks.com/aws/en/machine-learning/model-serving/query-anthropic-messages
  (the direct fetch returned 404; that path form is only from the search summary)
- `https://docs.databricks.com/aws/en/machine-learning/model-serving/query-reason` returned 404;
  Databricks reasoning-parameter docs were not found.

Local: rig-core 0.45.0 (`providers/anthropic`, `providers/azure`, `catalog/models.json`);
Fontaine `providers/anthropic.rs`, `providers/opencode_go.rs`, `providers/mod.rs`, `models.rs`,
`agent.rs`, `scanner_stage.rs`, `fontaine-guardian`.

Caveat: web pages were read through a summarising fetch tool; quoted values come from its
extraction, and the 404 and truncation notes above mark where reading was incomplete.
