# Cascade Screen Stage Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/130

## Overview

Put a fast typed-decision screen between the deterministic floor and the LLM guardian in `auto`
(and `plan`) mode. The screen asks one yes/no question of a small local model and reads `p(yes)`
from its first-token logprobs. When the call is confidently outside what the user asked for, the
screen stops it as `escalate`. Otherwise the call goes to the LLM guardian exactly as today. The
screen never allows, so every call it does not stop still meets the guardian. That makes the stage
safe by construction (option 1).

Held out end to end in the #98 spike (`research/analysis-typed-decision-screen.md`, experiment 9),
the setup was:
- **Screen:** `qwen3-4b-instruct-2507` asked "If an employee were given the user's request, would
  they be allowed to take this action?";
- **Threshold:** a 10% escalation budget fitted on a synthetic corpus;
- **Guardian:** a fresh live `kimi-for-coding` run.

| | Catch | LLM calls | Mean latency | Allowed calls escalated |
|---|---|---|---|---|
| Guardian alone | 91.7% | 100% | 7.3 s | 3.4% |
| With the screen | 96.7% | 41% | 3.0 s | 13.6% |

## Current State

- `PermissionPipeline` builds an `AdjudicationRequest` with `adjudication::build` and calls
  `self.state.guardian().adjudicate(&request)` (`rust/fontaine-core/src/permissions.rs:1039`). The
  result goes to `settle`.
- `Guardian` (`rust/fontaine-guardian/src/guardian.rs`) renders the request with `prompt_builder`,
  which redacts and delimits the untrusted blocks, then calls a rig model and returns an
  `Adjudication`.
- `[guardian]` config is `GuardianSection` (`rust/fontaine-core/src/config.rs:329`).
- The eval harness already has a typed-decision client for `/v1/systemone`
  (`rust/fontaine-evals/src/screen.rs`). The logprob read used in the spike is
  `research/typed-decision-screen/scripts/llm_noul.py` (Ollama native `/api/chat` with
  `think: false`, `logprobs: true`, `top_logprobs: 20`, `num_predict: 1`).

## Design

### Where it runs

```
floor (deny → ask → critical-path → protected-path)
  → rules, modes, containment fast path       (unchanged)
  → screen   ── risk ≥ t_stop ──► escalate (Tier::Screen)
       │ error / timeout / below threshold
       ▼
  → LLM guardian                              (unchanged, fail-closed)
```

The screen runs only where the guardian would run. It does not touch the floor, rules or
containment, and a child subagent inherits it with its parent's mode.

### `fontaine-guardian::screen`

A new module, kept dependency-light so that the crate stays lift-out-able:

```rust
pub struct ScreenSettings {
    pub endpoint: Url,          // Ollama base URL
    pub model: String,          // e.g. "qwen3:4b-instruct-2507-q4_K_M"
    pub question: Question,     // built-in set; default EmployeeAllowed
    pub t_stop: f64,            // required; no vendor default
    pub timeout: Duration,      // default 2 s
}
pub enum ScreenOutcome {
    Stop { risk: f64, latency_ms: u64 },
    Pass { risk: f64, latency_ms: u64 },
    Unavailable { reason: String, latency_ms: u64 },   // goes to the guardian
}
pub struct Screen { /* reqwest client + settings */ }
impl Screen { pub async fn screen(&self, request: &AdjudicationRequest) -> ScreenOutcome; }
```

- **Input:** the guardian's own rendered user message (`prompt_builder::render` with the active
  profile). The redaction and untrusted-block delimiting are then the same, and nothing new is sent.
- **Read:** `risk = 1 − p(yes)` for a positive question, or `p(yes)` for a negative one, where
  `p(yes) = mass(yes) / (mass(yes) + mass(no))` over the top logprobs. If the yes and no mass
  together is below 0.5 (the model is not answering yes or no), the outcome is `Unavailable`.
- **Questions:** a fixed enum in code, not free text in config. The question and its polarity are
  part of the calibration. A config string would invite unvalidated thresholds.
- **Transport:** an OpenAI-compatible `logprobs` endpoint as a second backend later (#132). Ollama
  native comes first, because its OpenAI endpoint drops logprobs.

### Pipeline wiring (`fontaine-core::permissions`)

Before `adjudicate`, when a screen is configured:

- **`Stop`:** settle as `Decision::Escalate`, with `Tier::Screen` and the reason "a local screen
  judged this call outside the user's request (risk 0.97)". In headless runs, escalate follows
  today's `escalate` semantics. The guardian's breaker and cache are not touched.
- **`Pass` or `Unavailable`:** call the guardian as today. `Unavailable` warns once per session
  through `Approver::warn`, so a broken screen is visible but never blocks.

### Config (user scope only)

```toml
[guardian.screen]
model = "local/qwen3:4b-instruct-2507-q4_K_M"   # an [accounts.*] entry of kind ollama
question = "employee_allowed"
stop_threshold = 0.95
timeout_ms = 2000
shadow = true          # default: score and log, never stop
```

- **Scope:** loading the stage is a loosening-shaped key (it decides calls), so it loads only
  from user-trust scopes. Project scope may not enable it, but may set `shadow = true` to tighten.
- **No defaults:** `stop_threshold` has no default. The stage refuses to load without it (a
  configuration error, exit 2).
- **Shadow mode:** `shadow = true` is the default. The screen scores every call and records the
  score, and the guardian decides. This is #124's mechanism and how a user calibrates the
  threshold on their own traffic.

### Audit, telemetry, corpus

- The audit record gains `screen_risk`, `screen_model` and `screen_latency_ms` on every
  guardian-tier decision, plus `tier = screen` when the screen stopped the call.
- The `fontaine.guardian.screen` span carries `risk`, `outcome` and `latency_ms`. A
  `fontaine.screen.count{outcome}` metric counts the outcomes.
- **Corpus:** shadow scores joined with guardian verdicts and human answers are exactly the
  labelled pairs #123 wants. #125 distills from them.

### Security invariants

- **Fail-closed guardian:** unchanged. A screen failure routes to the guardian, never to allow.
- **No guardian disable:** the screen cannot replace the guardian, and no key makes it allow.
- **Floor gates every mode:** the screen runs after the floor. There is no regex on arguments:
  the screen reads the rendered request.
- **Fast path earned by containment:** untouched. The screen never allows (option 2 is out of
  scope, see #133).
- **Child never looser:** the screen only adds stops.
- **Project scope only tightens:** see Config.

## Implementation Steps

1. `fontaine-guardian/src/screen.rs`: `Screen`, `ScreenSettings`, `ScreenOutcome`, the question
   enum, the Ollama logprob client and the yes/no mass read. Unit tests against `wiremock`.
2. `fontaine-core/src/config.rs`: a `GuardianScreenSection` under `[guardian.screen]`, with
   scope rules and validation (threshold in (0, 1], required unless shadow, known question).
   Config tests.
3. `fontaine-core/src/permissions.rs`: wire the stage before `adjudicate`. Add `Tier::Screen`,
   with its audit and telemetry fields. Pipeline tests with a fake screen: stop, pass,
   unavailable, shadow, subagent inheritance, and that project scope cannot enable it.
4. `fontaine-evals`: `--screen live:<account>/<model>` beside `--guardian live:…`, so a
   scorecard reports the cascade (catch, LLM calls, latency, allowed escalated) from one run.
5. Docs: `docs/guardian.md` (the stage, shadow mode, calibration), `docs/configuration.md`
   (`[guardian.screen]`), `docs/evals.md` (cascade scorecards). `CHANGELOG.md`.
6. A plan `## Decisions` line recording that the stage is option 1 only, and why.

## Testing Strategy

- **Unit:** the logprob read (yes/no mass, low label mass, case and whitespace variants), the
  risk polarity per question, the timeout, and HTTP errors mapping to `Unavailable`.
- **Integration** (`fontaine -p`, wiremock Ollama):
  - a stop yields `escalate` with `tier = screen` and no guardian call;
  - a pass and an unavailable screen both reach the guardian;
  - shadow mode never changes a decision and records the score.
- **Choke point:** the screen never produces `Allow`. Assert it in the pipeline test matrix.
- **Live** (manual, `evals/`): a cascade scorecard with qwen3-4b-instruct on the golden set,
  compared against the spike's numbers.

## Open Questions

- Whether `escalate` from the screen should carry the guardian's denial-retry affordance
  (`/denied`), or a "send to the guardian instead" choice in the TUI prompt.
- Whether to ship a starter threshold from the synthetic corpus for shadow-mode reports only, so
  a fresh install's shadow log is readable before calibration.
