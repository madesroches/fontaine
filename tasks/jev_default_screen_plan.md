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
- Audit: `screen_model` is the composition's model labels (`composed.models()`).

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
  (constructor `Reader::systemone(client, SystemOneSettings)`; `Reader::new(client, ReaderSettings)` stays the Ollama constructor).
  `read()` dispatches on backend. `warm_up` is a no-op for SystemOne. `Reader::timeout()` works
  for both backends and replaces `reader.settings().timeout` in `Composition::read_timeout`
  (`composition/mod.rs:252`). The key is held in `rig_core::wire::Secret` (redacts in `Debug`, has
  `expose()`) and sent as a bearer token only over https or to loopback (same rule as the scorer).
- New `Question::Level5Employee`. `yes_is_risky`/`text()` are Ollama concepts, so `Question` gains
  `fn backend(self) -> BackendKind` (`Ollama` for the three yes/no questions, `SystemOne` for
  `Level5Employee`). Risk is `1 − p(within)`. `Read` gets `backend: BackendKind`, set from the account kind that `CompositionSpec` carries (see
  3). `compile` checks `question.backend()` against `Read.backend`
  (this also runs in `check_composition`, which compiles with no readers), and `Prompt::Hardened`
  stays valid only with `employee_allowed`. A mismatch is a `CompositionError` naming the read.
- The state is `screen_message`'s state (wrapper sentences and digest line removed), sent as
  `state`; the question text goes in `instructions`, the five levels in `criteria`. The request
  profile shaping is the same as the Ollama read, so redaction and delimiting match.
- Fail-closed: timeout, transport error, non-2xx, non-JSON, missing/out-of-range `p(within)` all
  return `Err(String)`; the preset script already turns a failed read into `abstain()` and the
  call goes to the guardian.
- The model id the server resolved (`typesafe/jev-1.14-20261014`) is returned beside the risk.
  `Reader::read` keeps returning `f64`; a new `Reader::read_detailed` returns `{ risk, resolved_model }`, used by the evaluator to record the pin (see 4).
- The optional 12-category audit read is **not** built here (issue: "could be optional"). Recorded
  as a Decision; the server's chosen `level` is not recorded.

### 2. The preset

- The Jev preset declares reads by question, using `Prompt::Plain`:
  `stop` = `Level5Employee`, `allow` = `Level5Employee`. The `stop` and `allow` reads are the same
  question on the same model, so the composition must send one request, not two: the underlying
  request future is shared, keyed by `(model, prompt, question)`. One slot and one `ReadRecord`
  are kept per declared name (`start_read` dedups by name; `Composed::score(name)` and
  `adjudicate`'s `allow_risk` from `score("allow")` rely on it), each cloned from the shared
  result; the request's cost is counted once. Equal reads thus return identical scores and keep
  `shadow` and `screen_*` audit fields consistent.
- `steering` read: dropped for a Jev model (steered-case evidence: 2–9 of 1,503 cross a margin
  vs 93 of 134 on qwen3; at the 0.10 margin none do). The preset chooses its reads from the model
  account kind: an `openrouter` model declares `stop` and `allow` only; an `ollama` model keeps the
  three existing reads, unchanged. A second script `screen_jev.rn` is selected by backend; the
  Ollama script stays byte-identical.
- Backend selection: `CompositionSpec` (`composition/mod.rs`) gains `kinds: BTreeMap<account,
  BackendKind>`, one entry per read model's account, filled by core; `declared_reads()` and
  `check_preset_params` read the kind from it and set `Read.backend`.
- Params per backend (the preset's KNOWN params gain `allow`):
  - Jev: `question` rejected (unknown-param error), `steering_veto` rejected, `concurrent`
    accepted, `allow` accepted, `stop_threshold`/`allow_threshold` accepted.
  - Ollama: `allow` rejected (unknown-param error); `question`, `steering_veto`, `concurrent` and
    the thresholds as today.
- Defaults for the Jev preset, fitted on the synthetic corpus only (never the golden set), with the
  0.10 margin for nondeterminism:
  - `allow_threshold = 0.20` (synthetic zero-miss skip 0.30 − 0.10). It is on by default.
  - `stop_threshold = 0.34`: the "stop@5%" value in `research/analysis-jev-improvements.md`
    (~line 790, "skip 0.30, stop@5% 0.34"), derived with `transfer` in
    `research/typed-decision-screen/scripts/jev_synth.py` over the committed
    `research/typed-decision-screen/results/jev/synthetic/level5_employee.json`. A stop default is safe by construction (it only
    escalates), but off-domain it stopped 28–88% of allowed calls on synthetic thresholds, so
    the offer's wording recommends `shadow = true` first (below).
  - `allow = false` param: an explicit off value for the allow path (bool, default true on a Jev
    model). `allow = false` wins; `allow_threshold` is then ignored without error.
  - `shadow` default: the field today defaults to `true` globally. For the Jev preset *when set
    through setup* the offer writes `shadow = false`. A hand-written `[guardian.composition]
    preset = "screen"` with no `shadow` key keeps `shadow = true` (the existing, safe key default),
    so merely configuring the preset never enforces; the offer is what turns enforcement on.
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
  `base_url` (default `https://openrouter.ai/api/v1`, the same base URL that serves chat) must be
  http(s) with a host; the SystemOne endpoint is derived by joining `systemone` onto that
  `/api/v1` base URL, so one `base_url` serves chat and the screen. The credential
  must resolve now (`models::credential` → `Found`), checked as a separate step in `load` after
  `check_composition` (which lacks env and stored credentials), using `params.env` and
  `params.data_dir` as `resolve_telemetry` does; else exit code 2 naming
  `guardian.composition.model` and the account ("no OpenRouter key; set `api_key_env` or run
  `/login`"). No silent fallback.
- `GuardianComposition::spec()` takes the configured accounts to fill `CompositionSpec.kinds`;
  `check_composition` looks up each read model's account (and its kind) before calling
  `spec.read_models()`. All `spec()` callers pass the accounts: `check_composition`,
  `agent/setup.rs`, `fontaine/src/headless.rs`.
- `model` stays required for any preset, as today (`config.rs:2692`); there is no default model
  resolution. The offer writes it.
- `build_reader` becomes `build_readers`-by-kind: Ollama as today; OpenRouter builds
  `Reader::systemone` with the account's network settings client and resolved credential.
- Audit: `screen_model` records the pinned resolved id. The SystemOne read returns the resolved id
  (`typesafe/jev-1.14-20261014`), stored in `ReadRecord.model`, so `composed.models()` feeds
  `screen_model` (`permissions.rs`) per call. The tracing span stays on the label. Update the doc
  comments on `screen_model` (`audit.rs`: the resolved id has no account prefix) and
  `ReadRecord.model` (`eval.rs`). The allow explanation (`permissions.rs`) leaves out its "steering
  n/a" clause when there is no steering read. `screen_jev.rn` uses the node names `screen.allow` /
  `screen.stop`.
- Cost: the shared read carries its `usage.cost` (on `ReadRecord` or a `Composed` total), and
  `permissions.rs` records it once per shared request through the existing `add_guardian_usage`
  path, so `/cost` and `/usage` include it.

### 4. Setup offer

`composition_offer.rs`: add a Jev offer next to the Ollama benchmarks.
- `ollama_accounts` filters `kind == "ollama"` and both callers (`fontaine/src/auto_mode.rs`,
  `fontaine-tui/src/ui/commands/auto_mode_setup.rs`) use it to restrict listing, so Jev detection
  selects `openrouter` accounts separately, by credential alone (the `account_has` / `candidates`
  pattern in `models.rs`), with no listing call (an OpenRouter listing is a network call this
  check does not need). The Jev offer is made only when one
  exists. With none, setup does not offer it and prints a one-line hint to add an OpenRouter
  account via `/login` / `api_key_env` and re-run. The offer text says guardian state
  (task intent, recent user messages, tool name and arguments, project facts) is sent to
  OpenRouter / Typesafe, and that the allow path is on. It requires a yes; nothing is written
  without one.
- Accepting writes the composition section to the user file only: `[guardian.composition] preset =
  "screen"`, `model = "<account>/typesafe/jev-1.14"`, `shadow = false`. No account is created. No
  threshold keys: the defaults apply, so a later recalibration (new fit after shadow data)
  reaches users with no config rewrite. The posture choice (safe/fast) stays for Ollama; for Jev
  the single offer is the preset with defaults, and the proposal lists how to turn the allow path off
  (`allow = false`) and shadow first (`shadow = true`).
- `Offer` becomes an enum over Ollama (benchmark + posture + one threshold) and Jev (no posture,
  no threshold): `threshold()`, `proposal()`, `calibration_note()` and `nothing_to_offer()` handle
  both, and `write()` writes no threshold key for Jev.
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
- `docs/evals.md`: the new scorecards and the citation for the `stop_threshold` default.
- `docs/tui.md:461` (offer description), `docs/permissions.md:150-153` (`screen_model` is now the
  resolved id), `docs/guardian.md:263` (reads inherit Ollama account network settings).
- `tasks/fontaine_harness_plan.md` Decisions: new dated lines (append to the end of the relevant
  subsections) that supersede, by reference, the 2026-11-01 offer line (L624, "no threshold
  ships"), the 2026-10-21 "screeners deferred", the 2026-10-23 "nothing ships", and the 2026-10-24
  "off by default" (L567, which already has a supersession parenthetical: add the note to it) lines. The L575
  (2026-10-21) supersession note also marks its "thresholds are always recalibrated on Fontaine's
  golden set" clause superseded for the default fit (synthetic-only with a 0.10 margin).
  Add the supersession note in each old line's existing parenthetical style. Record as a new
  Decision, not a supersession, that the composition model must be an Ollama account (it lives
  only in code and `docs/configuration.md:527`). Also record: allow path default-on with the leak
  cases cited; Jev with no steering read; `shadow` stays `true` unless the offer writes `false`;
  one shared request for equal reads; no category read.
- `CHANGELOG.md` and `AGENTS.md` security-invariants fast-path wording: the exception for the
  composed guardian's allow (`allow_below` in a user-scope script such as the `screen` preset's
  `allow_threshold`, "a steering-hardened local screen") now covers a hosted Jev screen with a
  margin instead of steering-hardening. Update the phrase.

## Implementation Steps

1. Guardian: add `systemone.rs` (constants, body builder, parser, `ParseError`); move the scorer's
   copies; re-export; keep eval snapshots green.
2. Guardian: `Backend` in `Reader`, `Reader::systemone`, `Question::Level5Employee`,
   `BackendKind`, compile-time pairing, `read_detailed`; unit tests with wiremock.
3. Guardian: share the request future for equal reads in `composition` (key by model, prompt,
   question), keeping one slot and `ReadRecord` per declared name; cost counted once and recorded
   in the usage ledger via `add_guardian_usage` (test: one shared request adds its cost once).
4. Guardian: preset changes (backend-aware reads, `screen_jev.rn`, defaults, `allow` param,
   threshold-required rule only for Ollama models). `check_preset_params`
   (`composition/mod.rs:100-104`) gains a backend argument (derived from the model's account kind
   where known); update both callers (`config.rs:1185`, `composition_offer.rs:308-315`) so
   `params.allow` is accepted for a Jev backend and rejected as unknown for an Ollama one. Add
   `kinds` to `CompositionSpec`, update the `Read` struct literals (`config.rs`, `preset.rs`,
   `screen.rs`, `tests/composition.rs`) and the `spec()` callers.
5. Core: `check_composition` by backend, credential check, `build_reader` by account kind,
   `screen_model` pin, `Config` docs; `/config` read-only display unchanged.
6. Core: Jev offer in `composition_offer.rs` (the `Offer` enum) + `fontaine/src/auto_mode.rs`,
   `fontaine-tui/src/ui/commands/auto_mode_setup.rs` (posture match), `fontaine-tui/src/setup_prompt.rs`
   (posture stage) and `fontaine-tui/src/app.rs` if needed, wording, tests.
7. Evals: switch to the shared parser, Jev preset runner support, scorecard placeholders; update
   `runner.rs:114-120` (`--stop-threshold`/`--allow-threshold` "There is no default" help) and
   the `live.rs:158-159` comment (every read model is on an ollama account). In `runner.rs`, relax
   the threshold-required check for an openrouter screen model, record the effective default
   thresholds in `CascadeParams`, leave `steering_veto` unset for Jev and reject `--steering-veto`
   there.
8. Docs, Decisions, CHANGELOG, AGENTS.md wording.

## Files to Modify

- `rust/fontaine-guardian/src/{lib.rs, screen.rs, systemone.rs (new), composition/{mod.rs,preset.rs,eval.rs,host.rs,screen_jev.rn (new)}}`
- `rust/fontaine-core/src/{config.rs, config_scope.rs (tests), config_catalog.rs, agent/setup.rs, composition_offer.rs, permissions.rs}`
- `rust/fontaine/src/{auto_mode.rs, headless.rs}`, `rust/fontaine-tui/src/{app.rs, setup_prompt.rs, ui/commands/auto_mode_setup.rs}`
- `rust/fontaine-evals/src/{screen.rs, live.rs, runner.rs}`, tests under `rust/*/tests/`
- `docs/{guardian.md, configuration.md, evals.md, tui.md, permissions.md}`, `tasks/fontaine_harness_plan.md`, `CHANGELOG.md`, `AGENTS.md`

## Trade-offs

- Backend inside `Reader` (chosen) vs a trait object per read: the two backends differ only in
  transport, and `Composition` keys readers by label already; an enum keeps the crate small.
- Defaults in the preset (chosen) vs thresholds written to the user file by setup: defaults let a
  recalibration ship without rewriting configs, at the cost of config-less behavior change
  being possible; mitigated by the `shadow` default staying `true` outside the offer.
- Dropping the steering read on Jev (chosen) vs keeping a local steering read: the latter would
  make a local Ollama model a hidden requirement of the hosted default.

## Decisions

- AFK: the Jev `shadow` default is `true` for a hand-written section and `false` only when the
  offer writes it; chosen as the most conservative reading of the issue's "shadow defaults to
  false", since flipping the global default would change enforcement for existing users.
- AFK: no 12-category audit read now, and the server's chosen `level` is not recorded anywhere (no audit field exists for it; adding one is out of scope).
- AFK: no new `config_catalog.rs` row or `config_scope.rs` rule for `allow`: the existing `guardian.composition.params` Map row and user-only rule cover it by prefix, and a per-param row would fail `every_row_matches_a_settings_node`.
- AFK: `allow = false` wins over `allow_threshold` silently (a toggle, not a contradiction).
- AFK: no per-session egress notice at run time; disclosure is at the offer and in docs.
- AFK: the default stop threshold ships with the preset; the offer writes `shadow = false` and its
  wording mentions `shadow = true` as the option to check real traffic first, per the issue's
  escalation-rate warning.
- AFK: `model` stays required for any preset; a hand-written section must not start sending
  guardian state to OpenRouter without an explicit `model` (the offer writes it).
- AFK: account creation is out of scope for the Jev offer (deviation from the issue's "plus an
  OpenRouter account if there is none"); conservative, and avoids key entry in two UIs.

## Documentation

`docs/guardian.md`, `docs/configuration.md`, `docs/evals.md` as in Design 6; README unchanged.

## Testing Strategy

Unit/integration tests only, no live APIs (wiremock for `/v1/systemone`):
- `systemone` parser: valid; missing `p(within)`; out-of-range; non-JSON; unknown level;
  `1 − p(within)` arithmetic. Unknown level parses to `None` (current behavior,
  `fontaine-evals/src/screen.rs:412`). Request body test, including that the wrapper sentences
  and digest line are absent.
- Reader: timeout, HTTP 4xx/5xx, bearer key sent, key refused over plain http to a non-loopback
  host, key redacted in `Debug`. Each failure makes the screen abstain (composition test: call goes
  to the guardian). Fail-closed is a security invariant, so this is automated.
- Pairing: `Level5Employee` with `Read.backend = Ollama` and a yes/no question with
  `Read.backend = SystemOne` are compile errors naming the read.
- Preset: Jev defaults (`allow_threshold` 0.20, `stop_threshold` 0.34); `allow = false` disables
  the allow path; `params.allow` accepted for a Jev backend and rejected as unknown for Ollama
  (via `check_preset_params`); Ollama presets unchanged (existing tests stay green); equal reads
  issue one request (wiremock `expect(1)`), and both the `stop` and `allow` records and audit
  fields (`screen_risk`, `screen_allow_risk`) are populated from that one request.
- Config: `openrouter` model accepted; missing key → exit code 2 naming the key; no account →
  exit code 2; the default and a user-written `/api/v1` `base_url` both hit `/api/v1/systemone`;
  project scope cannot set `allow`, `allow_threshold` or `shadow = false`, but `shadow = true`
  holds.
- Audit: `screen_model` records the resolved id once a read resolved, the label before.
- Offer: Jev offer text mentions egress and allow-on; made only with a reachable `openrouter`
  account with a resolving credential, else the hint is printed; declining writes nothing;
  accepting writes the exact keys (no account) and `check_composition` passes.
- Eval parity: the scorer's `employee-levels` request body and parse results are unchanged
  (existing equality tests).

## Manual Verification

1. Run the cascade eval with the Jev preset on the golden, fresh and public sets and commit the
   scorecards; confirm the leak cases cited in docs. Same reason.
2. `fontaine auto-mode setup` in a scratch config dir with an OpenRouter account; accept; start
   `fontaine` and run a benign shell call; the audit log shows `tier = screen`, `rule =
   screen.allow` and `screen_model = typesafe/jev-1.14-20261014`. Wiring a TUI cannot be unit-tested.

## Open Questions

None.
