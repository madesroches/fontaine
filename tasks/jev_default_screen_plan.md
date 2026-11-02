# Jev as the Default Guardian Screen Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/375

## Overview

Make `typesafe/jev-1.14` (OpenRouter's `/v1/systemone`) the model of the `screen` preset of
`[guardian.composition]`, asked the `level5_employee` question, in place of a local Ollama
first-token logprob read. Research (`research/analysis-jev-improvements.md`) shows it catches more,
costs one hosted call (~$0.000027, ~150 ms), and is far harder to steer than qwen3-4b. The preset
ships default thresholds fitted on the synthetic corpus and an allow path that is on by default;
`fontaine auto-mode setup` / `/auto-mode-setup` (#372) offers it, because guardian state now leaves
the machine.

## Current State

- `fontaine-guardian/src/screen.rs`: `Reader` is Ollama-only (`/api/chat`, `logprobs`), with
  `ReaderSettings { endpoint, model, context_tokens, profile, timeout }`. `Question`
  (`employee_allowed`, `outside_request`, `steering`), `Prompt` (`plain`, `hardened`), `Read
  { model, prompt, question }`, and `screen_message()` (guardian blocks without wrapper sentences
  and digest line).
- `composition/preset.rs`: the `screen` preset declares three reads (`stop`, `allow` hardened,
  `steering`), takes `question`, `stop_threshold`, `allow_threshold`, `steering_veto`,
  `concurrent`; with `shadow = false` and no threshold it fails ("there is no default").
  `composition/screen.rn` is the script. `Composition::compile(spec, readers)` keys readers by
  model label.
- `fontaine-core/src/config.rs`: `check_composition` (~L2661) requires every read model's account
  to be `kind = "ollama"`; `GuardianCompositionSection.model` doc says so. Defaults: `shadow =
  true`, `timeout_ms = 2000` (`DEFAULT_SCREEN_TIMEOUT_MS`). `config_scope.rs` makes composition
  keys user-only except `shadow = true`.
- `fontaine-core/src/agent/setup.rs`: `build_reader` builds an Ollama `Reader` from the account's
  `base_url`; `build_composition` assembles readers.
- `fontaine-core/src/composition_offer.rs`: `BENCHMARKS` holds the qwen3 tag's thresholds; the
  offer writes `preset`, `model`, `shadow = false` and one threshold.
- `fontaine-core/src/models.rs`: `credential()` resolves an account's key (`api_key_env` or the
  stored `/login` key).
- `fontaine-evals/src/screen.rs`: the Jev scorer: request body builders for the `employee-levels`
  framing (`EMPLOYEE_LEVELS_QUESTION`, `EMPLOYEE_LEVELS`, key `q`), `parse_employee_levels` reading
  `p(within)`, and `Failure`.
- Audit: `screen_model` is the composition's model labels (`composition/mod.rs:431`).

## Design

### 1. A `systemone` read backend in `fontaine-guardian`

- New module `systemone.rs`, dependency-light (reqwest/serde_json already present). It owns what
  the scorer currently owns for the `employee-levels` framing and the scorer then imports it, so
  there is one copy: `EMPLOYEE_KEY`, `EMPLOYEE_LEVELS_QUESTION`, `EMPLOYEE_LEVELS`, a
  `request_body(model, state, ..)` builder, and `parse_levels(body) -> Result<LevelAnswer,
  ParseError>` (`within` probability validated to `[0,1]`, the server's `choice` level, the
  resolved `model` id and `usage.cost`). `ParseError` carries the scorer's reason strings
  (`unparseable`) so the scorer maps it to `Failure` without losing its report vocabulary.
- `Reader` becomes backend-aware: `ReaderSettings` stays the Ollama settings; a sibling
  `SystemOneSettings { endpoint, model, api_key, profile, timeout }` and
  `enum Backend { Ollama(ReaderSettings), SystemOne(SystemOneSettings) }` inside `Reader`
  (constructor `Reader::systemone`). `read()` dispatches on backend. `warm_up` is a no-op for
  SystemOne. The key is held in the existing secret wrapper (`fontaine_tools::websearch::ApiKey`
  is not reachable from this crate; define a small `Secret` newtype with redacting `Debug`) and
  sent as a bearer token only over https or to loopback (same rule as the scorer).
- New `Question::Level5Employee`. `yes_is_risky`/`text()` are Ollama concepts, so `Question` gains
  `fn backend(self) -> BackendKind` (`Ollama` for the three yes/no questions, `SystemOne` for
  `Level5Employee`). Risk is `1 − p(within)`. Pairing is checked when the composition compiles:
  the reader registered for a read's model label must be of the question's backend, and
  `Prompt::Hardened` stays valid only with `employee_allowed`. A mismatch is a `CompositionError`
  naming the read.
- The state is `screen_message`'s state (wrapper sentences and digest line removed), sent as
  `state`; the question text goes in `instructions`, the five levels in `criteria`. The request
  profile shaping is the same as the Ollama read, so redaction and delimiting match.
- Fail-closed: timeout, transport error, non-2xx, non-JSON, missing/out-of-range `p(within)` all
  return `Err(String)`; the preset script already turns a failed read into `abstain()` and the
  call goes to the guardian.
- The model id the server resolved (`typesafe/jev-1.14-20261014`) is returned beside the risk.
  `Reader::read` keeps returning `f64`; a new `Reader::read_detailed` returns `{ risk, resolved_model,
  level }`, used by the evaluator to record the pin (see 4).
- The optional 12-category audit read is **not** built here (issue: "could be optional"). Recorded
  as a Decision; the `level` the server returns is recorded instead, cheaply, as a score label.

### 2. The preset

- `Read` gains nothing; the Jev preset declares reads by question:
  `stop` = `Level5Employee`, `allow` = `Level5Employee`. The `stop` and `allow` reads are the same
  question on the same model, so the composition must send one request, not two: reads with equal
  `(model, prompt, question)` share one request (the host already shares per read *name*; extend
  the key to the read tuple). Equal reads thus cost one call and return identical scores. This is
  also required for `shadow` and `screen_*` audit fields to stay consistent.
- `steering` read: dropped for a Jev model (steered-case evidence: 2–9 of 1,503 cross a margin
  vs 93 of 134 on qwen3; at the 0.10 margin none do). The preset chooses its reads from the model
  account kind: an `openrouter` model declares `stop` and `allow` only; an `ollama` model keeps the
  three existing reads, unchanged. `screen.rn` treats a missing `steering` read as "no veto"
  (`declared_reads` is host-visible; add a host function `has_read(name)` or branch on a preset
  param `steering_veto` being unset; the implementer picks the smaller change — prefer a second
  script `screen_jev.rn` selected by backend, to keep the Ollama script byte-identical).
- Backend selection: `preset::declared_reads` needs the model's account kind. `CompositionSpec`
  gains `model_kinds: BTreeMap<String /*account*/, ModelKind>` supplied by core
  (`GuardianComposition::spec`), or simply `Read.backend` resolved by core before compile. Use the
  latter: `Read` gets `backend: BackendKind`, set by `declared_reads(name, model, backend, params)`.
- Defaults for the Jev preset, fitted on the synthetic corpus only (never the golden set), with the
  0.10 margin for nondeterminism:
  - `allow_threshold = 0.20` (synthetic zero-miss skip 0.30 − 0.10). It is on by default.
  - `stop_threshold`: the 5%-synthetic-budget stop value computed by
    `research/typed-decision-screen/scripts/jev_analyze.py` from the committed results; the
    implementer records the number and the command in `docs/evals.md`. A stop default is safe by
    construction (it only escalates), but off-domain it stopped 28–88% of allowed calls on
    synthetic thresholds, so shadow mode first is the documented path (below).
  - `allow = false` param: an explicit off value for the allow path (bool, default true on a Jev
    model). With `allow_threshold` set it is rejected as contradictory only if both are given?
    No: `allow = false` simply wins and `allow_threshold` is ignored with no error, so a user-scope
    file can keep a custom threshold and toggle. (Simplest; recorded in Decisions.)
  - `shadow` default: the field today defaults to `true` globally. For the Jev preset *when set
    through setup* the offer writes `shadow = false`. A hand-written `[guardian.composition]
    preset = "screen"` with no `shadow` key keeps `shadow = true` (the existing, safe key default),
    so merely configuring the preset never enforces; the offer is what turns enforcement on. This
    narrows the issue's "shadow defaults to false for the Jev preset" to "the Jev offer writes
    `shadow = false`" — a Decision with the reason that a global default flip would silently
    change enforcement for existing Ollama users.
  - With `shadow = false`, the "no threshold" error in `resolve` no longer applies to a Jev model
    (defaults exist); it stays for an Ollama model.
- Project scope: unchanged rules (`config_scope.rs`): only `shadow = true` is settable. A
  repository can therefore force shadow (the repository-level off switch for the allow path) but
  never enable the allow path or change `allow`. No change to `Rule`s; add a test that
  `guardian.composition.params.allow` is user-only (it falls under the existing `params` rule).

### 3. Config and startup

- `[guardian.composition] model` accepts `openrouter/<model>`: `check_composition` accepts an
  account of kind `ollama` (for Ollama questions) or `openrouter` (for `Level5Employee`),
  derived from each declared read's question backend, not from a blanket rule. For `openrouter`:
  `base_url` (default `https://openrouter.ai/api`) must be http(s) with a host; the credential
  must resolve now (`models::credential` → `Found`), else exit code 2 naming
  `guardian.composition.model` and the account ("no OpenRouter key; set `api_key_env` or run
  `/login`"). No silent fallback.
- `Config` default for the `screen` preset's `model`: when `preset = "screen"` and `model` is
  absent, the model is `<account>/typesafe/jev-1.14` on the account named `openrouter` if one
  exists, else the single configured `openrouter`-kind account; if none or several, startup fails
  with exit code 2 naming `guardian.composition.model`. Presets still require a model in
  the existing branch only for non-openrouter use.
- `build_reader` becomes `build_readers`-by-kind: Ollama as today; OpenRouter builds
  `Reader::systemone` with the account's network settings client and resolved credential.
- Audit: `screen_model` records the pinned resolved id. The composition records
  `resolved_model` from the first successful SystemOne read (`typesafe/jev-1.14-20261014`) in a
  per-session cell and `permissions.rs:414` writes `resolved` instead of the configured label when
  known; before any read it falls back to the label. Data egress is flagged in the one-time
  per-session notice when the first Jev read happens? No — egress is disclosed at the offer and in
  docs; no extra runtime notice (Decision).

### 4. Setup offer

`composition_offer.rs`: add a Jev offer next to the Ollama benchmarks.
- `detect` also lists reachable `openrouter` accounts (or, when none exists, offers to add one:
  setup already has an account step; the Jev offer calls it). The offer text says guardian state
  (task intent, recent user messages, tool name and arguments, project facts) is sent to
  OpenRouter / Typesafe, and that the allow path is on. It requires a yes; nothing is written
  without one.
- Accepting writes to the user file only: `[guardian.composition] preset = "screen"`, `model =
  "<account>/typesafe/jev-1.14"`, `shadow = false`, and the account if it was missing. No
  threshold keys: the defaults apply, so a later recalibration (new fit after shadow data)
  reaches users with no config rewrite. The posture choice (safe/fast) stays for Ollama; for Jev
  the single offer is the preset with defaults, and the proposal lists how to turn the allow path off
  (`allow = false`) and shadow first (`shadow = true`).
- The existing flows for an Ollama offer are unchanged.

### 5. Evals

- `fontaine-evals` imports the guardian's `systemone` constants/parser; delete its copies. The
  `employee-levels` framing keeps working byte-identically (snapshot test pins the request body).
- Add Jev preset coverage to the cascade runner: `--screen live:openrouter/typesafe/jev-1.14`
  (via an `[accounts.*]` in the loaded config) with the defaults, so the preset's real thresholds
  are scored. `check_composition` in `live.rs` already runs the shared check.
- Produce `evals/scorecards/cascade-jev-level5-employee-kimi.json` (and one with haiku) on the
  golden set, plus a fresh-set and public-corpora report. These need live OpenRouter keys and are
  a human/manual step (evals are not a CI gate); the plan ships the runner support and doc
  placeholders, and the scorecards are added when run (Manual Verification).

### 6. Docs and Decisions

- `docs/guardian.md` ("The screen preset"): the Jev default; the two reads; no steering read on
  Jev; defaults and their fit (synthetic only, 0.10 margin); nondeterminism (60–75% of repeats
  differ by up to 0.22; a zero-miss threshold fitted on one pass does not hold); **data egress**
  to OpenRouter/Typesafe where the screen is enabled; known leak classes (`.env` reads, session
  facts, `kubectl get … secrets`; 1–3 must-not-clear cases cleared on the fresh set and public
  corpora); how to turn the allow path off (`allow = false`, lower `allow_threshold`, `shadow =
  true`; a repository can force `shadow = true`); an example fallback `script` (Jev read, falling
  back to an Ollama read on failure) as a Rune script that passes the soundness rules.
- `docs/configuration.md` (`[guardian.composition]`): `model` kinds, the `allow` param, defaults,
  startup failure.
- `docs/evals.md`: the new scorecards and the stop-threshold derivation.
- `tasks/fontaine_harness_plan.md` Decisions: new dated lines (append to the end of the relevant
  subsections) that supersede, by reference, the "no model or threshold ships" (2026-11-01
  offer line), "composition `model` must be an unauthenticated Ollama account", the 2026-10-21
  "screeners deferred", the 2026-10-23 "nothing ships", and the 2026-10-24 "off by default" lines.
  Add the supersession note in each old line's existing parenthetical style. Also record: allow
  path default-on with the leak cases cited; Jev with no steering read; `shadow` stays `true`
  unless the offer writes `false`; one shared request for equal reads; no category read.
- `CHANGELOG.md` and `AGENTS.md` security-invariants fast-path wording: the exception for the
  composed guardian's allow (`allow_below` in a user-scope script such as the `screen` preset's
  `allow_threshold`, "a steering-hardened local screen") now covers a hosted Jev screen with a
  margin instead of steering-hardening. Update the phrase.

## Implementation Steps

1. Guardian: add `systemone.rs` (constants, body builder, parser, `ParseError`); move the scorer's
   copies; re-export; keep eval snapshots green.
2. Guardian: `Backend` in `Reader`, `Reader::systemone`, `Question::Level5Employee`,
   `BackendKind`, compile-time pairing, `read_detailed`; unit tests with wiremock.
3. Guardian: shared requests for equal reads in `composition` (key by model, prompt, question).
4. Guardian: preset changes (backend-aware reads, `screen_jev.rn`, defaults, `allow` param,
   threshold-required rule only for Ollama models).
5. Core: `check_composition` by backend, credential check, default `model` resolution,
   `build_reader` by account kind, `screen_model` pin, `Config` schema docs and generated JSON
   schema (`schemars`), `/config` read-only display unchanged.
6. Core: Jev offer in `composition_offer.rs` + CLI/TUI wording, tests.
7. Evals: switch to the shared parser, Jev preset runner support, scorecard placeholders.
8. Derive the default `stop_threshold` from the committed synthetic results; record it.
9. Docs, Decisions, CHANGELOG, AGENTS.md wording.

## Files to Modify

- `rust/fontaine-guardian/src/{lib.rs, screen.rs, systemone.rs (new), composition/{mod.rs,preset.rs,eval.rs,host.rs,screen_jev.rn (new)}}`
- `rust/fontaine-core/src/{config.rs, config_scope.rs (tests), config_catalog.rs, agent/setup.rs, composition_offer.rs, permissions.rs, models.rs (visibility)}`
- `rust/fontaine-evals/src/{screen.rs, live.rs, runner.rs}`, tests under `rust/*/tests/`
- `docs/{guardian.md, configuration.md, evals.md}`, `tasks/fontaine_harness_plan.md`, `CHANGELOG.md`, `AGENTS.md`

## Trade-offs

- Backend inside `Reader` (chosen) vs a trait object per read: the two backends differ only in
  transport, and `Composition` keys readers by label already; an enum keeps the crate small.
- Defaults in the preset (chosen) vs thresholds written to the user file by setup: defaults let a
  recalibration ship without rewriting configs, at the cost of config-less behavior change
  being possible; mitigated by the `shadow` default staying `true` outside the offer.
- Dropping the steering read on Jev (chosen) vs keeping a local steering read: the latter would
  make a local Ollama model a hidden requirement of the hosted default.
- Reusing the scorer's parser by moving it into the guardian (chosen) vs evals depending the other
  way: the dependency direction (evals → guardian) forces the move.

## Decisions

- AFK: the Jev `shadow` default is `true` for a hand-written section and `false` only when the
  offer writes it; chosen as the most conservative reading of the issue's "shadow defaults to
  false", since flipping the global default would change enforcement for existing users.
- AFK: no 12-category audit read now; the server's chosen `level` is recorded instead.
- AFK: `allow = false` wins over `allow_threshold` silently (a toggle, not a contradiction).
- AFK: no per-session egress notice at run time; disclosure is at the offer and in docs.
- AFK: the default stop threshold ships with the preset but the offer's wording recommends
  starting in shadow mode (`shadow = true`) until real traffic is checked, per the issue's
  escalation-rate warning.

## Documentation

`docs/guardian.md`, `docs/configuration.md`, `docs/evals.md` as in Design 6; README unchanged.

## Testing Strategy

Unit/integration tests only, no live APIs (wiremock for `/v1/systemone`):
- `systemone` parser: valid; missing `p(within)`; out-of-range; non-JSON; unknown level;
  `1 − p(within)` arithmetic. Request body snapshot (insta), including that the wrapper sentences
  and digest line are absent.
- Reader: timeout, HTTP 4xx/5xx, bearer key sent, key refused over plain http to a non-loopback
  host, key redacted in `Debug`. Each failure makes the screen abstain (composition test: call goes
  to the guardian). Fail-closed is a security invariant, so this is automated.
- Pairing: `Level5Employee` with an Ollama reader and a yes/no question with a SystemOne reader are
  compile errors naming the read.
- Preset: Jev defaults (`allow_threshold` 0.20, stop default); `allow = false` disables the allow
  path; Ollama presets unchanged (existing tests stay green); equal reads issue one request
  (wiremock `expect(1)`).
- Config: `openrouter` model accepted; missing key → exit code 2 naming the key; no account →
  exit code 2; default model resolution (none/one/several accounts); project scope cannot set
  `allow`, `allow_threshold` or `shadow = false`, but `shadow = true` holds.
- Audit: `screen_model` records the pinned id once a read resolved, the label before.
- Offer: Jev offer text mentions egress and allow-on; declining writes nothing; accepting writes
  the exact keys and `check_composition` passes.
- Eval parity: the scorer's `employee-levels` request body and parse results are unchanged
  (existing snapshots).

## Manual Verification

1. With a real OpenRouter key: `cargo run -p fontaine-evals -- --dataset ../evals/dataset
   --guardian screen:typesafe/jev-1.14 --screen-url https://openrouter.ai/api
   --screen-api-key-file <key> --screen-framing employee-levels`. Expect the same catch/benign
   figures as `evals/scorecards/screen-jev-1.14-employee-levels.json` (within nondeterminism).
   Not automated: needs a paid external service and is nondeterministic.
2. Run the cascade eval with the Jev preset on the golden, fresh and public sets and commit the
   scorecards; confirm the leak cases cited in docs. Same reason.
3. `fontaine auto-mode setup` in a scratch config dir with an OpenRouter account; accept; start
   `fontaine` and run a benign shell call; the audit log shows `tier = screen`, `rule =
   screen.allow` and `screen_model = typesafe/jev-1.14-20261014`. Wiring a TUI cannot be unit-tested.

## Open Questions

None that the code cannot settle; the exact `stop_threshold` default is computed in step 8.
