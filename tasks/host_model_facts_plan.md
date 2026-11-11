# Host Model Facts Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/167

## Overview

A host that serves Claude under a name of its own (an Azure Foundry deployment, a custom Databricks
serving endpoint, a LiteLLM-style proxy behind an `anthropic` account) leaves Fontaine and rig with
no model facts for that name. Without facts, rig does not suppress forced tool choice for models
that reject it, the turn has no known output limit, and compaction never runs because the context
window is unknown. This plan adds a per-account, user-scope table, `model_facts`, that maps a host
model name to the catalog model it serves. The account binds that model's facts to the served
name. A name with no facts at all gets a one-time warning that points at the new key.

## Current State

- **Facts binding per kind.** `anthropic` binds only derived facts for an unlisted id whose shape
  matches a listed one: `rust/fontaine-core/src/providers/anthropic.rs:76` calls
  `derived_facts::unlisted_facts(vendor("anthropic"), model)`. A name like `prod-opus` has no
  version token, so `shape()` returns `None` and the model gets no facts
  (`rust/fontaine-core/src/providers/derived_facts.rs:45`).
- **Databricks** (#577) strips `system.ai.` / `databricks-` and binds facts with
  `derived_facts::facts_as(vendor, databricks, base, served)` (`providers/databricks.rs:244`
  `facts_for`). So `databricks-claude-opus-5-5` is already covered. A custom
  `catalog.schema.name` endpoint that serves Claude (`main.team.claude`) is classified as
  `Family::Other` and gets no facts.
- **`facts_as`** (`derived_facts.rs:145`) already does the relabelling this feature needs. It
  takes a catalog id on a vendor, looks up the exact entry (or a dotted version, or a derived
  one), and returns the spec relabelled to the served id under the served dialect, with no price.
- **Notices.** `AccountClient::facts_notice` (`providers/mod.rs:275`) warns once, and only for
  derived facts. `ModelHandle::guardian_from_client` stores the notice (`models.rs:254`). A model
  with no facts at all produces no warning.
- **Consumers of the bound spec.** `ModelHandle::turn_max_tokens` (`models.rs`) reads
  `completion.spec()`. Session compaction skips when `spec()` is `None` or has no
  `context_window` (`session.rs:625-636`). Effort validation uses `spec.validate`. rig's wire reads
  `rejects_forced_tool_choice` and its default `max_tokens` from the bound facts.
- **Config.** `Account` (`config.rs:374`) is `deny_unknown_fields`. `check_account`
  (`config.rs:1594`) refuses kind-specific keys through `Provider::accepts_*` hooks
  (`providers/mod.rs:115-135`). `[accounts.*]` is user scope only (`docs/configuration.md` scope
  table; `tasks/design/config.md:182`), so any new account key inherits that.
- **Claude Code** offers `modelOverrides`, which maps an Anthropic model id to the provider's id
  (an ARN or a deployment name), so the picker keeps canonical names.

## Design

### Config key

```toml
[accounts.foundry]
kind = "anthropic"
base_url = "https://my-resource.services.ai.azure.com/anthropic"
auth = "bearer"
models = ["prod-opus", "team-haiku"]

[accounts.foundry.model_facts]
prod-opus = "claude-opus-5-5"
team-haiku = "claude-haiku-5-5"
```

- `Account.model_facts: Option<BTreeMap<String, String>>`. The key is the model id as the account
  serves it (the `<model>` in `<account>/<model>`). The value is a catalog id.
- New hook `Provider::accepts_model_facts()`, default `false`, `true` for `anthropic` and
  `databricks`. `check_account` refuses the key on any other kind ("not used by kind = …"), the
  same way it handles `wires`. Foundry (#165) turns the hook on when it lands.
- Validation in `check_account`: every value must resolve, through
  `derived_facts::facts_as(vendor, served, value, key)`, to listed or derived facts on the
  account's mapping vendor (below). An unknown value is a config error naming
  `accounts.<name>.model_facts.<key>`. A mapping that silently did nothing would leave the user
  believing the model is covered.
- User scope only, since it sits under `[accounts.*]`. It changes what the guardian believes about
  its model (forced tool choice, limits), so it must never come from a project file.

### Mapping vendor

- **`anthropic`**: the value is looked up under vendor `anthropic` and served as `anthropic`.
- **`databricks`**: the value's family (`family(value)`, the existing classifier) chooses the
  vendor through the existing `catalog_vendor_of`. `Family::Other` has no vendor, so the value is
  refused. Served as `databricks`. Routing is unchanged: the wire is still chosen from the served
  name, and a custom endpoint that should use Messages sets `wires` as today. Inferring the wire
  from the mapping would make one key do two things.

### Shared helper

In `derived_facts.rs`, add:

```rust
/// The facts `model_facts` maps `served_id` to, if any; `Err` names a value the catalog cannot
/// give facts for.
pub fn mapped(map: &BTreeMap<String,String>, vendor_of: impl Fn(&str) -> Option<ProviderId>,
              served: ProviderId, served_id: &str) -> Option<Result<(ModelFacts, String, bool), String>>
```

It is a thin wrapper over `facts_as`, so config validation and both kinds share one lookup.

### Binding

- **`anthropic`** `Connected` keeps the account's map. `model()` binds mapped facts first, then
  falls back to `unlisted_facts`. A mapped name that is also listed takes the mapping: the user's
  explicit statement wins.
- **`databricks`** `DatabricksClient` keeps the map. `facts_for(model)` checks the map before the
  prefix-strip path. The rest is unchanged.
- **`facts_notice`** for a mapped model: if the target was derived (not listed), reuse
  `notice(model, target, from, None)`. A listed target gets no notice.

### Warning for a name with no facts

Add a default `AccountClient::no_facts_notice`. Better, extend the `facts_notice` override on
`anthropic` and `databricks`: when the model has no facts from any source (not mapped, not listed,
not derived), return a one-time warning:

> Model `prod-opus` is not one this build of Fontaine knows, so its context window and output
> limit are unknown and it is never compacted automatically. If it serves a Claude model, map it
> under `[accounts.foundry.model_facts]`.

The client needs the account name to write that, so `ConnectContext` passes it if it does not
already (check during implementation). The existing one-time machinery
(`ModelHandle.facts_notice`, `tree.first_facts_notice`) carries the warning, so no new plumbing is
needed. Only `anthropic` and `databricks` get the warning: their ids are expected to be Claude,
GPT or Gemini, while on other kinds an unknown id is routine.

### No assumed 200K window

Claude Code assumes 200K for an unrecognised deployment name. Fontaine does not fabricate a spec
for an unmapped name; it warns instead. This keeps the 2026-10-24 "no fallback spec" rule, and a
wrong guess sizes compaction wrongly without telling anyone. Record it in Decisions.

## Implementation Steps

1. **Config**: add `model_facts` to `Account` (field, `Account::new`, doc comment), the
   `accepts_model_facts` hook on `Provider` (true for `Anthropic`, `Databricks`), and the
   refusal and value validation in `check_account`. Regenerate the JSON schema under
   `docs/schemas` if it is generated from `JsonSchema` (check the existing schema test).
2. **Helper**: add `derived_facts::mapped`. Expose `databricks::{family, catalog_vendor_of}` to it
   as `pub(super)` if needed, or have each kind pass its own vendor closure.
3. **anthropic**: store the map in `Connected` and bind mapped facts before `unlisted_facts`. Add
   a `facts_notice` override (mapped-derived notice, then no-facts notice, else the default).
4. **databricks**: store the map in `DatabricksClient`, look up the map first in `facts_for` and
   `facts_notice_for`, and add the no-facts notice.
5. **Docs and changelog** (below).
6. **Decisions**: add the lines in `## Decisions` below to the master plan's `### Providers`
   subsection, at its end.

## Files to Modify

- `rust/fontaine-core/src/config.rs`: `Account.model_facts`, `check_account`.
- `rust/fontaine-core/src/providers/mod.rs`: `Provider::accepts_model_facts`; account name in
  `ConnectContext` if missing.
- `rust/fontaine-core/src/providers/derived_facts.rs`: `mapped`.
- `rust/fontaine-core/src/providers/anthropic.rs`: binding and notices.
- `rust/fontaine-core/src/providers/databricks.rs`: binding and notices.
- Any exhaustive `Account { .. }` literal outside `Account::new` (compiler-guided).
- `docs/configuration.md`, `tasks/design/config.md`, `CHANGELOG.md`,
  `tasks/fontaine_harness_plan.md`, generated schema if any.

## Trade-offs

- **Direction: served name → catalog id** (chosen) versus Claude Code's catalog id → host id.
  Fontaine model references already name the host's id (`<account>/<model>`, taken from the
  listing or `models`), and ledger, telemetry and `served_model` all record that id. Reversing the
  direction would rename models on the wire and in every record, which is a much larger change for
  the same facts. Recorded as a deviation.
- **Per-kind opt-in** versus every kind with a catalog vendor. `openai`, `gemini`, `openrouter`
  and `ollama` already serve catalog ids; `openai-compatible` has no catalog vendor and a chat wire
  on which Claude facts have not been measured. Opting in per kind keeps the key where the issue
  needs it. Foundry (#165) opts in with one hook.
- **Refuse an unknown target at load** versus warning at run time. A mapping that does nothing
  defeats the key's purpose, and config errors already exit 2 at load.

## Decisions

- 2026-11-11: `model_facts` maps served name → catalog id, the opposite direction to Claude Code's
  `modelOverrides`, because Fontaine model references and records already name the host's id; the
  mapping binds facts only and never renames the model on the wire.
- 2026-11-11: An unmapped, unknown model on `anthropic` or `databricks` is warned about once
  and given no assumed 200K window (Claude Code assumes 200K), keeping the "no fallback spec" rule.
- 2026-11-11: On `databricks` the mapping does not change wire routing; a custom endpoint that
  serves Claude on Messages also sets `wires`.

## Documentation

- `docs/configuration.md`: a `model_facts` row in the `[accounts.<name>]` key table, with the
  Foundry-style example, plus the per-key scope table if it lists account keys individually. In
  the Databricks section, a note on custom endpoints (`model_facts` plus `wires`).
- `tasks/design/config.md:182`: add `model_facts` to the `[accounts.*]` key list.
- `CHANGELOG.md`: an Unreleased entry.

## Testing Strategy

Unit tests, all offline:

- `derived_facts`: `mapped` returns the listed facts relabelled to the served name (id, provider,
  no pricing, `context_window` and `max_output_tokens` equal to the catalog entry's). A derived
  target returns `listed == false`. An unknown target returns `Err`.
- `config` (`check_account`): `model_facts` refused on `openai-compatible`. An unknown value is
  refused with the `accounts.<name>.model_facts.<key>` path. A Databricks value of family `Other`
  is refused. A valid table is accepted on `anthropic` and `databricks`.
- `anthropic`: on a connected client with `model_facts = { prod-opus = "claude-opus-5-5" }`,
  `completion("prod-opus").spec()` carries the Opus context window and output limit. Without the
  map it carries none. `facts_notice("prod-opus")` is `None` when mapped and the no-facts warning
  when unmapped. A shape-derivable unlisted id keeps its derived notice.
- `databricks`: `main.team.claude` mapped to `claude-sonnet-5-5` gets the Sonnet facts under the
  `databricks` provider. `databricks-claude-opus-5-5` behaves exactly as before (regression).
  `main.team.router` unmapped gets the no-facts warning.
- Integration (`fontaine-core/tests/max_tokens.rs` or a new `model_facts.rs`, wiremock): an
  `anthropic` account whose `prod-opus` is mapped sends `max_tokens` equal to the Opus output
  limit on a main turn, where before it sent none. This exercises the whole path, config → client →
  handle → request, and a regression there would otherwise be silent.
