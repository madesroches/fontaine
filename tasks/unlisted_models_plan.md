# Models Not in rig's Catalog Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/515

## Overview

A `claude-subscription` account only offers a model whose canonical id rig's built-in catalog
lists with an output limit; every other row is refused and silently dropped from the picker. Each
new Claude point release therefore disappears until the pinned rig version catches up (seen with
`haiku` → `claude-haiku-5-5` on rig-core 0.45.0). This plan builds usable model facts for an
unlisted canonical id from the closest listed model of the **same family**, overlaid with the
capability flags the CLI reports for that row, with pricing left unknown. An id with no listed
family stays refused. The user is warned once whenever a derived model is resolved, and a refused
row is shown with its reason in `/model`, `/guardian-model` and `fontaine models` instead of being
hidden.

## Current State

- `fontaine-core/src/providers/claude_subscription/cli.rs:224` — `ModelRow { id, canonical,
  credits }`. `model_rows` (`cli.rs:682`) reads `value`, `resolvedModel` and the credit markers
  from the CLI's `initialize` answer and ignores every other field.
- `fontaine-core/src/providers/claude_subscription/mod.rs:405` — `entry_of` refuses a credit row,
  then takes `Catalog::builtin().facts(anthropic, canonical)` and refuses the row unless the bound
  spec has `max_output_tokens`.
- `mod.rs:382` — `Connected::list` stores every row's `Entry` in `discovered` and returns only the
  usable ids. `unavailable_reason` (`mod.rs:375`) reports a refused row's reason, which only
  `models.rs:584` (`not_offered`) reads, when a named model fails to resolve.
- `mod.rs:316` — `Connected::model` binds the route's `ModelFacts` to rig's Anthropic Messages
  wire (`with_facts`); rig derives `max_tokens`, reasoning refusals and compat quirks from that
  spec, and Fontaine's compaction and `/context` read `completion.spec()` (`agent.rs:1093`,
  `session.rs:545`, `child_runner.rs:648`, `side_question.rs:134`).
- `fontaine-core/src/models.rs:849` — `AccountListing { account, kind, models }`.
  `Switcher::listings` (`models.rs:955`) reuses the main account's already-fetched ids
  (`main: Option<(&ModelRef, &[String])>`) rather than re-listing it. `Models.main_listing` and
  `Switcher::main` carry those ids as `Vec<String>`.
- `fontaine-tui/src/model_picker.rs` — `Group` holds `Result<Vec<String>, String>`; rows are
  `Heading`, `Model`, `Failed`. `fontaine-core/src/model_report.rs` builds the `fontaine models`
  and `models`-tool report from the same `AccountListing`s.
- `Models.notices` (`models.rs:351`) is the startup channel for one-time warnings, printed by the
  TUI and `fontaine models`; headless `announce` (`headless.rs:684-690`) prints it only in
  Auto/Afk/Plan mode.
- rig-core 0.46.0 catalog: every current Anthropic entry carries `details.family`
  (`claude-opus`, `claude-sonnet`, `claude-haiku`, `claude-fable`) and a `release_date`; the
  deprecated `claude-*-4-0/4-1` rows have no family.
- The real CLI (2.1.319, observed with an `initialize` control request) reports per row:
  `supportsEffort`, `supportedEffortLevels` (`["low","medium","high","xhigh","max"]`),
  `supportsAdaptiveThinking`, `supportsThinkingOff`, plus `displayName`, `description`,
  `supportsFastMode`, `supportsAutoMode`.
- Plan Decisions in force: "Protocol shapes outside the qualified matrix (… unlisted models …)
  fail closed" (Providers, 2026-11-04); `docs/claude-subscription.md` says an unknown model "is
  refused rather than guessed at"; Sessions 2026-10-24: an unlisted model gets no fallback
  `ModelSpec` and is never compacted.

## Design

### 1. Capability flags on `ModelRow`

`ModelRow` gains `capabilities: Capabilities`:

```rust
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Capabilities {
    /// `supportedEffortLevels`, when the row lists them; `None` when absent or not an array.
    pub effort_levels: Option<Vec<String>>,
    /// `supportsAdaptiveThinking`, when present and a boolean.
    pub adaptive_thinking: Option<bool>,
    /// `supportsThinkingOff`, when present and a boolean.
    pub thinking_off: Option<bool>,
}
```

`model_rows` fills it; a field of the wrong type reads as absent (it is only an overlay, see 2).
Listed models ignore these flags: the catalog's reviewed facts stay authoritative for them.

### 2. Derived facts (`fontaine-core/src/providers/derived_facts.rs`, new)

A provider-neutral function, so another kind can adopt it later without moving code:

```rust
pub struct Derived {
    pub facts: ModelFacts,
    /// The catalog id the facts were taken from.
    pub from: String,
}

pub fn derive(catalog: &Catalog, vendor: ProviderId, model: &str, overlay: &Capabilities)
    -> Option<Derived>;
```

Rule:

1. The **family** of `model` is the longest `details.family` among `vendor`'s catalog entries such
   that `model` starts with `<family>-`. No such family → `None` (the "never guess" boundary:
   `claude-quantum-9` stays refused).
2. The **base** is the family's entry that is not deprecated, has `max_output_tokens`, and has the
   latest `release_date` (ties broken by the greater id); entries without a release date rank
   last. No such entry → `None`.
3. The spec is the base's clone with `id` and `display_name` set to `model`, `pricing = None`,
   `details.cost` reset to default, and `details.release_date`/`last_updated`/`canonical_model_id`
   cleared.
4. Overlay (only what the row reports):
   - `levels` maps the `effort_levels` strings `minimal|low|medium|high|xhigh|max` to rig's
     `Effort` and **drops any string it does not know** (never claim a level it cannot name); an
     `effort_levels` list whose known subset is empty counts as absent (base reasoning kept).
   - If `effort_levels` (known subset non-empty) is present or the base's reasoning is `Listed`,
     the result is `Listed` with `can_disable = thinking_off.unwrap_or(base's Listed
     can_disable, else false)`; otherwise the base's reasoning is kept and `thinking_off` is
     ignored. With `effort_levels`, `reasoning = Listed { levels, budget: None, can_disable,
     default }` and `default` is the base's default when it is in `levels`, else `None`;
     without it, the base's levels, default and budget are kept.
   - `adaptive_thinking` present → `compat.adaptive_thinking`.
   - `compat.thinking_off` (the `thinking.type` string that turns reasoning off) is always the
     base's.
5. `ModelFacts::new(spec).with_catalog(catalog)`.

The `Capabilities` type lives in `derived_facts.rs` (it is the overlay's input), and `cli.rs`
re-uses it.

### 3. `claude-subscription` entries

`entry_of` order: credits → refused (unchanged); listed with an output limit → `Usable`
(unchanged); else `derive(...)` → `Usable` with `Route.derived_from: Some(from)`; else `Refused`
with the existing "update Fontaine" text, now saying no listed model of the same family exists.

### 4. One-time warning

`AccountClient` gains `fn facts_notice(&self, model: &str) -> Option<String>` (default `None`).
`claude-subscription` returns, for a derived route:

> Model `haiku` resolves to `claude-haiku-9-9`, which this build of Fontaine does not list; its
> limits and features are taken from `claude-haiku-5-5` and the Claude CLI's own capability
> flags. Update Fontaine for its exact facts.

`ModelHandle` gains `facts_notice: Option<String>`. `guardian_from_client` (which `from_client`
delegates to) sets it to `client.facts_notice(&reference.model)`; `ModelHandle::new` sets `None`.
`resolve_with` pushes it as a `Notice::warning` for the main model and for a `Ready` guardian
(once when both are the same model: the dedup compares the main and guardian references, and a
fallback guardian is `main.guardian_scoped()`, which carries the same notice). The TUI's `/model` and `/guardian-model`
(`fontaine-tui/src/ui/commands/model.rs`) read the new handle's `facts_notice` after the switch
and show it with `Transcript::warning`; the TUI reads it from `runtime.models.main` /
`runtime.models.guardian` after `switch_main_model` / `set_guardian_model`. In the modes where
`announce` returns early (Manual/AcceptEdits, which never consult the guardian), headless prints
only the main model's `facts_notice` to stderr; the guardian's notice stays on the Auto/Afk/Plan
path, where `announce` prints `runtime.notices()`, which carries both from `Models.notices`. A
derived model used only by a subagent warns through `context.approver.warn(...)` in `ChildContext::model_for`'s caller, once per model reference,
deduplicated like `first_unpriced_cap`. It is shown once per resolution of that model, never per
request.

### 5. Refused rows are shown, not hidden

- `AccountClient` gains `fn refused(&self) -> Vec<RefusedModel>` (default empty), read right after
  `list()` on the same client; `RefusedModel { id: String, reason: String }` (derives
  `Clone, Debug, PartialEq, Eq`) lives in `providers/mod.rs`. `claude-subscription`'s `list()` builds an ordered `Vec<RefusedModel>` of its `Entry::Refused`
  rows in the same loop and stores it alongside `discovered` (a `BTreeMap` keyed by alias) for
  `refused()` to return.
- `models.rs` introduces `pub struct Listing { pub models: Vec<String>, pub refused:
  Vec<RefusedModel> }`. `list_models`/`models_of` return it (an explicit `models` list on a
  non-discovering kind gives `refused: []`; a narrowing `models` list on a discovering kind
  narrows `refused` too; a refused row narrowed away is hidden from listings but still explains
  itself through `not_offered`/`unavailable_reason` when named). `Models.main_listing` and `Switcher::main`'s second value become
  `Listing`; call sites that need ids read `.models`. `Listing` derives `Clone, Debug, Default, PartialEq, Eq`. `Switcher::listings`'
  `main` argument becomes `Option<(&ModelRef, &Listing)>`, so the main account's refused rows
  survive the reuse; `agent/models.rs:216` (`slot.models = Ok(self.models.main_listing.clone())`)
  must copy `refused` too, or the main account's refused rows vanish from `/model`.
- `AccountListing` gains `refused: Vec<RefusedModel>` (`vec![]` in the sources-error literal at `models.rs:971`); the non-main branch of
  `Switcher::listings_within` (`models.rs:996-1001`) calls `client.refused()` right after
  `client.list()` and narrows it by `account.models` the same way it narrows ids, and sets `refused`
  in its `AccountListing` literal (`models.rs:1004`); the production
  `impl From<&AccountListing> for Group` (`model_picker.rs`, used by `/model` and the provider-setup
  picker in `fontaine-tui/src/setup.rs`) copies it, so the setup picker shows unavailable rows too.
- `model_picker.rs`: `Group` carries the refused rows; a new `Row::Refused { label, reason }` is
  drawn dim as `  <id> — unavailable: <reason>` (clipped like every row), matched by the same
  fuzzy filter as model rows, after the group's usable models, and **never selectable**:
  `model_count`, arrow keys and Enter skip it. `visible()` emits a group's heading when usable
  or refused rows match the query (today it skips a group with no model hits). `Row::Refused` is
  rendered in `ModelPicker::body_as` in both branches (qualified uses the full reference);
  `linear.rs` only needs its `Group` test call sites updated.
- `model_report.rs`: `AccountReport` gains `unavailable: Vec<RefusedModel>`; the text report lists
  them under the usable rows as `<id> (unavailable: <reason>)`, the JSON report as an
  `"unavailable": [{"id", "reason"}]` array per account (omitted when empty).

## Implementation Steps

1. `derived_facts.rs`: `Capabilities`, `Derived`, `derive`, with unit tests (Testing Strategy).
   Register in `providers/mod.rs`.
2. `cli.rs`: `ModelRow.capabilities`, filled by `model_rows`; update the row constructors in
   `cli.rs`/`transport.rs` tests.
3. `providers/mod.rs`: `RefusedModel`, the two `AccountClient` defaults.
4. `claude_subscription/mod.rs`: `Route.derived_from`, the new `entry_of` order, `facts_notice`,
   `refused`.
5. `models.rs`: `Listing` (derives `Clone, Debug, Default, PartialEq, Eq`), `ModelHandle.facts_notice` (set in
   `guardian_from_client`, `None` in `ModelHandle::new`), notices in `resolve_with`,
   `AccountListing.refused`, `Switcher::main`/`listings` signatures, `external_check`; update
   callers: `fontaine/src/models_cmd.rs`, `fontaine-core/src/agent/models.rs` (incl. the :216
   `refused` copy), `agent/setup.rs`, `child_runner.rs` (`MainModel.listing` becomes `Listing`; after `model_for`, emit the handle's `facts_notice` via
   `context.approver.warn`, once per model reference like `first_unpriced_cap`),
   `agent/model_source.rs`, `composition_offer.rs` (test `AccountListing` literal), `model_report.rs:183-184` (test
   `AccountListing` literal), `fontaine-core/tests/model_source.rs:16-17` (`MainModel { listing }`
   literal becomes `Listing`), and the test files building `Models { main_listing: Vec::new(), .. }` (use `Listing::default()`).
6. `model_report.rs`: `unavailable` in text and JSON.
7. TUI: `model_picker.rs` `Row::Refused`, `impl From<&AccountListing> for Group` copying `refused`, heading rule, `body_as`, `linear.rs` test call sites,
   the switch notice in `fontaine-tui/src/ui/commands/model.rs`.
8. Headless: `fontaine/src/headless.rs` prints the main model's `facts_notice` warning to stderr
   in the modes where `announce` returns early (not Auto/Afk/Plan); the guardian's notice stays on
   the `runtime.notices()` path.
9. Docs and master-plan Decisions (below); file the `anthropic` follow-up issue and link it in the
   Decisions line.

## Files to Modify

- `rust/fontaine-core/src/providers/derived_facts.rs` (new)
- `rust/fontaine-core/src/providers/mod.rs`
- `rust/fontaine-core/src/providers/claude_subscription/{mod.rs,cli.rs,transport.rs}`
- `rust/fontaine-core/src/models.rs`, `rust/fontaine-core/src/model_report.rs`,
  `rust/fontaine-core/src/agent/{models.rs,setup.rs,model_source.rs}`,
  `rust/fontaine-core/src/{child_runner.rs,composition_offer.rs}`
- `rust/fontaine/src/{models_cmd.rs,headless.rs}`
- `rust/fontaine-tui/src/{model_picker.rs,linear.rs,ui/commands/model.rs}`
- `rust/fontaine-core/tests/{claude_subscription.rs,models.rs,goal.rs,model_source.rs}`,
  `rust/fontaine-tui/tests/common/mod.rs`, and the other tests building
  `Models { main_listing: Vec::new(), .. }`
- `docs/claude-subscription.md`, `docs/configuration.md` (Model discovery; compaction at
  :355-357 and :907-909), `docs/tui.md` (model picker; :666-667, :674-676), `docs/tools.md`
  (`models` tool output), `docs/headless.md` (:110-111)
- `tasks/fontaine_harness_plan.md` (Decisions)

## Trade-offs

- **Family sibling + CLI flags vs CLI flags alone.** The CLI does not report an output limit,
  context window, modalities or compat quirks, so flags alone cannot build a spec rig accepts. The
  sibling supplies those; the flags correct the part most likely to change between point releases
  (effort levels, thinking-off).
- **Latest release in the family vs nearest version number.** Parsing version numbers out of ids
  is fragile (`claude-opus-5-fast`, dated snapshots); the newest listed sibling is the best guess
  for a newer release, which is the case this exists for.
- **Extending `list()`'s return type vs a separate `refused()`.** Changing `list()` touches every
  provider for one kind's need; a defaulted method keeps the other kinds untouched (open/closed).
- **Refused rows in the picker vs only in `fontaine models`.** The picker is where the user looked
  for Haiku; a dim unselectable row with its reason answers "where is my model" in place.

## Decisions

- 2026-11-08: Scope is `claude-subscription` only; `anthropic`/`openai-compatible` unlisted ids are
  left to a follow-up issue, because their listings carry no capability flags and the Sessions
  2026-10-24 "no fallback spec" decision governs them. `anthropic` already gets rig's 64000
  fallback for unlisted `claude-opus-4*`/`claude-sonnet-4*`/`claude-haiku-4-5*` ids; the
  follow-up applies family-sibling derivation only.
- 2026-11-08: Listed models keep their catalog facts; a CLI flag that disagrees is a reason to
  bump rig, not to override at runtime.

## Documentation

- `docs/claude-subscription.md`: replace "refused rather than guessed at" with the derivation rule
  (same family only, newest listed sibling, CLI capability flags, no price, one-time warning), and
  that a row with no listed family or a credit marker is still refused and shown with its reason.
- `docs/configuration.md` (Model discovery) and `docs/tools.md`: the `unavailable` rows in
  `fontaine models` and the `models` tool.
- `docs/tui.md`: unavailable rows in `/model`, `/guardian-model` and the provider-setup picker; the
  picker heading rule (:767-769): a group's heading is kept also when only unavailable rows match.
- `docs/configuration.md` (:536): effort levels come from the catalog; a derived model's come from
  the CLI flags.
- `docs/configuration.md` (:355-357, :907-909), `docs/tui.md` (:666-667, :674-676) and
  `docs/headless.md` (:110-111): update the statements that an uncatalogued model is never
  compacted / has no `/context` window to say a derived `claude-subscription` model counts as
  listed for compaction and `/context`.
- Master plan `## Decisions`, Providers subsection (dated lines):
  - the deviation from "unlisted models fail closed" for `claude-subscription`, with the rule and
    its "unknown family stays refused" boundary;
  - derived specs enable threshold compaction and `/context` windows for those models, an
    exception to Sessions 2026-10-24 (recorded at the end of the Sessions subsection);
  - refused rows are shown, never selectable;
  - other kinds deferred to the follow-up issue.
- `CHANGELOG.md` entry (written by the PR step).

## Testing Strategy

All unit/integration, no live CLI (`claude_cli_double` fake only).

- `derived_facts.rs` unit tests, against `Catalog::builtin()` and a small hand-built `Catalog`:
  same-family newest base chosen (`claude-haiku-9-9` → newest `claude-haiku-*`); longest family
  prefix wins; deprecated, output-limit-less and undated entries skipped/ranked last; unknown
  family → `None`; family with no usable entry → `None`; id/display name replaced, pricing and
  cost cleared; overlay maps known effort strings, drops unknown ones, applies `thinking_off` and
  `adaptive_thinking`, keeps the base default only when listed; absent overlay keeps the base's
  reasoning; an `effort_levels` list with no known string counts as absent.
- `cli.rs` `model_rows`: flags parsed; missing and wrong-typed flags read as absent.
- `claude_subscription/mod.rs` unit tests for `entry_of`: listed → not derived; unlisted
  same-family → derived with `from`; unknown family → refused; credit row with a known family →
  refused before derivation; `facts_notice` and `refused` contents.
- `tests/claude_subscription.rs` (fake CLI): a listing with `haiku → claude-haiku-9-9` resolves
  as the main model with one `facts_notice` warning in `Models.notices` (and the same for a
  derived `Ready` guardian model), and its request runs with
  the canonical id and the base's output limit; `future → claude-quantum-9` appears in
  `AccountListing.refused` with its reason, including when the account is the main account
  (the reuse path); `models` narrowing applies to refused rows; `haiku` is usable and listed in
  `AccountListing.models` (regression pin for Haiku hidden from the picker). A subagent whose
  model is derived emits the `facts_notice` once through the approver.
- `model_picker.rs` unit tests: refused row drawn with its reason, filtered by the query, skipped
  by arrows and Enter, absent when the query excludes it; a group whose query matches only
  refused rows keeps its heading.
- Headless integration test (fake CLI): a derived main model prints the `facts_notice` warning to
  stderr in default mode, and in one Auto-mode run stderr carries exactly one `facts_notice` line.
- `model_report.rs` unit tests: text and JSON `unavailable` output, omitted when empty.
