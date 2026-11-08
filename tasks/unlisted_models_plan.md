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
  spec, and Fontaine's compaction and `/context` read `completion.spec()` (`agent.rs:1097`,
  `session.rs:464`).
- `fontaine-core/src/models.rs:849` — `AccountListing { account, kind, models }`.
  `Switcher::listings` (`models.rs:955`) reuses the main account's already-fetched ids
  (`main: Option<(&ModelRef, &[String])>`) rather than re-listing it. `Models.main_listing` and
  `Switcher::main` carry those ids as `Vec<String>`.
- `fontaine-tui/src/model_picker.rs` — `Group` holds `Result<Vec<String>, String>`; rows are
  `Heading`, `Model`, `Failed`. `fontaine-core/src/model_report.rs` builds the `fontaine models`
  and `models`-tool report from the same `AccountListing`s.
- `Models.notices` (`models.rs:351`) is the startup channel for one-time warnings, printed by the
  TUI, headless and `fontaine models`.
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
   `details.cost` reset to default, `details.release_date`/`last_updated` cleared, and
   `deprecated = false`.
4. Overlay (only what the row reports):
   - `effort_levels` present → `reasoning = ReasoningSupport::Listed { levels, budget: None,
     can_disable, default }`, where `levels` maps `minimal|low|medium|high|xhigh|max` to rig's
     `Effort` and **drops any string it does not know** (never claim a level it cannot name),
     `can_disable = thinking_off.unwrap_or(base's can_disable)`, and `default` is the base's
     default when it is in `levels`, else `None`.
   - `adaptive_thinking` present → `compat.adaptive_thinking`.
   - `thinking_off == Some(false)` → `compat.thinking_off = None`.
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
> flags, and it has no price, so cost budgets do not apply to it. Update Fontaine for its exact
> facts.

`ModelHandle` gains `facts_notice: Option<String>`, set in `ModelHandle::from_client`.
`resolve_with` pushes it as a `Notice::warning` for the main model and for a `Ready` guardian
(once when both are the same model). The TUI's `/model` and `/guardian-model` switch show the new
handle's notice as a warning line in the transcript. It is shown once per resolution of that
model, never per request.

### 5. Refused rows are shown, not hidden

- `AccountClient` gains `fn refused(&self) -> Vec<RefusedModel>` (default empty), read right after
  `list()` on the same client; `RefusedModel { id: String, reason: String }` lives in
  `providers/mod.rs`. `claude-subscription` returns its `Entry::Refused` rows in listing order.
- `models.rs` introduces `pub struct Listing { pub models: Vec<String>, pub refused:
  Vec<RefusedModel> }`. `list_models`/`models_of` return it (an explicit `models` list on a
  non-discovering kind gives `refused: []`; a narrowing `models` list on a discovering kind
  narrows `refused` too). `Models.main_listing` and `Switcher::main`'s second value become
  `Listing`; call sites that need ids read `.models`. `Switcher::listings`' `main` argument becomes
  `Option<(&ModelRef, &Listing)>`, so the main account's refused rows survive the reuse.
- `AccountListing` gains `refused: Vec<RefusedModel>` (empty on a failed listing).
- `model_picker.rs`: `Group` carries the refused rows; a new `Row::Refused { label, reason }` is
  drawn dim as `  <id> — unavailable: <reason>` (clipped like every row), matched by the same
  fuzzy filter as model rows, after the group's usable models, and **never selectable**:
  `model_count`, arrow keys and Enter skip it. `linear.rs` renders it with the qualified
  reference, as it does model rows.
- `model_report.rs`: `AccountReport` gains `unavailable: Vec<RefusedModel>`; the text report lists
  them under the usable rows as `<id> (unavailable: <reason>)`, the JSON report as an
  `"unavailable": [{"id", "reason"}]` array per account (omitted when empty).

### 6. Other account kinds

Not changed here (see Decisions). `anthropic` already gets rig's 64000 fallback for unlisted
`claude-opus-4*`/`claude-sonnet-4*`/`claude-haiku-4-5*` ids and an explicit "`max_tokens` must be
set" error otherwise; `openai-compatible` gateways send no default output limit. A follow-up issue
covers applying `derive` to `anthropic` (family sibling only, as its listing has no capability
flags).

## Implementation Steps

1. `derived_facts.rs`: `Capabilities`, `Derived`, `derive`, with unit tests (Testing Strategy).
   Register in `providers/mod.rs`.
2. `cli.rs`: `ModelRow.capabilities`, filled by `model_rows`; update the row constructors in
   `cli.rs`/`transport.rs` tests.
3. `claude_subscription/mod.rs`: `Route.derived_from`, the new `entry_of` order, `facts_notice`,
   `refused`.
4. `providers/mod.rs`: `RefusedModel`, the two `AccountClient` defaults.
5. `models.rs`: `Listing`, `ModelHandle.facts_notice`, notices in `resolve_with`, `AccountListing.
   refused`, `Switcher::main`/`listings` signatures; update callers (`fontaine/src/models_cmd.rs`,
   `fontaine/src/auto_mode.rs`, `fontaine/src/lib.rs`, `fontaine-tui/src/setup.rs`,
   `fontaine-core/src/agent.rs`, the TUI `/model` command, tests).
6. `model_report.rs`: `unavailable` in text and JSON.
7. TUI: `model_picker.rs` `Row::Refused`, `linear.rs`, the switch notice in
   `fontaine-tui/src/ui/commands/model.rs`.
8. Docs and master-plan Decisions (below); file the `anthropic` follow-up issue and link it in the
   Decisions line.

## Files to Modify

- `rust/fontaine-core/src/providers/derived_facts.rs` (new)
- `rust/fontaine-core/src/providers/mod.rs`
- `rust/fontaine-core/src/providers/claude_subscription/{mod.rs,cli.rs,transport.rs}`
- `rust/fontaine-core/src/models.rs`, `rust/fontaine-core/src/model_report.rs`,
  `rust/fontaine-core/src/agent.rs`
- `rust/fontaine/src/{models_cmd.rs,auto_mode.rs,lib.rs}`
- `rust/fontaine-tui/src/{model_picker.rs,linear.rs,setup.rs,ui/commands/model.rs}`
- `rust/fontaine-core/tests/{claude_subscription.rs,models.rs}`
- `docs/claude-subscription.md`, `docs/configuration.md` (Model discovery), `docs/tui.md`
  (model picker), `docs/tools.md` (`models` tool output)
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
  2026-10-24 "no fallback spec" decision governs them.

## Documentation

- `docs/claude-subscription.md`: replace "refused rather than guessed at" with the derivation rule
  (same family only, newest listed sibling, CLI capability flags, no price, one-time warning), and
  that a row with no listed family or a credit marker is still refused and shown with its reason.
- `docs/configuration.md` (Model discovery) and `docs/tools.md`: the `unavailable` rows in
  `fontaine models` and the `models` tool.
- `docs/tui.md`: unavailable rows in `/model` and `/guardian-model`.
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
  reasoning.
- `cli.rs` `model_rows`: flags parsed; missing and wrong-typed flags read as absent.
- `claude_subscription/mod.rs` unit tests for `entry_of`: listed → not derived; unlisted
  same-family → derived with `from`; unknown family → refused; credit row with a known family →
  refused before derivation; `facts_notice` and `refused` contents.
- `tests/claude_subscription.rs` (fake CLI): a listing with `haiku → claude-haiku-9-9` resolves
  as the main model with one `facts_notice` warning in `Models.notices`, and its request runs with
  the canonical id and the base's output limit; `future → claude-quantum-9` appears in
  `AccountListing.refused` with its reason, including when the account is the main account
  (the reuse path); `models` narrowing applies to refused rows.
- `model_picker.rs` unit tests: refused row drawn with its reason, filtered by the query, skipped
  by arrows and Enter, absent when the query excludes it.
- `model_report.rs` unit tests: text and JSON `unavailable` output, omitted when empty.
- Regression pin for the bug seen in the wild (Haiku hidden from the picker): the integration test
  above with a family member absent from the pinned catalog shows it usable in the listing.

## Open Questions

- Should the overlay also correct catalog-listed models when the CLI's flags disagree with the
  catalog? This plan keeps listed models untouched.
