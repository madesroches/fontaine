# Price Databricks Models at the Served Model's List Price Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/677

## Overview

A `databricks` account is `Billing::Unpriced`, so a dbx session counts nothing in dollars even when
the gateway serves a Claude, GPT or Gemini model whose price rig's catalog already knows. This plan
prices such a model at the *served* vendor's list price and labels the figure an estimate, leaving
genuinely unpriceable models (GLM, Kimi, DeepSeek, `gpt-oss-*`, custom `catalog.schema.name`
endpoints) exactly as they are today. Spend, `total_cost_usd`, the OTLP cost metric and the dollar
cost caps then all describe what the session's tokens would have cost on the served vendor's own
API — never the workspace's DBU bill, which Fontaine cannot read per call.

## Current State

- `providers/databricks.rs:83` — `fn billing() -> Billing::Unpriced`; the `DBX_DIALECT` comment at
  `:60` says "Never priced: the workspace bills in DBUs".
- `providers/derived_facts.rs` `facts_as()` builds the facts a gateway wire binds: it relabels the
  vendor's catalog entry to the served id and **clears** `spec.pricing` and `spec.details.cost`.
  `derive()` (the shape-derived path) clears pricing too. So even a listed
  `system.ai.claude-sonnet-5-5` reaches the wire with no price.
- `core/models.rs:304` `catalog_billed()` is `kind.provider().billing() == Billing::Catalog`, and
  `:379` `priced()` is `catalog_billed() && spec.pricing.is_some()`. Both are false for dbx.
- Everything downstream keys off those two:
  - the cost is dropped where usage enters — `core/turn.rs:855` (`TurnParams.priced`),
    `core/side_question.rs:277` (`billed`), and the same flag reaches `child_runner.rs`,
    `goal.rs`, `session.rs` (the summarizer), `scanner_stage.rs`, `advisor.rs`, `user_hooks.rs`;
  - the token is booked as `unpriced_tokens` in `core/usage_ledger.rs` `Bucket::add`;
  - `--max-budget-usd` is a usage error with a dbx main model (`assemble.rs:699`), the flag makes a
    preference list skip dbx entries (`core/models.rs:1122`), and the dollar caps read inactive
    (`core/agent/models.rs:433`, `core/child_runner.rs:743`);
  - telemetry: the `fontaine.interaction` span records `model.priced = false`
    (`core/turn.rs:268`), which makes `span_bridge.rs:169` skip the cost.
- `span_bridge.rs:270` `cost_usd(provider, model, usage)` re-derives the price from
  `Catalog::builtin().resolve("{provider}/{model}")`. That only works for a catalog vendor, so a
  `databricks/system.ai.*` reference resolves to nothing — flipping `model.priced` is not enough
  on its own.
- Displays print dollars with no qualifier: `agents_panel.rs:133` `spend_text`, `reports.rs:783`
  `cost_cell`, `statusline.rs:123` `CostInfo`, and `events.rs:252` `total_cost_usd`.
- `tests/databricks.rs:144` asserts `!handle.priced()` and `!handle.catalog_billed()`.

## Design

### 1. `Billing::Estimated`

`providers/mod.rs`, next to `Catalog` and `Unpriced`:

```rust
/// The account bills in its own units (DBUs), so the catalog price of a model whose vendor
/// lists one is an estimate of what the same tokens would cost on that vendor's API.
Estimated,
```

`databricks::billing()` returns it. The `DBX_DIALECT` comment becomes the reason: the workspace
bills in DBUs, so a listed model's price is an estimate, never the bill.

`Billing::Unpriced` keeps its meaning verbatim for `kimi`, `opencode-go`, `openai-compatible` and
`claude-subscription`, so no other kind changes behavior.

### 2. `CostBasis` on the handle

`core/models.rs` gains the three-way answer every caller actually wants, and the two existing
predicates become thin readers of it:

```rust
pub enum CostBasis { Billed, Estimated, Unpriced }

impl ModelHandle {
    /// How the cost a call reports is treated. An `Estimated` kind prices only a model the
    /// catalog lists: one it does not stays `Unpriced`, since no cost reaches us for it.
    pub fn cost_basis(&self) -> CostBasis {
        match self.kind.provider().billing() {
            Billing::Catalog => CostBasis::Billed,
            Billing::Unpriced => CostBasis::Unpriced,
            Billing::Estimated if self.listed_price().is_some() => CostBasis::Estimated,
            Billing::Estimated => CostBasis::Unpriced,
        }
    }
    fn listed_price(&self) -> Option<&Pricing> { self.completion.spec()?.pricing.as_ref() }
    /// Whether the cost a call reports is counted at all; `cost_basis` says how.
    pub fn catalog_billed(&self) -> bool { self.cost_basis() != CostBasis::Unpriced }
    /// Whether rig's catalog knows a price for this model; a spend cap needs one.
    pub fn priced(&self) -> bool { self.listed_price().is_some() && self.catalog_billed() }
}
```

`catalog_billed()` keeps its name and signature, so the `priced`/`billed` booleans already threaded
into turn, side question, goal, subagent, scanner and hook paths keep compiling; each of those call
sites is where a basis is threaded instead (step 4).

### 3. Keep the vendor's price in dbx facts

`derived_facts::facts_as(vendor, served, base_id, served_id)` gains one parameter saying what the
gateway wants done with the price:

```rust
/// What a gateway does with the vendor entry's price. `Keep` is for a workspace that bills in
/// its own units: the vendor's list price is the only per-token figure there is.
pub enum Price { Keep, Strip }
```

Inside, `spec.pricing` is cleared unless `keep == Price::Keep` **and** the facts came from an exact
catalog entry (`listed`). A shape-derived entry never contributes a price: it is already a guess
about which model this is, and a guessed price on top of that is not worth having. `details.cost`
stays cleared either way.

`databricks::facts_for` passes `Price::Keep`; `anthropic`, `kimi` and `opencode_go` pass
`Price::Strip` and are unchanged. `fact_notice_for`'s `check_model_facts` calls pass `Strip` (it
only asks whether a target resolves).

The `OUTPUT_CAPS` fallback spec (`glm-*`) is built bare, so it keeps no price — a `system.ai.glm-5-3`
model reads `unpriced tok` exactly as today.

### 4. Thread the basis where the boolean goes

`CostBasis` replaces the `priced: bool` / `billed: bool` that each entry point carries:

- `core/turn.rs`: `TurnParams.priced` and `Drive.priced` become `cost: CostBasis`; the drop site
  becomes `if params.cost == CostBasis::Unpriced { usage.cost = None; }`, and
  `cost == CostBasis::Estimated` is what marks the ledger and the agent tree.
- `core/side_question.rs`, `core/goal.rs`, `core/child_runner.rs`, `core/session.rs`,
  `core/scanner_stage.rs`, `core/agent/{goal,advisor,setup}.rs`, `core/user_hooks.rs`,
  `core/project_env.rs`, `run.rs`, `fontaine-tui/src/ui/turn.rs`, `fontaine/src/auto_mode.rs`,
  `fontaine-evals/src/live.rs`: pass `handle.cost_basis()` where they pass
  `handle.catalog_billed()` today. The span field stays `model.priced` (bool) and gains
  `model.estimated` (bool) so a telemetry reader can tell the two apart.

### 5. Carry "this is an estimate" to the figures

Two carriers, mirroring the existing `unpriced_tokens` convention:

- `core/usage_ledger.rs` `Bucket` gains `estimated: bool` — set when a priced call's basis was
  `Estimated`, merged with `||`. Its doc says an estimate is never presented as a bill, so a bucket
  holding both is marked.
- `agents/budget.rs` `Spend` gains the same field, and `core/gate.rs` `spend_of(usage, basis)` takes
  the basis; `record_usage_answered` and its three callers carry it through.

An estimate is rendered, never inferred: `spend_text` (`agents_panel.rs`), `cost_cell`
(`reports.rs`) and the statusline append ` est.` to the dollar figure when the bucket or spend is
estimated, so `$0.0066 est.` / `$0.0021 est. / $2.00`, next to `· 128k unpriced tok` where that
still applies. `TOOL` and truncation rules for these rows are unchanged; `est.` rides with the
dollar segment.

`events.rs` `ResultEvent.total_cost_usd` and `statusline.rs` `CostInfo.total_cost_usd` keep their
JSON shape (the headless `result` object is the Claude-SDK-compatible contract) and are documented
as a list-price estimate for an estimate-priced run.

### 6. Price the telemetry stream

`span_bridge::cost_usd` today only resolves catalog vendors. Add one seam in
`core/providers/mod.rs`:

```rust
/// The catalog price of a model served through the kind `provider`, when the kind's account
/// bills in its own units and Fontaine prices its tokens at the vendor's list price instead.
pub fn listed_price(provider: &str, model: &str) -> Option<Pricing>
```

which returns the catalog's own entry for a catalog vendor, and otherwise asks the kind:
`databricks::listed_price(model)` — `family`/`base_id`/`catalog_vendor_of` over the exact catalog
entry, the same resolution `facts_for` uses, so the span's price and the wire's price cannot drift.

`span_bridge::cost_usd` becomes `listed_price(provider, model)?.cost(usage).map(|c| c.total)`, and
the emitted `fontaine.cost.usage` for a call under a `model.estimated` span carries
`estimated = "true"` — the attribute the guardian-drop estimate already uses for the same reason.
`fontaine.token.usage` is untouched: those tokens were measured.

## Implementation Steps

**Phase 1 — facts keep the price**

1. `providers/derived_facts.rs`: add `Price`, thread it through `facts_as`, clear pricing only when
   `Strip` or the facts were derived. Update the existing `facts_as_*` unit tests for the new
   argument and add the `Keep`/derived/strip cases.
2. `providers/databricks.rs`: pass `Price::Keep` from `facts_for`, `Price::Strip` from
   `check_model_facts`; add `pub fn listed_price(model) -> Option<Pricing>` next to `facts_for`.
3. `providers/mod.rs`: add `Billing::Estimated` and the `listed_price(provider, model)` seam;
   `databricks::billing()` returns `Estimated`.

**Phase 2 — the handle says how its cost is treated**

4. `core/models.rs`: add `CostBasis`, `cost_basis()`, `listed_price()`; rewrite `catalog_billed()`
   and `priced()` on top of them. Unit-test all four arms, including a dbx model the catalog does
   not list (`glm-*` → `Unpriced`).
5. Replace the `priced`/`billed` booleans with `CostBasis` at every site listed in Design §4, and
   add the `model.estimated` span field.

**Phase 3 — count and label the estimate**

6. `core/usage_ledger.rs`: `Bucket.estimated`, set in `add`, folded in `merge`; unit tests.
7. `agents/budget.rs` `Spend.estimated`; `core/gate.rs` `spend_of(usage, basis)` and the
   `record_usage_answered`/`add_*_usage` chain.
8. Displays: `agents_panel.rs` `SpendView.estimated` + `spend_text`, `reports.rs` `cost_cell`,
   `statusline.rs` if the cost is rendered there, and the TUI row plumbing that fills them.

**Phase 4 — telemetry**

9. `fontaine/src/telemetry/span_bridge.rs`: `cost_usd` through `listed_price`, and the
   `estimated = "true"` attribute on the cost metric when the enclosing span says so.

**Phase 5 — docs and changelog**

10. The documentation edits listed below, and a `CHANGELOG.md` entry.

## Files to Modify

- `rust/fontaine-sdk/src/core/providers/mod.rs` — `Billing::Estimated`, `listed_price` seam
- `rust/fontaine-sdk/src/core/providers/derived_facts.rs` — `Price`, `facts_as`
- `rust/fontaine-sdk/src/core/providers/databricks.rs` — `billing()`, `listed_price`, `facts_for`
- `rust/fontaine-sdk/src/core/providers/{anthropic,kimi,opencode_go}.rs` — pass `Price::Strip`
- `rust/fontaine-sdk/src/core/models.rs` — `CostBasis`, `cost_basis`, `catalog_billed`, `priced`
- `rust/fontaine-sdk/src/core/{turn,side_question,goal,child_runner,session,scanner_stage,user_hooks,project_env}.rs`
- `rust/fontaine-sdk/src/core/agent/{goal,advisor,setup,models}.rs`
- `rust/fontaine-sdk/src/core/usage_ledger.rs`, `core/gate.rs`, `core/statusline.rs`, `run.rs`, `assemble.rs`
- `rust/fontaine-sdk/src/agents/budget.rs`
- `rust/fontaine-tui/src/{agents_panel,reports,agent_list}.rs`, `rust/fontaine-tui/src/ui/{turn,mod}.rs`
- `rust/fontaine/src/telemetry/span_bridge.rs`, `rust/fontaine/src/auto_mode.rs`
- `rust/fontaine-evals/src/live.rs`
- `docs/{providers,configuration,tui,headless,telemetry}.md`, `CHANGELOG.md`

## Trade-offs

- **Estimate, not the DBU bill** — the accurate figure needs account-admin scope, a SQL warehouse
  and hours of delay, and it is per workspace rather than per call, so it can never drive a session
  cap. The vendor list price is the only per-token figure available at call time. Rejected:
  deriving a rate from `system.billing.usage`.
- **An exact catalog entry only** — a shape-derived model (`derive()`) keeps no price. Its limits
  come from a model of the same shape, and its price would be a second guess stacked on the first;
  an unknown model's spend stays in `unpriced tok`, which is a warning the user can act on by
  mapping it under `model_facts`.
- **`model_facts` on `anthropic` stays unpriced** — that mapping exists for limits and tool
  behavior (an Azure Foundry deployment, a proxy alias), and changing its pricing is not this
  issue. On `databricks` a mapped name *does* take the target's price, since the mapping there is
  the only way a custom endpoint can be priced at all.
- **No `pricing` config key** — every dbx user wants the figure, and a key would add a validation
  surface, docs and tests for an opt-out nobody asked for. The `est.` marker is what keeps the
  figure honest.
- **`total_cost_usd` keeps its JSON type** — adding a sibling boolean to the `result` object
  breaks the Claude-SDK-compatible contract consumers parse. The qualifier lives in the
  documentation, and in the TUI where a human reads it.

## Documentation

- `docs/providers.md:18` — the pricing column for `databricks` becomes "estimated (list price)",
  and the paragraph at `:158` and the `--max-budget-usd` note at `:22` stop calling the models
  unpriced unconditionally.
- `docs/configuration.md:385-393` — the Databricks section: a listed Claude/GPT/Gemini model is
  priced at the served model's list price as an estimate; open models, `gpt-oss-*` and custom
  endpoints stay unpriced and keep `--max-budget-usd` a usage error. `:197` and `:311` split the
  "the mapped model is unpriced" rule by kind.
- `docs/tui.md:866` and `:1355-1366` — the dollar figure ends in ` est.` when it is an estimate, and
  the agents-panel paragraph gets the same clause.
- `docs/headless.md:40`, `:62`, `:77`, `:312` — `total_cost_usd` is a list-price estimate for an
  estimate-priced run; a priced dbx model makes `--max-budget-usd` and a preference list work.
- `docs/telemetry.md:130-138` — `fontaine.cost.usage` is present for a dbx model the catalog lists
  and carries `estimated = "true"`.
- `CHANGELOG.md` — one entry.

## Testing Strategy

- **Facts** (`derived_facts.rs` unit tests): `Price::Keep` keeps an exact entry's `pricing` and
  `Price::Strip` clears it; a derived entry keeps none under either; `facts_as` still relabels the
  id and the bounds. This is the whole price decision, so it is unit-tested at every arm.
- **Basis** (`models.rs` unit tests): `Billing::Catalog` → `Billed`, `Billing::Unpriced` →
  `Unpriced`, `Estimated` with and without a listed price → `Estimated`/`Unpriced`; `priced()` is
  false for every `Unpriced` arm.
- **Ledger and spend** (`usage_ledger.rs`, `gate.rs` unit tests): `estimated` set only by an
  estimate-priced call, carried through `merge` and through `spend_of`.
- **Display** (`agents_panel.rs`, `reports.rs` unit tests): `$0.0066 est.`, the budgeted form, a
  mixed bucket marked, and an unpriced bucket still `n/a` / `· N unpriced tok`. These are the
  strings the docs promise, so they are pinned where they are built.
- **Telemetry** (`span_bridge.rs` unit tests): a `databricks` completion under `model.priced` yields
  `fontaine.cost.usage` from the served vendor's price, carries `estimated = "true"` under a
  `model.estimated` span, and a `system.ai.glm-*` model still yields no cost.
- **Databricks integration** (`tests/databricks.rs`, wiremock): a `system.ai.claude-haiku-5-5` handle
  is `priced()`, `cost_basis() == Estimated`, and a reply's reported usage accrues a
  `cost_usd` on the ledger instead of `unpriced_tokens`; `system.ai.glm-5-3` stays unpriced.
- **Caps** (`tests/` for `--max-budget-usd`): a dbx Claude main model starts a headless run, a dbx
  GLM main model is still a usage error (exit 2).
- The live gateway is not covered by any test — `AGENTS.md` forbids live APIs in tests — so the
  one thing a mock cannot show, that a real dbx reply reports the token counts this prices, is the
  manual step below.

## Manual Verification

The one behavior no test can reach is a real workspace replying. It needs a dbx account configured
in the user's settings (`kind = "databricks"`, its workspace `base_url`, and
`api_key_env = "DATABRICKS_TOKEN"`); the token can be exported from the file at
`~/.claude_databricks_token`, never on a command line.

1. **A listed model is priced.** With `[model] model = "dbx/system.ai.claude-sonnet-5-5"`:
   `DATABRICKS_TOKEN=$(cat ~/.claude_databricks_token) cargo run -p fontaine -- -p "Reply with exactly: ok"`
   — expect exit 0 and a `result.total_cost_usd` that is a number well above zero, where today it
   is `null`. Not automated: it is a live, billed call.
2. **A cap binds on the estimate.** The same command with `--max-budget-usd 0.000001` — expect exit 1
   with `error_max_budget_usd`, not the exit 2 usage error it gives today.
3. **An open model is still unpriced.** With `[model] model = "dbx/system.ai.glm-5-3"` and
   `--max-budget-usd 1` — expect exit 2 and the "needs a known price" usage error, unchanged.
4. **The TUI says estimate.** `DATABRICKS_TOKEN=... cargo run -p fontaine`, one prompt, then
   `/usage` — expect the main row's dollar figure to end in ` est.` and no `unpriced tok` segment.
   Not automated: it is a rendering a human has to eyeball.

## Open Questions

1. **May a cost cap bind on an estimate?** The caps promise a dollar ceiling on real spend, and an
   estimate is the vendor's list price, not the DBU charge. This plan says yes (direction 1 of the
   issue, and the figure is marked ` est.` wherever a cap shows it), because the alternative leaves
   every dbx session uncapped, which is the state the issue reports.
2. **Should the headless `result` object say the figure is an estimate?** A script reading
   `total_cost_usd` cannot tell. This plan says no new field (contract stability) and a documented
   qualifier instead.
3. **Is a `pricing = "off"` escape hatch needed?** This plan says no: no user has asked to hide the
   figure, and the marker is what protects its reading.
