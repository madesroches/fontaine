# Provider Guide Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/578

## Overview

Add `docs/providers.md`, a guide that documents every registered account kind, helps the reader
choose between them, and gives each a minimal working example. Link it from the README, both docs
navigation files and the account section of `docs/configuration.md`. Add a test that fails when a
registered kind is missing from the guide. Per the user's instruction during this run, the
`claude-subscription` kind is no longer presented as experimental: the label comes out of the docs
and the few user-visible strings that carry it.

## Current State

- Provider facts live in `docs/configuration.md` (`### [accounts.<name>]`, lines ~173-535: key
  table, a "What each kind talks to" table, credential rules, model discovery, worked examples) and
  `docs/claude-subscription.md` (protocol, checks, safety). No provider overview exists.
- `README.md:63` lists "Anthropic, OpenAI, Gemini, OpenRouter, Ollama and OpenAI-compatible
  endpoints, plus a Claude subscription", omitting Kimi, OpenCode Go and Databricks.
- The registry is `providers::names()` (`rust/fontaine-core/src/providers/mod.rs:426`), exposed as
  `accounts::kinds()` (`accounts.rs:403`). It holds ten kinds.
- "experimental" for `claude-subscription` appears in: `docs/claude-subscription.md` (title and
  intro), `docs/configuration.md:181,525`, `docs/index.md:61`, `docs/tui.md:968`,
  `fontaine-core/src/accounts.rs:63` (`login owned by the external CLI (experimental)`),
  `fontaine-tui/src/provider_dialog.rs:245`, and `providers/claude_subscription/cli.rs:470` (error
  text). `tests/claude_subscription.rs:130` already asserts no experimental notice is emitted.
  `CHANGELOG.md` history entries are left as written.

## Design

### `docs/providers.md`

Structure:

1. Intro: an account is `[accounts.<name>]`; models are addressed `<account>/<model>`; this page
   chooses and shows, `configuration.md` is the exhaustive key reference.
2. **Choosing a kind** comparison table, one row per kind: service, wire(s), default endpoint /
   `base_url` requirement, credential, pricing/budget behavior (priced vs unpriced,
   `--max-budget-usd`), and notable capabilities (prompt caching, native web search, quota).
3. **Easily confused choices**, one short subsection each, covering the five pairs from the issue:
   `kimi` vs `openai-compatible` (Moonshot pay-as-you-go), `openrouter` vs `openai-compatible` at
   OpenRouter (cache markers), `databricks` vs `anthropic` with `auth = "bearer"`, multi-wire
   (`opencode-go`, `databricks`) vs single-wire, and `claude-subscription` using the CLI's Max/Ultra
   login rather than a Fontaine credential.
4. **One section per kind**, in registry order, each a `## \`kind\`` heading with: what it is for and
   its wire; endpoint / `base_url`; authentication (including `/login` sign-in and external-CLI
   login); model ids and discovery; a minimal TOML example; kind-specific options and limits
   (`prompt_caching`/`cache_ttl`, `auth`, `wires`, `model_facts`, `context_tokens`, `think`,
   `quota`, quota display, pricing, native web search). Facts are taken from `configuration.md` and
   the code; the guide links to `configuration.md` anchors for key semantics and to
   `claude-subscription.md` for CLI checks and scope rather than copying them.

Facts must be verified against the code while writing (the native web search availability per kind
is in `fontaine-core/src/native_search.rs`; pricing in each provider module), not copied
unchecked.

### Experimental label removal

Delete the "experimental" framing for `claude-subscription`:

- `docs/claude-subscription.md`: title becomes `# Claude subscription`; drop "It is an
  experiment: selecting such an account ... prints a notice saying so" (the code emits none) and
  the "not offered as a stable feature" clause. Keep the scope limits and the instruction to check
  Anthropic's terms, since those are policy, not stability.
- `docs/configuration.md:181,525`, `docs/index.md:61`: drop the word.
- `accounts.rs:63`, `provider_dialog.rs:245`, `docs/tui.md:968`: drop `(experimental)` from the
  account-row state and the checking note; update any test or snapshot that asserts the string.
- `cli.rs:470`: reword "the only plans this experimental adapter accepts" to "the only plans this
  account kind accepts"; update the test asserting it, if any.

### Registry-coverage test

A test in `rust/fontaine-core/tests/` (new `provider_docs.rs`, or an existing providers test file)
reads `docs/providers.md` via `CARGO_MANIFEST_DIR/../../docs/providers.md` (the pattern used by
`fontaine/tests/analytics_notebook.rs`) and asserts, matching whole lines, for every name in `providers::names()`, that
the guide has a line `` ## `<kind>` `` and a table line starting with `` | `<kind>` ``, and that
the section headings appear in registry order. A new registered kind then fails CI until documented.

The `claude-subscription` comparison row and section replace the label with the subscription-specific
facts: Max/Ultra plans only, the external CLI login (no Fontaine credential), the scope limits and
the Anthropic-terms caveat, each linking to `claude-subscription.md`. This is how the issue's
"experimental and subscription-specific behavior remains clearly identified" item is met once the
label is dropped; the PR description notes that the issue's "(experimental)" wording is superseded.

## Implementation Steps

1. Verify per-kind facts in `providers/*.rs`, `native_search.rs`, `models.rs` and
   `configuration.md` (endpoints, wires, auth, caching defaults, pricing, web search, quota).
2. Write `docs/providers.md` per the structure above.
3. Link the guide: `docs/SUMMARY.md` (entry directly after Configuration) and `docs/index.md`
   (entry directly after Configuration, line 13); README line 63 becomes "Any provider: the
   [provider guide](docs/providers.md) covers Anthropic, OpenAI, Gemini, OpenRouter, Ollama, Kimi,
   OpenCode Go, Databricks, OpenAI-compatible endpoints and a Claude subscription."; line 54 also
   points at the guide; `docs/configuration.md`: add a pointer at the top of
   `### [accounts.<name>]` and in the `kind` row of the key table. `docs/index.md:61` becomes
   "the account kind that runs your installed Claude Code CLI as the model transport on a Max or
   Ultra plan, its checks, limits and billing caveats."
4. Remove the experimental label (list above) and fix affected tests.
5. Add the registry-coverage test.
6. Add a `CHANGELOG.md` entry under Unreleased: the provider guide, and `claude-subscription` no
   longer labeled experimental.
7. Add the dated line to `tasks/fontaine_harness_plan.md` Decisions, at the end of the subsection
   holding the 2026-11-04 `claude-subscription` lines: the kind is no longer experimental (user
   directive 2026-11-13), superseding "experimental" in the 2026-11-04 line, and the policy
   constraints it asks to re-read stay in `docs/claude-subscription.md` unchanged.
8. Run `.github/scripts/docs-site.sh <dir>` on Linux (WSL or CI) before declaring done; it is a
   definition-of-done gate. Also check by hand that every relative link and `configuration.md#...`
   anchor in the new page resolves.

## Files to Modify

- New: `docs/providers.md`, `rust/fontaine-core/tests/provider_docs.rs`
- `README.md`, `docs/SUMMARY.md`, `docs/index.md`, `docs/configuration.md`,
  `docs/claude-subscription.md`, `docs/tui.md`, `CHANGELOG.md`
- `tasks/fontaine_harness_plan.md` (one Decisions line)
- `rust/fontaine-core/src/accounts.rs`, `rust/fontaine-tui/src/provider_dialog.rs`,
  `rust/fontaine-core/src/providers/claude_subscription/cli.rs` and tests asserting those strings

## Trade-offs

- A guide that summarises and links, rather than moving the configuration reference's content:
  the issue asks for no competing copies. The cost is some deliberate repetition of one-line facts
  in the comparison table; the test and links keep the detail in one place.
- Test checks kind names only, not facts: facts drift is not mechanically checkable, and the
  issue's requirement is that a new kind is not silently omitted.

## Decisions

- 2026-11-13 (user): `claude-subscription` is no longer documented or labeled experimental; the
  scope and terms caveats stay. The master plan records it (step 7).
- 2026-11-13: CHANGELOG history entries that say "experimental" are left as written, as they record
  past releases.

## Documentation

This plan is documentation: `docs/providers.md` is new; `README.md`, `docs/index.md`,
`docs/SUMMARY.md`, `docs/configuration.md`, `docs/claude-subscription.md` and `docs/tui.md` change
as above. `tasks/fontaine_harness_plan.md` Decisions gets the dated line (step 7).

## Testing Strategy

- The registry-coverage test above (automated; a unit-level file read, no live service).
- Existing tests asserting the removed strings are updated; `cargo test` for `fontaine-core` and
  `fontaine-tui`.
- `cargo fmt --check` and `cargo clippy --all-targets -- -D warnings`.
- The docs-site build and link check (`.github/scripts/docs-site.sh`) in CI.

## Open Questions

None.
