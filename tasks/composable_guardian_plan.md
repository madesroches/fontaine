# Composable Guardian Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/137

## Overview

`[guardian.screen]` hard-codes one composition (a logprob screen raced against the LLM guardian)
and flattens it into keys. This plan makes the guardian's composition a **Rune script**: the
script is the composition, the host supplies the leaves (the LLM guardian, logprob reads) and
mints every verdict, and Rust wraps the script in a fail-closed root. Today's screen ships as the
built-in `screen` preset, a Rune script compiled into `fontaine-guardian`, so the common case stays
a few lines of TOML. Concurrency is opt-in: Rune futures are lazy, so a script that awaits calls
one after another runs them in sequence, and only `first_decisive([...])` or
`std::future::join(...)` runs them together. It folds in #136 (sequential versus concurrent
cost, dropped-request spend).

## Current State

- `rust/fontaine-guardian/src/cascade.rs:36` — `adjudicate(screen, guardian, request)` is a
  biased `tokio::select!` between `Guardian::adjudicate` and `Screen::screen`. It returns
  `Composed { screen, decided: Decided::{ScreenStop, ScreenAllow, Guardian}, dropped_input_tokens }`.
- `rust/fontaine-guardian/src/screen.rs` — `Screen` asks an Ollama `/api/chat` model one-token
  yes/no questions and reads risk from first-token logprobs (`ask`, `risk_from`). `run` hard-codes
  the stop read, the hardened allow read and the steering read, the thresholds `t_stop`,
  `t_allow`, `steering_veto`, and one `timeout`. Prompts and questions are fixed in code because
  thresholds are calibrated against their exact wording (`Question`, `SYSTEM_PROMPT`,
  `HARDENED_SYSTEM_PROMPT`, `STEERING_QUESTION`).
- `rust/fontaine-core/src/config.rs:444` `GuardianScreenSection` / `:1426` `GuardianScreen`, merged
  at `:1939`, validated at `:1134` and `check_screen` (`:2500`). Scope rules:
  `rust/fontaine-core/src/config_scope.rs:117,166-172` (everything user-only except
  `shadow = true`). `/config` rows: `rust/fontaine-core/src/config_catalog.rs:341-362`.
  `rust/fontaine-core/src/user_config.rs:205` and `agent/models.rs:222-242` (a model in use by the
  screen cannot be removed).
- `rust/fontaine-core/src/agent/setup.rs:159` `build_screen` and `:344` `attach_screen` (warm-up,
  `state.set_screen`). `rust/fontaine-core/src/gate.rs:150` `ScreenStage { screen, label, shadow }`.
- `rust/fontaine-core/src/permissions.rs:1443-1490` calls `cascade::adjudicate`, then
  `screen_stop` (`:1545`), `screen_allow` (`:1511`) or `settle`. `Screened::annotate` (`:391`)
  fills the audit fields `screen_risk`, `screen_allow_risk`, `screen_steering`, `screen_model`,
  `screen_latency_ms`, `screen_outcome` and `guardian_dropped_*` (`rust/fontaine-core/src/audit.rs:88-106`),
  which are mirrored in headless output (`rust/fontaine/src/headless.rs:425`,
  `docs/schemas/headless-v1.schema.json`), telemetry (`rust/fontaine/src/telemetry/span_bridge.rs:336`,
  `fontaine.screen.count`) and `analytics/notebooks/fontaine-overview.json`.
- `rust/fontaine-evals/src/live.rs:149` `add_screen` and `:395` `adjudicate` drive the cascade for
  `--screen live:<account>/<model>` with `--stop-threshold`, `--allow-threshold` and
  `--steering-veto` (`rust/fontaine-evals/src/runner.rs:116-124,419-445`). The cascade scorecard
  reports `llm_call_rate`, `allowed_latency_ms` and related metrics (`runner.rs:1532`).
- Docs: `docs/guardian.md` "The screen stage" (`:260-338`), `docs/configuration.md#guardianscreen`,
  `docs/headless.md:183`, `docs/evals.md` (cascade scorecards), `docs/permissions.md`.
- Precedent plans: `tasks/completed/cascade_screen_stage_plan.md`,
  `tasks/completed/screen_allow_path_plan.md`.

## Design

### Shape

```
                 ┌──────────────── fail-closed root (Rust) ────────────────┐
request ──► cascade::adjudicate                                            │
                 │  budget + timeout + "must return a host Verdict"        │
                 │  abstain → Deny     monotone: an LLM non-allow in hand  │
                 │                     beats a script allow                │
                 │        ┌──────── Rune script `guardian(call, params)` ─┐│
                 │        │ read(call, "stop").await  ─► Ollama logprob   ││
                 │        │ llm(call).await           ─► shared Guardian  ││
                 │        │ first_decisive([..]) / std::future::join(..)  ││
                 │        │ escalate / deny / abstain / allow_below       ││
                 │        └───────────────────────────────────────────────┘│
                 └─────────────────────────────────────────────────────────┘
```

The composition runs exactly where the screen/guardian runs today: after the floor, rules, modes,
containment and hooks, and not on a paused breaker. Nothing about the floor changes.

### Where it lives

There is no new crate. The code lives in `fontaine-guardian`, behind a new non-default cargo
feature, `composition = ["dep:rune"]`, which `fontaine-core` enables. An embedder that does not
enable the feature keeps today's dependency-light crate with the plain `Guardian`. `rune` is pinned
exactly (`rune = "=0.14.2"`) in `rust/Cargo.toml`.

```
fontaine-guardian/src/
  screen.rs            -> the Ollama logprob reader only: `Reader`, `Read`, `Prompt`, `Question`
  composition/
    mod.rs             Composition (compiled unit + context + reads + params), adjudicate(), Composed
    host.rs            the Rune `guardian` module: Call, Score, Verdict, read, llm, first_decisive, ctors
    eval.rs            per-call evaluation context: shared LLM future, read log, dropped estimate
    preset.rs          built-in presets (name -> source, declared reads, param schema)
    screen.rn          the `screen` preset, include_str!'d
```

`cascade.rs` is replaced by `composition/`, and `lib.rs` re-exports the new types.

### The reader (`screen.rs`)

`Screen::run`'s fixed three-read logic is removed. What remains is one reusable read:

```rust
pub enum Prompt { Plain, Hardened }                  // SYSTEM_PROMPT / HARDENED_SYSTEM_PROMPT
pub enum Question { EmployeeAllowed, OutsideRequest, Steering }  // Steering = STEERING_QUESTION, yes is risky
pub struct Read { pub model: String /* label */, pub prompt: Prompt, pub question: Question }
pub struct Reader { client, endpoint: Url, model: String, context_tokens: u32, profile: Profile, timeout: Duration }
impl Reader {
    pub async fn warm_up(&self) -> Result<(), String>;
    pub async fn read(&self, read: &Read, request: &AdjudicationRequest) -> Result<f64, String>; // risk; timeout inside
}
```

`ask`, `risk_from`, `screen_message` and the calibration constants keep their exact bytes. Because
those strings are what the thresholds were fitted to, the existing message tests stay and keep
passing. `Prompt::Hardened` is valid only with `Question::EmployeeAllowed`, the only wording its
threshold was fitted to, and the load-time check rejects any other combination.

### The script contract

Script entry point: `pub async fn guardian(call, params)`. `params` is a Rune object built from
`[guardian.composition.params]` (numbers, strings, booleans). The host module `guardian` (all
functions are `use`-able as `guardian::…`; the prelude imports them) exposes:

| Item | Returns | Notes |
|---|---|---|
| `call.tool()`, `call.destructive()` | `String`, `bool` | Read-only facts of the call; no arguments are exposed (the script decides by reads, not by matching arguments — no regex on tool arguments). |
| `read(call, name)` | `Future<Result<Score, String>>` | `name` is a declared read. The score is recorded under `name` for the audit record either way; an undeclared name is an `Err`. |
| `score.value()` | `f64` | |
| `llm(call)` | `Future<Verdict>` | The LLM guardian. Every call in one evaluation shares **one** request (a `futures::future::Shared` in the evaluation context), so the script can mention it on several paths without paying twice. It never abstains: its fail-closed outcomes are Deny verdicts. |
| `first_decisive(futures)` | `Future<Verdict>` | Polls all futures concurrently; returns the first non-abstain verdict; when several are ready on the same poll, list order wins; drops the rest; all abstain → abstain. |
| `std::future::join(..)` | Rune native | For reads the script wants together. |
| `escalate(node, reason)`, `deny(node, reason)`, `abstain()` | `Verdict` | `node` names the deciding node in the audit record's `rule`. |
| `allow_below(node, score, threshold)` | `Verdict` | Allow if `score.value() < threshold`, else abstain. The only script-side way to an allow. |
| `verdict.is_abstain()`, `verdict.is_allow()` | `bool` | For sequencing. |

`Call`, `Score` and `Verdict` are host `Any` types with no Rune constructors and no settable
fields, so a script cannot forge them. An `Allow` can only come from `llm()` (it carries the
`Adjudication`) or from `allow_below` applied to a `Score` that a declared read produced. This
replaces a static "can this tree allow?" analysis with a structural guarantee.

### The fail-closed root (`composition::adjudicate`)

```rust
pub async fn adjudicate(c: &Composition, guardian: Arc<Guardian>, request: Arc<AdjudicationRequest>) -> Composed;

pub struct Composed {
    pub decided: Decided,
    pub reads: Vec<ReadRecord>,        // name, model label, Ok(score) | Err(reason), latency_ms
    pub outcome: Outcome,              // what the script came to: Stop | Deny | Allow | Pass | Failed(reason)
    pub dropped_input_tokens: Option<u64>,
    pub llm_usage: Option<Usage>,      // a completed LLM request the final verdict did not use
}
pub enum Decided {
    Guardian(Box<Adjudication>),       // the LLM decided (also every shadow-mode call)
    Escalate { node: String, reason: String },
    Deny { node: String, reason: String, fail_closed: Option<FailClosed> },
    Allow { node: String, score: f64, threshold: f64 },
}
```

The rules below are in Rust and outside the script's reach:

1. **Runs bounded.** The VM runs through `Vm::send_execute(["guardian"], (call, params))` wrapped
   in `rune::runtime::budget::with(INSTRUCTION_BUDGET, …)` and in
   `tokio::time::timeout(guardian timeout + read timeout + COMPOSITION_SLACK, …)`. Both constants
   ship in code (the 2026-10-21 "guardian constants are not config keys" decision). A fresh `Vm`
   is made per call; the session measured about 0.1 µs for that and about 0.8 µs for a
   screen-sized evaluation.
2. **Fail-closed.** A VM error, budget exhaustion, a timeout, a panic in a host function, or a
   return value that is not a host `Verdict` becomes `Decided::Deny { node: "composition",
   fail_closed: Some(Error | Timeout) }`. Error text explains the deny; it never changes it.
3. **Abstain at the root is a deny.** (`fail_closed: Some(Unparseable)`, reason "the composition
   reached no verdict"). The `screen` preset never abstains at the root, because its last resort
   is `llm()`.
4. **Monotone over verdicts in hand.** If the script returns a non-LLM `Allow` while a
   completed `llm()` result in this evaluation is not an allow, the root returns that LLM verdict
   (`Decided::Guardian`). A computed denial is never turned into an allow, which matches today's
   "a guardian verdict in hand wins". A script `Escalate`/`Deny` is kept even when an LLM allow
   is in hand, since that is the stricter outcome.
5. **Spend is never lost.** When the evaluation ends with the shared LLM request started but not
   finished, it is dropped (cancelled) and `dropped_input_tokens` is set to
   `Guardian::dropped_estimate` (as today). When it finished but the final verdict is not
   `Guardian`, its usage is returned in `llm_usage` so the pipeline adds it to the session's
   usage.
6. **Shadow.** With `shadow = true`, the root joins the script with the shared LLM request,
   records the script's would-be `outcome`, and always decides by `Decided::Guardian`. The
   script's `llm()` is the same shared request, so shadow mode costs no extra guardian call.

### The `screen` preset (`composition/screen.rn`)

It declares reads `stop` (`Plain`, `params.question`, default `employee_allowed`), `allow`
(`Hardened`, `employee_allowed`) and `steering` (`Plain`, `Steering`), all on the composition's
`model`. Its logic is today's `Screen::run` plus `cascade::adjudicate`:

```rune
pub async fn guardian(call, params) {
    if params.concurrent {
        first_decisive([llm(call), screen(call, params)]).await
    } else {
        let v = screen(call, params).await;
        if v.is_abstain() { llm(call).await } else { v }
    }
}

async fn screen(call, params) {
    let (stop, allow, steering) = if params.allow_threshold.is_some() {
        std::future::join((read(call, "stop"), read(call, "allow"), read(call, "steering"))).await
    } else { (read(call, "stop").await, Err(()), Err(())) };
    let stop = match stop { Ok(s) => s, Err(_) => return abstain() };
    if let Some(t) = params.stop_threshold { if stop.value() >= t { return escalate("screen.stop", `risk ${stop.value()}`); } }
    match (params.allow_threshold, allow, steering) {
        (Some(t), Ok(a), Ok(s)) if s.value() < params.steering_veto => allow_below("screen.allow", a, t),
        _ => abstain(),
    }
}
```

(This is illustrative; the implementer writes valid Rune 0.14. Shadow mode always runs the allow
reads so their scores are logged, as `build_screen` does today.) Preset params, checked in Rust at
load with today's ranges and messages: `question`, `stop_threshold` (0, 1], `allow_threshold`
(0, 1), `steering_veto` (0, 1], default `DEFAULT_STEERING_VETO`, and `concurrent` (bool). When
`concurrent` is unset, it defaults to `false` if the guardian is priced (`Settings::priced`) and
`true` otherwise (#136). A non-shadow preset with neither threshold is an error, as today.

When any read fails, the preset abstains on the screen branch, so the LLM decides. Today a failed
read makes the whole screen `Unavailable`. This keeps that.

### Configuration

`[guardian.screen]` is removed (nothing is released, so there is no shim) and replaced by:

```toml
[guardian.composition]
preset = "screen"                 # a built-in preset, or:
# script = "guardian.rn"          # a Rune file; relative paths resolve against the user config dir
model = "local/qwen3:4b-instruct-2507"  # `<account>/<model>` of an `ollama` account; default model of every read
shadow = true                     # default true
timeout_ms = 2000                 # each read's bound (DEFAULT_SCREEN_TIMEOUT_MS)

[guardian.composition.params]     # preset params, or free-form values passed to a script
allow_threshold = 1.56e-6

[guardian.composition.reads.extra]   # scripts only; a preset declares its own reads
model = "local/other"             # optional, defaults to `model`
prompt = "plain"                  # plain | hardened
question = "outside_request"      # employee_allowed | outside_request | steering
```

Load-time checks (`config.rs` validation and `check_composition`, which replaces `check_screen`):
exactly one of `preset`/`script`; the preset name is known; preset params are known and in range;
every read model is `<account>/<model>` on an `ollama` account; `hardened` only with
`employee_allowed`; a script compiles against the host context with Rune's link checks on, so an
unknown function is a load error; and it defines `guardian` with two parameters. A composition that
fails these checks **stops startup** with a `ConfigError` naming the key, as an invalid `[network]`
does. It does not silently fall back to a guardian without its stop stage.

Scope (`config_scope.rs`): every `guardian.composition.*` key is user-only (user file, `--settings`,
untracked `settings.local.toml` of a trusted root, as `Rule::user_only` already resolves), except
`shadow`, which other scopes may set to `true` only (`Rule::shadow_true`, as today). A project
therefore cannot load a script, so a project-defined script can never mint an allow. A project
tighten-only script (with a host context that has no `allow_below` and no reads) is a follow-up
issue (see Decisions).

### Core wiring

- `gate.rs`: `ScreenStage` becomes `CompositionStage { composition: Arc<Composition>, readers:
  Vec<Arc<Reader>>, label: String, shadow: bool }`. `set_screen`/`screen()` and
  `first_screen_warning` are renamed. Subagents share the session state, so they run the same
  composition, as they share the screen today.
- `agent/setup.rs`: `build_composition` resolves each distinct read model to an Ollama account
  (`context_tokens`, base URL) and builds one `Reader` per model; `attach_composition` warms up
  each reader.
- `permissions.rs`: the call site passes `Arc`s. `Decided::Escalate` takes today's `screen_stop`
  path (prompt, or deny when headless, `tier = screen`, `rule = node`). `Decided::Deny` is a deny
  with `tier = screen`, `rule = node`, no prompt and no hook retry. `Decided::Allow` takes the
  `screen_allow` path. `Decided::Guardian` goes to `settle`. `llm_usage` goes through
  `add_guardian_usage`. A failed read warns once per session through `Approver::warn`, as an
  unavailable screen does today.
- Audit (`audit.rs`), headless output and schema: keep `screen_risk`, `screen_allow_risk` and
  `screen_steering`, filled from the reads named `stop`, `allow` and `steering`; add `screen_scores`
  (an object of every other read name → score). `screen_model` becomes the comma-joined distinct
  read models. `screen_outcome` gains `deny`; its values stay `stop`, `allow`, `pass` and
  `unavailable` (`unavailable` = a read failed or the script failed). The deciding node is the
  record's `rule`. The telemetry `fontaine.screen.count` keeps working from `screen.outcome`.
- `config_catalog.rs`, `user_config.rs` and `agent/models.rs`: move to the new keys. A model used
  by any read is "in use by the guardian's composition".

### Evals

`fontaine-evals` keeps `--screen live:<account>/<model>` with `--stop-threshold`,
`--allow-threshold` and `--steering-veto`, which now build the `screen` preset. It also gains
`--composition-script <path>` and repeatable `--composition-param key=value`, so any tree can be
scored. `live.rs::adjudicate` calls `composition::adjudicate`, and the cascade step records
`reads` and `outcome`, so `llm_call_rate`, `allowed_latency_ms` and the screen metrics are computed
for any composition. Scorecard JSON keys stay the same; the preset's `stop`/`allow`/`steering` map
to the existing `screen_risk`/`allow_risk`/`steering` fields.

## Implementation Steps

1. **Spike-check, then the dependency.** Add `rune = "=0.14.2"` to `rust/Cargo.toml` workspace
   deps and the `composition` feature to `fontaine-guardian/Cargo.toml`. Before building on it,
   confirm in a throwaway test that a host `async fn` taking `Vec<rune::runtime::Future>`
   (`first_decisive`) keeps `VmSendExecution` `Send` under `tokio::spawn`. If it does not, run the
   evaluation on a `tokio::task::LocalSet` inside `spawn_blocking` and record that in Decisions.
   Run `cargo deny check` and fix the license/advisory config for rune's dependency tree.
2. **Reader.** Refactor `screen.rs` into `Reader`/`Read`/`Prompt`/`Question::Steering`. Keep the
   wording and message tests; move the threshold logic out.
3. **Host module and evaluation context.** `composition/host.rs` and `composition/eval.rs`: the
   types and functions in the contract table, the shared LLM future, the read log, the
   drop-time dropped estimate, and `first_decisive`.
4. **Root.** `composition/mod.rs`: `Composition::compile(source, reads, params, preset?)` (link
   checks, entry check), `adjudicate` with rules 1–6, the constants `INSTRUCTION_BUDGET` and
   `COMPOSITION_SLACK`. Delete `cascade.rs`; update `lib.rs` exports.
5. **Preset.** `composition/screen.rn`, `composition/preset.rs` (declared reads, param schema and
   range checks, the `concurrent` default from `priced`). Port `fontaine-guardian/tests/cascade.rs`
   to the preset.
6. **Config.** `config.rs` (`GuardianCompositionSection`/`GuardianComposition`, merge, validation,
   `check_composition` including compile), `config_scope.rs`, `config_catalog.rs`,
   `user_config.rs`, `agent/models.rs`. Regenerate the config JSON schema if one is generated
   (`docs/schemas/`).
7. **Pipeline.** `gate.rs`, `agent/setup.rs`, `agent.rs` re-exports, `permissions.rs` (the four
   `Decided` arms, `llm_usage`, warnings), `audit.rs`, `user_shell.rs` (record fields),
   `rust/fontaine/src/headless.rs`, `docs/schemas/headless-v1.schema.json`.
8. **Evals.** `fontaine-evals/src/live.rs` and `runner.rs` (new flags, preset construction, step
   recording); update `fontaine-evals/tests/live.rs` and `tests/screen.rs` if they touch the
   cascade types.
9. **Tests.** See Testing Strategy, and port `fontaine-core/tests/config.rs` (the
   `[guardian.screen]` block), `permissions_screen.rs`, `account_ops.rs:275`,
   `account_session.rs:245`, `common/mod.rs` and `rust/fontaine/tests/auto_headless.rs`.
10. **Docs and plan.** See Documentation. Add the Decisions lines below to the master plan's
    `### Guardian` subsection (at its end) and to this plan. File the follow-up issues named in
    Decisions.

## Files to Modify

- `rust/Cargo.toml`, `rust/fontaine-guardian/Cargo.toml`, `rust/fontaine-core/Cargo.toml`, `rust/deny.toml` (if needed)
- `rust/fontaine-guardian/src/{lib.rs,screen.rs}`, delete `cascade.rs`, new `composition/{mod.rs,host.rs,eval.rs,preset.rs,screen.rn}`
- `rust/fontaine-guardian/tests/cascade.rs` → `tests/composition.rs`
- `rust/fontaine-core/src/{config.rs,config_scope.rs,config_catalog.rs,user_config.rs,gate.rs,permissions.rs,audit.rs,user_shell.rs,agent.rs,agent/setup.rs,agent/models.rs}`
- `rust/fontaine-core/tests/{config.rs,permissions_screen.rs,account_ops.rs,account_session.rs,common/mod.rs}`
- `rust/fontaine/src/headless.rs`, `rust/fontaine/tests/auto_headless.rs`
- `rust/fontaine-tui/src/config_panel.rs` (its `guardian.screen` filter test)
- `rust/fontaine-evals/src/{live.rs,runner.rs}`, `rust/fontaine-evals/tests/live.rs`
- `docs/{guardian.md,configuration.md,headless.md,evals.md,permissions.md}`, `docs/schemas/headless-v1.schema.json`
- `tasks/fontaine_harness_plan.md` (Decisions)

## Trade-offs

- **Rune over data (TOML), a bespoke DSL, Lua (mlua/Luau), Starlark, JS or WASM.** The user's
  choice. Rune has native `async`/`.await` with lazy futures, so sequential evaluation is the
  default and concurrency is opt-in. It is pure Rust (no C build, and it fits the Windows
  cross-compile), with an instruction budget and `Send` execution. Measured in this session
  (release build): about 0.8 µs per screen-style evaluation with 3 async host calls (Luau: 1.3 µs);
  `join` of three 20 ms host calls took 21 ms against 63 ms sequential; raw interpretation is
  about 4–5× slower than Luau (`fib(27)` 82 ms against 18 ms), which is irrelevant next to model
  calls. The costs are a pre-1.0 dependency (pinned exactly) and the loss of static "can this tree
  allow?" analysis. Unforgeable host verdicts and the Rust root recover that guarantee
  structurally.
- **Host-minted verdicts over static analysis.** A Rune script cannot be analysed for which
  verdicts it yields. Making `Verdict`/`Score` host types with no constructors moves "only these
  leaves can allow" into the type system, and the fail-closed root needs no knowledge of the
  script.
- **One shared LLM request per evaluation** over a fresh call per `llm()`. It keeps shadow mode free
  and stops a script that names `llm()` on several branches from paying more than once.
- **No new crate** (user decision). A cargo feature keeps `fontaine-guardian` lift-out-able for
  embedders that don't want rune.
- **Read names declared in TOML** over model names in the script. The load step then knows every
  model, which drives warm-up, the Ollama-account check and the "model in use" guard without
  running the script.
- **Keeping the audit field names** over a fully generic `scores` map. Headless consumers, the
  analytics notebook and the scorecards keep working; only extra read names go to the new map.

## Decisions

- 2026-11-01: Rune (`=0.14.2`) is the composition language; no new crate, behind the
  `fontaine-guardian` `composition` feature (user).
- 2026-11-01: A composition that fails to load stops startup, rather than dropping to a guardian
  without its stop stage, because a silent fallback would remove a configured tightening.
- 2026-11-01: v1 loads scripts from user scope only; project scopes may only force `shadow = true`.
  The project-scope tighten-only script (a context without `allow_below` or reads, strictest-wins
  over the user composition) is a follow-up issue.
- 2026-11-01: The `screen` preset defaults to sequential (screen, then LLM) for a priced guardian
  and concurrent otherwise; `params.concurrent` overrides (#136).
- 2026-11-01: A script escalate or deny beats an LLM allow that is already in hand (the stricter
  outcome wins), where today an in-hand LLM verdict wins either way; an in-hand LLM non-allow still
  beats a script allow.

## Documentation

- `docs/guardian.md`: rewrite "The screen stage" as "Composing the guardian": the `screen` preset
  (today's text on the stop and allow paths, shadow calibration, cost), then "Writing a
  composition script" (the contract table, the fail-closed rules, sequential versus concurrent cost
  in the issue's race/seq table, and a worked example). Update the `session_facts` row's "screen
  stage" link.
- `docs/configuration.md`: replace `#guardianscreen` with `#guardiancomposition` (keys, reads,
  params, scope).
- `docs/headless.md`: the `gate_decision` row (`screen_scores`, `screen_outcome` = `deny`).
- `docs/evals.md`: cascade scorecards via the preset and `--composition-script`.
- `docs/permissions.md`: update its screen references.
- `CHANGELOG.md` is handled by the `pr` step.

## Testing Strategy

All of these are unit or integration tests with `wiremock` for the Ollama reads and
`rig_core::test_utils` / the existing fixtures for the guardian. No live APIs.

- **Host module** (`fontaine-guardian`, unit): a script that tries to build a `Verdict`/`Score`
  (a struct literal, `Verdict::Allow`, or an unknown constructor) fails to compile; a script that
  returns a string, unit or a number becomes Deny; `allow_below` with a score at the threshold
  abstains; `read` of an undeclared name is `Err`.
- **Root rules** (unit, `tokio::test` with paused time where useful): an infinite loop exhausts
  the budget → Deny; a script that sleeps past the timeout → Deny `Timeout`; root abstain → Deny;
  a script that returns a screen allow while a completed LLM deny is in hand → the LLM deny
  (monotone); a script that escalates with an LLM allow in hand → escalate; a host panic → Deny.
- **Concurrency and cost**: `first_decisive` returns the first decisive verdict and list order wins
  a same-poll tie; dropping an in-flight `llm()` sets `dropped_input_tokens`; a completed but unused
  LLM result is returned in `llm_usage`; `llm()` named twice sends one request (wiremock
  `expect(1)`); a sequential script runs reads in order, and `join` runs them together (paused-time
  latency assertion).
- **Shadow**: the script's verdict is recorded and the LLM decides; one LLM request.
- **Preset parity**: port every case in `fontaine-guardian/tests/cascade.rs` and
  `fontaine-core/tests/permissions_screen.rs` (stop, allow, pass, unavailable read, guardian in
  hand wins, shadow scores logged, dropped estimate) to the preset; add the priced → sequential
  default.
- **Config** (`fontaine-core/tests/config.rs`, `config_scope.rs` unit tables): every rejection
  that exists today, moved to the new keys; `preset` and `script` together or both missing;
  unknown preset or param; `hardened` with another question; a script compile error and a missing
  `guardian` entry are `ConfigError`s naming the key; project scope cannot set any key except
  `shadow = true`.
- **Pipeline** (headless `fontaine -p` integration, `rust/fontaine/tests/auto_headless.rs`): a
  script deny is a `tier = screen` deny with `rule` = the node name; the audit and headless records
  carry `screen_scores`. These go through the real pipeline because a mis-wired arm would be a
  silent permission bug.
- **Evals**: the existing offline cascade scorecard tests pass unchanged through the preset; add
  one `--composition-script` run.

## Open Questions

None that block implementation. The `Send`-ness of `first_decisive` is checked first, in step 1,
and the fallback is specified there.
