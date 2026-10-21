# Documentation Plan

Part of the Fontaine design; the master plan `tasks/fontaine_harness_plan.md` holds the Phase Tracker, workspace layout and `## Decisions`.

## Documentation

All product documentation lives under `docs/` — a standalone directory, one topic per page,
relative cross-links, current behavior only (no plan/phase citations in the docs themselves).
The tree is kept **website-ready**: lifting `docs/` into a static site is a likely future step,
explicitly **not urgent** — no site tooling ships in v1, and the only forward constraint is the
file layout above. Root `README.md` is the thin front door (pitch, install, quickstart, mode
table, auto-mode explainer) that links into `docs/`, not a second copy of it; its quickstart leads
with the local setup — an Ollama agent model plus an Ollama guardian, no cloud key. Each page is
delivered (or extended) by the docs step of the phase named with it — see the phase plans.

- `docs/index.md`: landing page / TOC (Phase 0).
- `docs/architecture.md`: workspace map, agent-loop and permission-pipeline diagrams, surfaces
  (TUI + headless), workspace roots, session ledger vs model context, session persistence/resume/fork
  (skeleton Phase 0; grown in Phases 1, 2, 3, 5).
- `docs/configuration.md`: every config key, the settings scope stack and per-key scope table,
  trust asymmetry, workspace trust, account and provider setup examples — seeded from the `config.md` §Config schema (Phase 0) and documenting every key from Phase 0; a key that is parsed but not yet active says "accepted and validated; not yet active".
- `docs/tools.md`: built-in tool inventory, risk classes, the containment fast path, shell
  hardening (Phase 2).
- `docs/permissions.md`: mode table, deterministic floor, the `Tool(specifier)` rule grammar and
  allow rules (incl. suspension in `auto`), Tier-0.5 pre-classifier, `auto` and the `[auto_mode]`
  prose lists (Phases 3–4).
- `docs/headless.md`: `fontaine -p`, the CLI flag table, exit codes, denial semantics, and the
  versioned `json`/`stream-json` event schema (Phase 1; grown in Phases 2–5 as flags land).
- `docs/telemetry.md`: enabling telemetry (the required ingestion URL and ingestion key — env-var
  name or stored credential, never inline; enabled without either is a config error), both backends
  with recipes for micromegas native (span trees via async span events, queried through
  `process_spans`) and micromegas over OTLP, the full metric/event/span table, privacy defaults and
  opt-in keys, the scope rule (project may only set `enabled = false`), and telemetry vs the audit
  log (Phase 1; grown in Phases 2–6).
- `docs/guardian.md`: threat model, request/verdict schema, rule precedence, prompt-versioning
  policy, how to choose a guardian model, known limits (latency, injection surface, eval results),
  result scanning, and attribution of Anthropic-derived rule prose (and MIT-licensed
  LlamaFirewall prompts if mined) (Phase 4; scanning in Phase 6).
- `docs/tui.md`: keybinds, slash commands, approval + denial UX, background-agents panel
  (Phases 3–5).
- `docs/evals.md`: the published guardian scorecard — per model, per profile (Phase 4).
- `docs/mcp.md`: MCP server config + trust model (Phase 5).
- `docs/agents.md`: parallel subagents, budgets, mode inheritance, agent files and project-agent
  narrowing (Phase 5).
- `docs/hooks.md`: user hooks — events, handler I/O, place in the pipeline, composition, failure
  semantics, scopes and trust (Phase 5).
- `docs/extensions.md`: skills, custom slash commands, output styles, file layout and frontmatter,
  porting from `.claude/` (Phase 5).
- `docs/sandboxing.md`: sandbox-interop integration path (srt/bubblewrap spike — Phase 5 roadmap
  slot); sandbox = blast radius, guardian = intent, complementary. Documents the container
  containment probe (`--in-container` verification) and the escape-detection test suite that gates
  `[guardian] profile = "minimal"` — the escape classes minimal must still deny.
- `evals/README.md`: golden-dataset format + runner usage (Phase 0) — the dataset is a public
  asset, so its format doc ships with it.
- `CONTRIBUTING.md` (repo root, Phase 0): build/test commands, branch and commit rules, how to submit
  golden-dataset cases; guardian-bypass reports are routed to `SECURITY.md`.
- `SECURITY.md` (repo root, Phase 0): private reporting via GitHub private vulnerability reporting, 90-day coordinated disclosure window.
- `CHANGELOG.md`: release notes in Keep a Changelog format — created in Phase 0 with an `## [Unreleased]`
  section; every phase appends an entry there in its docs step (plan/phase citations belong here, not
  in the docs pages); at release, `[Unreleased]` moves under the new version heading.
- README install section: prebuilt binaries from GitHub Releases (Linux, macOS, Windows) or
  `cargo install --git https://github.com/madesroches/fontaine fontaine` (Phase 4).
