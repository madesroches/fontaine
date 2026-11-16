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
- `tests/databricks.rs:144` asserts `!handle.priced()` and `!handle.catalog_billed()`, and `:249` —
  in `facts_are_bound_under_the_served_id_on_the_responses_and_messages_wires` — asserts
  `spec.pricing == None` on a Messages-bound `system.ai.claude-haiku-5-5`, which `Price::Keep`
  flips to `Some`.

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

`Billing::Unpriced` keeps its meaning verbatim for `kimi`, `opencode-go` and `claude-subscription`,
so no other kind changes behavior.

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

### 3. Keep the vendor's price in dbx facts

`derived_facts::facts_as(vendor, served, base_id, served_id)` gains one parameter saying what the
gateway wants done with the price:

```rust
/// What a gateway does with the vendor entry's price. `Keep` is for a workspace that bills in
/// its own units: the vendor's list price is the only per-token figure there is.
pub enum Price { Keep, Strip }
```

Inside, `spec.pricing` is cleared unless `keep == Price::Keep` **and** the facts came from an exact
catalog entry (`listed`). A shape-derived entry never contributes a price. `details.cost` stays
cleared either way.

`databricks::facts_for` passes `Price::Keep`; `anthropic` passes `Price::Strip` and is unchanged.
`check_model_facts`, called from `validate`, passes `Strip` (it only asks whether a target
resolves).

The `OUTPUT_CAPS` fallback spec (`glm-*`) is built bare, so it keeps no price — a `system.ai.glm-5-3`
model reads `unpriced tok` exactly as today.

### 4. Thread the basis where the boolean goes

`CostBasis` replaces the `priced: bool` / `billed: bool` that each entry point carries:

- `core/turn.rs`: `TurnParams.priced` and `Drive.priced` become `cost: CostBasis`; the drop site
  becomes `if params.cost == CostBasis::Unpriced { usage.cost = None; }`, and
  `cost == CostBasis::Estimated` is what marks the ledger and the agent tree.
- `core/side_question.rs`, `core/goal.rs`, `core/child_runner.rs`, `core/session.rs`,
  `core/scanner_stage.rs`, `core/agent/{goal,advisor,setup,afk}.rs`, `core/user_hooks.rs`,
  `core/project_env.rs`, `core/permissions.rs`, `run.rs`, `fontaine-tui/src/ui/turn.rs`,
  `fontaine-tui/src/ui/commands/auto_mode_setup.rs`, `fontaine/src/auto_mode.rs`,
  `fontaine-evals/src/live.rs`: pass `handle.cost_basis()` where they pass
  `handle.catalog_billed()` today — `auto_mode_setup.rs:192` is today's `billed`. The guardian
  sites hold only a `Guardian`, so their basis comes from `guardian.settings()`
  (`guardian/guardian.rs:191`): `permissions.rs:2242` (`add_guardian_usage`), `:2307,2374`
  (`add_guardian_adjudication`), `agent/afk.rs:70,83` and `agent/setup.rs:795`
  (`add_guardian_usage_answered`). `permissions.rs:2408` (`note_composition`) takes the basis from
  `guardian.settings()` like the rest and keeps `Dropped.priced` a bool,
  `priced: basis != CostBasis::Unpriced`, so a dbx guardian-drop estimate still reaches
  `span_bridge::dropped_guardian_spend` priced. The composition-read site `permissions.rs:2249`
  records what the composition's own readers answered, not the guardian model, so it passes
  `CostBasis::Billed` and keeps today's count-as-reported behavior. The span field stays
  `model.priced` (bool) and gains `model.estimated` (bool) so a telemetry reader can tell the two
  apart.
- `guardian/guardian.rs`: `Settings.priced` (`:51`) and the cost drop that reads it (`:403`)
  become the basis, and the span field it records (`:236`) splits into `model.priced` /
  `model.estimated`; `guardian/scanner.rs` `LlmScanner.priced` (`:108`) and its span (`:199`)
  follow.
- `core/usage_ledger.rs` `UsageLedger::record` and every wrapper that feeds it — `core/gate.rs`
  `record_usage`, `record_usage_split`, `add_guardian_usage`, `add_guardian_adjudication`,
  `add_guardian_usage_answered` and `add_goal_usage` — take the basis too, so each bucket can be
  marked.

### 5. Carry "this is an estimate" to the figures

Two carriers, mirroring the existing `unpriced_tokens` convention:

- `core/usage_ledger.rs` `Bucket` gains `estimated: bool` — set when a priced call's basis was
  `Estimated`, merged with `||`. Its doc says an estimate is never presented as a bill, so a bucket
  holding both is marked.
- `agents/budget.rs` `Spend` gains the same field, combined by `Spend::add` (`:20`) with `||` too,
  so a sum holding one estimate is marked; `core/goal.rs:271` `spend_since` takes `estimated` from
  `now`. `core/gate.rs` `spend_of(usage, basis)` takes the basis — and so does
  `core/child_runner.rs` `spend` (`:1434`), the public wrapper over it; `record_usage_answered` and
  its four callers (`gate.rs:1324`, `:1385`, `:1405`, `:1425`) carry it through.

An estimate is rendered, never inferred: `spend_text` (`agents_panel.rs`) and `cost_cell`
(`reports.rs`) append ` est.` to the dollar figure when the bucket or spend is estimated, so
`$0.0066 est.` / `$0.0021 est. / $2.00`, next to `· 128k unpriced tok` where that still applies.
`TOOL` and truncation rules for these rows are unchanged; `est.` rides with the dollar segment.
A cost cap binds on a list-price estimate: an `--max-budget-usd`, a session cap and a goal's cost
cap all compare against the estimated figure, and every figure a cap shows carries ` est.`.
The master plan's `### Providers` decisions keep a cap inactive only for a model with no catalog
price, and prefer a cap that binds to one silently loosened by a stripped price.

Every other figure a dollar or a cap shows is marked the same way: the `/cost` footer
`Session cost budget: ${spent:.4} / ${cap:.2}` (`reports.rs:848-849`) through `UsageReport.budget`
(`reports.rs:762`), which becomes `Option<(f64, f64, bool)>` and is filled from `session_spend()` in
`ui/commands/reports.rs:38-47`; `Exhausted::Cost` and `Exhausted::SessionCost`
(`agents/budget.rs:62,72`), which take `Spend.estimated` so `describe_with` reads
`its cost budget is spent ($0.0120 est. of $2.00)`; the goal pause reason
`PauseReason::CostCap(f64, bool)` (`core/goal.rs:146,175`), the cap and `Spend.estimated`, set from
the stretch spend in `over_cost` (`:932`) and shown as `the goal's spend passed $2.00 est.`; and
`core/project_env.rs` `usage_line(usage, billed)` (`:639,647`), which takes the basis for the
setup run's figure.

`events.rs` `ResultEvent.total_cost_usd` and `statusline.rs` `CostInfo.total_cost_usd` keep their
JSON shape and are documented as a list-price estimate for an estimate-priced run.

### 6. Price the telemetry stream

`span_bridge::cost_usd` today only resolves catalog vendors. Add one seam in
`core/providers/mod.rs`:

```rust
/// The catalog price of a `(provider, model)` span pair: for a gateway-prefixed id, the served
/// vendor's list price; otherwise the catalog vendor's own entry.
pub fn listed_price(provider: &str, model: &str) -> Option<Pricing>
```

A pair goes to the kind's own `listed_price` when `provider` is the kind's name (`databricks`) or
when the id carries either prefix `base_id` strips — `system.ai.`, or the older `databricks-` —
whatever provider name reaches the function, so `listed_price` — and `cost_usd`, which calls it —
asks `databricks::listed_price(model)` for such a pair; otherwise it returns the catalog's own
entry for a catalog vendor. `databricks::listed_price(model)` is
`family`/`base_id`/`catalog_vendor_of` over the exact catalog entry, the same resolution
`facts_for` uses.

The prefix rule is what prices a dbx Gemini call: rig's Gemini wire hardcodes the provider name
`gcp.gemini`, and `databricks.rs:170` builds it with `GeminiConfig::new("")`, so the span arrives
as `gcp.gemini` / `system.ai.gemini-3-5-flash` and the provider name alone would take the
catalog-vendor branch and resolve nothing. `system.ai.gemini-3-pro` stays unpriced: `gcp.gemini`
lists `gemini-3-pro-preview` and `gemini-3-pro-image`, never `gemini-3-pro`, and `dotted_version`
leaves that id as it is, so the model goes down the derived path with no price.

A `model_facts`-mapped endpoint is the exception: `facts_for` prices it from the account's mapping,
so a mapped endpoint is priced in the ledger and `/cost` and has no OTLP cost metric.

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
5. Replace the `priced`/`billed` booleans with `CostBasis` at every site listed in Design §4 — the
   turn and per-agent paths, `guardian/guardian.rs` `Settings.priced`, `guardian/scanner.rs`
   `LlmScanner.priced`, the `permissions.rs`/`afk.rs`/`setup.rs` record calls and
   `auto_mode_setup.rs` — taking it from `handle.cost_basis()` where a handle is at hand, from
   `guardian.settings()` at the guardian-only sites (`permissions.rs:2242/2307/2374`, `afk.rs:70/83`,
   `setup.rs:795`, and `permissions.rs:2408` keeping `Dropped.priced` a bool as
   `basis != CostBasis::Unpriced`) and `CostBasis::Billed` at the composition-read site
   (`permissions.rs:2249`); add the `model.estimated` span field, and pass the basis down the ledger
   `record` chain. The integration tests that build those fields or pass that flag move with them:
   `tests/{agent_pipeline,audit_telemetry,side_question,account_session,afk}.rs`,
   `tests/{usage_pause_turn,effort,turn_recovery,guardian_scanner}.rs`,
   `tests/{agent_reply,max_tokens,memory,multimodal,native_search,output_styles}.rs`,
   `tests/{prefix_stability,rewind,teams,tool_search,priced,goal}.rs` and `fontaine/tests/embed.rs`.

**Phase 3 — count and label the estimate**

6. `core/usage_ledger.rs`: `Bucket.estimated`, set in `add`, folded in `merge`; unit tests.
7. `agents/budget.rs` `Spend.estimated`, combined with `||` in `Spend::add` and taken from `now` by
   `core/goal.rs` `spend_since`; `core/gate.rs` `spend_of(usage, basis)` and the
   `record_usage_answered`/`add_*_usage` chain, with the integration tests that call them directly
   (`tests/{billing,agent_pipeline,agent_resume,usage_pause_turn}.rs`) or build a `Spend`
   (`tests/goal.rs`) updated to match. `core/child_runner.rs:1434` `spend(usage)`, the public
   wrapper over `spend_of`, takes the basis too, or goes if it has no callers.
8. Displays: `agents_panel.rs` `SpendView.estimated` + `spend_text`, `reports.rs` `cost_cell` and
   the `/cost` footer's `budget` flag, `agents/budget.rs` `Exhausted::{Cost,SessionCost}`,
   `core/goal.rs` `PauseReason::CostCap`, `core/project_env.rs` `usage_line`, the `PanelRow`
   literals in `fontaine-tui/src/{app,screen}.rs`, and the TUI row plumbing that fills them.

**Phase 4 — telemetry**

9. `fontaine/src/telemetry/span_bridge.rs`: `cost_usd` through `listed_price`, which takes a pair
   to `databricks::listed_price` when the provider is `databricks` or the id carries either prefix
   `base_id` strips (`system.ai.`, `databricks-`), and the `estimated = "true"` attribute on the
   cost metric when the enclosing span says so.

**Phase 5 — docs and changelog**

10. The documentation edits listed below, and a `CHANGELOG.md` entry.
11. `tasks/fontaine_harness_plan.md`: scope its `### Providers` line "a mapped model stays unpriced"
    to `anthropic`, and append this plan's decisions — `Billing::Estimated`, the ` est.` marker, a
    cap binding on an estimate, and a mapped `databricks` model taking the target's price — as dated
    lines at the end of that subsection, as `tasks/completed/host_model_facts_plan.md` step 5 did.

## Files to Modify

- `rust/fontaine-sdk/src/core/providers/mod.rs` — `Billing::Estimated`, `listed_price` seam
- `rust/fontaine-sdk/src/core/providers/derived_facts.rs` — `Price`, `facts_as`
- `rust/fontaine-sdk/src/core/providers/databricks.rs` — `billing()`, `listed_price`, `facts_for`
- `rust/fontaine-sdk/src/core/providers/anthropic.rs` — pass `Price::Strip`
- `rust/fontaine-sdk/src/core/models.rs` — `CostBasis`, `cost_basis`, `catalog_billed`, `priced`
- `rust/fontaine-sdk/src/core/{turn,side_question,goal,child_runner,session,scanner_stage,user_hooks,project_env,permissions}.rs`
- `rust/fontaine-sdk/src/core/agent/{goal,advisor,setup,afk}.rs`
- `rust/fontaine-sdk/src/core/usage_ledger.rs`, `core/gate.rs`, `core/statusline.rs`, `run.rs`
- `rust/fontaine-sdk/src/guardian/{guardian,scanner}.rs`
- `rust/fontaine-sdk/src/agents/budget.rs`
- `rust/fontaine-tui/src/{agents_panel,reports,agent_list,app,screen}.rs` (`PanelRow` literals in
  the last two), `rust/fontaine-tui/src/ui/{turn,mod}.rs`,
  `rust/fontaine-tui/src/ui/commands/{auto_mode_setup,reports}.rs`
- `rust/fontaine/src/telemetry/span_bridge.rs`, `rust/fontaine/src/auto_mode.rs`
- `rust/fontaine-evals/src/live.rs`
- `rust/fontaine-sdk/tests/{databricks,billing,audit_telemetry,agent_pipeline,agent_resume}.rs`,
  `rust/fontaine-sdk/tests/{side_question,guardian_scanner,turn_recovery,account_session,afk}.rs`,
  `rust/fontaine-sdk/tests/{usage_pause_turn,effort}.rs`,
  `rust/fontaine-sdk/tests/{agent_reply,max_tokens,memory,multimodal,native_search}.rs`,
  `rust/fontaine-sdk/tests/{output_styles,prefix_stability,rewind,teams,tool_search,priced,goal}.rs`,
  `rust/fontaine/tests/embed.rs`
- `docs/{providers,configuration,tui,headless,telemetry}.md`, `CHANGELOG.md`,
  `tasks/fontaine_harness_plan.md`

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
  the only way a custom endpoint can be priced at all — in the ledger and `/cost`, with no OTLP
  cost metric, since the span carries only the provider and model strings.
- **No `pricing` config key** — every dbx user wants the figure, and a key would add a validation
  surface, docs and tests for an opt-out nobody asked for. The `est.` marker is what keeps the
  figure honest.
- **`total_cost_usd` keeps its JSON type** — adding a sibling boolean to the `result` object
  breaks the Claude-SDK-compatible contract consumers parse. The figure stays a plain number and
  the qualifier is documented in `docs/headless.md`, and shown in the TUI where a human reads it.

## Documentation

- `docs/providers.md:18` — the pricing column for `databricks` becomes "estimated (list price)",
  and the paragraph at `:158` and the `--max-budget-usd` note at `:22` stop calling the models
  unpriced unconditionally.
- `docs/configuration.md:385-393` — the Databricks section: a listed Claude/GPT/Gemini model is
  priced at the served model's list price as an estimate; open models, `gpt-oss-*` and custom
  endpoints stay unpriced and keep `--max-budget-usd` a usage error. `:197` and `:311` split the
  "the mapped model is unpriced" rule by kind.
- `docs/tui.md:195`, `:866` and `:1355-1366` — the statusline's `cost.total_cost_usd` is a
  list-price estimate for an estimate-priced run, the dollar figure ends in ` est.` when it is an
  estimate, and the agents-panel paragraph gets the same clause.
- `docs/headless.md:40`, `:62`, `:77`, `:312` — `total_cost_usd` is a list-price estimate for an
  estimate-priced run; a priced dbx model makes `--max-budget-usd` and a preference list work.
- `docs/telemetry.md:130-138` — `fontaine.cost.usage` is present for a dbx model the catalog lists
  and carries `estimated = "true"`.
- `docs/telemetry.md:126` and `:146-148` — `estimated` on `fontaine.cost.usage` marks two things: a
  guessed guardian-drop spend, and a cost priced at a vendor's list price for an account that bills
  in its own units. The `fontaine.token.usage` row (`:125`) keeps the guardian estimate only.
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
  `model.estimated` span, a `gcp.gemini` completion for `system.ai.gemini-3-5-flash` (dotted to the
  listed `gemini-3.5-flash`) yields the same cost, a `databricks` completion for the older
  `databricks-claude-haiku-5-5` spelling yields it too, and a `system.ai.glm-*` or an unlisted
  `system.ai.gemini-3-pro` model still yields no cost.
- **Databricks integration** (`tests/databricks.rs`, wiremock): a `system.ai.claude-haiku-5-5`
  handle is `priced()` and `cost_basis() == Estimated`, and each wire's reply reports a price at the
  existing `completion.call` seam — `usage.cost.is_some()` for Messages, `responses_reply()` and
  `gemini_reply()`. The Gemini case needs a new `system.ai.gemini-3-5-flash` entry in the file's
  `MODELS` list, a second route constant beside `GEMINI`, and a `gemini_reply()` mounted on it;
  `GEMINI` stays the `system.ai.gemini-3-pro` route, which `:187-192` and `:341-346` keep calling.
  No cost for `system.ai.glm-5-3` or the derived `system.ai.gemini-3-pro`, the no-cost case calling
  `gemini-3-pro` on `GEMINI` itself. `say_hi` returns the response rather than only its text so the
  assertion can read `usage.cost`; the `estimated` bucket marking stays with the ledger and gate
  unit tests.

## Manual Verification

The one behavior no test can reach is a real workspace replying. It needs a dbx account configured
in the user's settings (`kind = "databricks"`, its workspace `base_url`, and
`api_key_env = "DATABRICKS_TOKEN"`); the token can be exported from the file at
`~/.claude_databricks_token`, never on a command line.

1. **A listed model is priced.** With `[model] model = "dbx/system.ai.claude-sonnet-5-5"`:
   `DATABRICKS_TOKEN=$(cat ~/.claude_databricks_token) cargo run -p fontaine -- --output-format
   json -p "Reply with exactly: ok"` — expect exit 0 and a `result.total_cost_usd` that is a
   number well above zero, where today it is `null`. Not automated: it is a live, billed call.
