# oh-my-pi Harness Study Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/213

## Overview

Study oh-my-pi (`can1357/oh-my-pi`, command `omp`, MIT, TypeScript on Bun with Rust native
crates; a fork of `badlogic/pi-mono`) and record what Fontaine should adopt, adapt or skip. The
deliverable is `research/research-oh-my-pi-harness.md`, built around two lists: desirable
features (led by those that let the model iterate) and ideas to adopt or adapt, each with its
security implications under Fontaine's invariants. It is a research spike in the pattern of the
DeepSeek study (`completed/deepseek_harness_study_plan.md`, #154): read the pinned source, run
the harness in a disposable container, record every finding including negative ones, and file
follow-up issues. No product crate changes. Unlike the DeepSeek study there is no vendor model
to support, so the model-specific questions become "how does it adjust per provider, and what
matters for Fontaine's rig path".

## Current State

**The subject.** Shallow clone pinned for the study: commit
`dcafcef6422c6a260649b879d75a5aabb1ea778e` (2026-10-26, `main`). The repo is very active (pushed
the day of the clone), so every citation in the doc is `path@dcafcef6`. The repo is large
(~700 MB); the study reads only what the questions need.

**Known shape from a first look** (to be verified, not trusted):

- Packages: `packages/agent` (loop: `agent-loop.ts`, `append-only-context.ts`, `compaction/`,
  `replay-policy.ts`, `output-budget.ts`, `speculative-execution.ts`), `packages/ai` (providers),
  `packages/coding-agent` (the product), `packages/tui`, `packages/natives` plus `crates/`
  (Rust), `packages/typescript-edit-benchmark`, `packages/mnemopi` and `packages/snapcompact`.
- Tools in `packages/coding-agent/src/tools/` go well beyond read/write/edit/bash: `ast-grep`,
  `ast-edit`, `grep`, `glob`, `todo`, `think`, `resolve`, `yield`, `wait`, `ask`, `fetch`,
  `browser`, `eval`/`run-code`, `ida`, gh helpers, memory tools, `report-tool-issue`.
- Editing: `src/edit/` (`hashline-compact.md`, `auto-repair.ts`, `normalize.ts`,
  `blackbox.ts`) and docs under `docs/tools/` (`read.md`, `write.md`, `ast-edit.md`, `grep.md`
  mention hashline anchors). Also `tools/conflict-detect.ts`, `file-mutation-queue.ts`,
  `read-supersede.ts`.
- Code intelligence: `src/lsp/`, `src/dap/`, `docs/lsp-config.md`.
- Loop guards and iteration: `session/tool-call-loop-redirect.ts`, `turn-recovery.ts`,
  `unexpected-stop-classifier.ts`, `stream-guards.ts`, `retry-fallback-chains.ts`,
  `tools/tool-timeouts.ts`, `src/goals/`, `src/ratchet/`, `src/autoresearch/`,
  `docs/non-compaction-retry-policy.md`, `docs/ttsr-injection-lifecycle.md` (TTSR: rule injection
  triggered by model output).
- Context: `docs/compaction.md`, `src/compress/`, `packages/snapcompact`,
  `session/cache-warmer.ts`, `read-supersede.ts`, `docs/blob-artifact-architecture.md`,
  `docs/handoff-generation-pipeline.md`, `docs/context-files.md`.
- Tasks and subagents: `tools/todo.ts`, `src/plan-mode/`, `src/task/`, `docs/task-agent-discovery.md`,
  `session/sub-sessions.ts`, `docs/agent-hub.md`.
- Extensibility: `docs/extensions.md`, `hooks.md`, `custom-tools.md`, `skills.md`,
  `marketplace.md`, `mcp-config.md`, `rulebook-matching-pipeline.md`, `extension-loading.md`.
- Safety: `docs/approval-mode.md` (tiers `read`/`write`/`exec`; modes `always-ask`,
  `always-ask-cached`, `write`, and `yolo` as the **default**; MCP tools declare `write`
  regardless of annotations), `src/security/`, `tools/bash-interceptor.ts`,
  `tools/shell-tokenize.ts`, `src/secrets/`.
- Session log: `session/session-manager.ts`, `session-entries.ts`, `session-persistence.ts`,
  `docs/session.md`, `session-tree-plan.md`, `session-operations-export-share-fork-resume.md`.
  Third-party sources claim JSONL under `~/.omp/agent/sessions/`; the doc must confirm or
  correct.
- Telemetry: `telemetry-export*.ts`, `packages/agent/src/telemetry.ts`, `docs/install-id.md`,
  `src/stats/`, `src/live/`, `src/collab/`.
- Providers: `docs/providers.md`, `provider-quirks.md`, `provider-compat-reference.md`,
  `provider-endpoint-constraints.md`, `ai-schema-normalize.md`, `ERRATA-GPT5-HARMONY.md`,
  `local-models.md`.

**Fontaine's side** (the comparison column): edit/code-intelligence plans are tracked in #27;
prior study docs are `research/research-deepseek-harness.md` and
`research/research-claude-code-parity.md`; the permission floor and guardian invariants are in
`AGENTS.md`. Session format, compaction, task list, instruction hierarchy and subagents each
have a completed plan under `tasks/completed/`; the doc reads the relevant ones before it
writes a "Fontaine today" line. Fontaine issues the findings may feed are looked up with
`gh issue list` at write time, not guessed here.

**Environment constraints found on this machine:** no `bun` or `node` on the host; Docker and
`bwrap` are present. omp therefore runs in Docker only.

## Design

### Method

Two tracks: read the pinned source, and run the harness. Every claim in the doc carries its
evidence: a `path@dcafcef6` citation, a captured request or log line, or a command and its
output. Third-party write-ups (hashline, LSP, DAP, session path) are claims to test against
source, and the doc says which held. Negative results are findings ("X does not exist", "Y
failed because Z"). Anything that cannot be run goes into a "Not tried" list with the reason.

### Environment and safety

- omp runs **only in a Docker container** (Bun image pinned by digest in the script), with a
  throwaway workspace, a throwaway `OMP_HOME`/config dir (confirm the env var in
  `docs/environment-variables.md`) and no host mounts except
  `research/oh-my-pi-harness/runs/<run>/`. Build omp inside the image (clone at the pinned
  commit, `bun install`; native crates build only if the run needs them — record if the
  prebuilt addon loads instead).
- **No real credential enters the container.** omp gets a dummy key. A host recording proxy
  (stdlib Python, run with `python3 -I`) on the docker bridge address injects the real upstream
  auth, read-only from Fontaine's `credentials.toml` as in the DeepSeek study, and redacts it
  in captures. The provider is whichever the user's Fontaine credentials reach (the opencode-go
  gateway's OpenAI-compatible route is the expected one); the doc records which, because omp's
  per-provider adjustments depend on it. If omp cannot be pointed at a base URL, that is a
  recorded finding and the run falls back to source-only for those questions.
- The container is on a Docker `--internal` network whose only exit is a `socat` sidecar to the
  proxy, with a `tcpdump` sidecar logging DNS and packets. This records every endpoint omp
  contacts by default (telemetry question) without changing the host firewall.
- omp defaults to `yolo` approval mode, so hostile-command experiments run only inside the
  container and the fixture workspace.
- Captures are scrubbed (`scrub.py`) and checked with `rg` for credential-shaped strings and
  literal token values before commit; the check reports file and line, never the value.

### Experiments

Each has a script under `research/oh-my-pi-harness/scripts/`, a fixture (a tiny repo with a
failing test) under `fixtures/` where needed, and a results section in the doc.

| # | Experiment | Answers |
|---|---|---|
| E1 | Build, boot headless/print mode, record the default tool list, settings defaults (approval mode, telemetry, session path), and every egress endpoint (`egress.pcap`, errors) with no prompt sent and with one prompt | architecture; defaults; telemetry and privacy; session path claim |
| E2 | One read-only question with the wire captured | system prompt framing, tool schemas, tool-call/result formatting, headers, install-id or other identifiers; diff against Fontaine's capture of the same task through `fontaine -p` |
| E3 | Fix a failing test in the fixture (read → edit → run tests → iterate), with triggers for each iteration mechanism: identical repeated failing call (`tool-call-loop-redirect`), a hanging command (`tool-timeouts`), an unexpected stop (`unexpected-stop-classifier`, `turn-recovery`), a tool error on a bad edit anchor (`edit/auto-repair`), a goal/ratchet if reachable headlessly. A mechanism that does not fire is "did not fire" plus the trigger used, distinct from "source-only" | the iteration loop: guards, continuation, timeouts, error/test/lint/diagnostic feedback |
| E4 | Edit formats: the same set of edit tasks (multi-hunk, whitespace-sensitive, stale-file, duplicate-match) through omp's hashline edit and through Fontaine's edit tool, capturing the model-visible `read` output and the edit call. Check LSP diagnostics after an edit if the language server is installable in the image; record DAP only from source unless a debugger can be driven | edit format and reliability, code intelligence vs #27 |
| E5 | Risky actions in the fixture (write outside workspace, `rm -rf` of a parent, `curl \| sh`, reading a planted fake secret) under default (`yolo`), `write` and `always-ask` modes, recording what the model is told in each; probe `bash-interceptor` and `shell-tokenize` for what they classify and whether any matches on arguments | safety model vs the guardian and deterministic floor |
| E6 | A long session that crosses the compaction threshold plus a large tool result and a repeated read of one file (`read-supersede`) | compaction trigger and summary prompt, truncation/spill of tool results, supersede, cache accounting |
| E7 | A multi-step task with `todo`, plan mode, and one subagent (`task`) | task representation, plan handoff, subagent context and return value, mode inheritance of the child |
| E8 | Resume and fork the E3 session, then inspect the on-disk log (format, one record type per line?, tree structure, what is stored for tool results and blobs), plus export | session log, resume/branch/review |
| E9 | A project-scoped extension, hook, skill and MCP config planted in the fixture, to see what loads without a prompt and what a project can loosen | extension trust model vs Fontaine's "project scope can only tighten" |
| E10 | Where omp's behaviour might differ from Claude Code, capture the baseline with `claude -p` on the same fixture (never from tests or CI) | the "Claude Code does it?" column |

Provider handling (E2 plus source): read `provider-quirks.md`, `provider-compat-reference.md`
and `ai-schema-normalize.md`, list each per-model adjustment, and mark each as relevant or not to
Fontaine's rig provider path by checking `rust/fontaine-core` provider code.

### The research doc

`research/research-oh-my-pi-harness.md`, following `research-deepseek-harness.md`:

1. Executive summary and bottom line.
2. Identity: repo, license, pinned commit, maturity, how it runs, lineage from pi-mono.
3. Architecture and turn flow.
4. One section per issue question (iteration; tools and editing; framing; context management;
   tasks and subagents; extensibility; safety; session log; telemetry and privacy; provider
   handling). Each has "What omp does", "Claude Code", "Fontaine today", evidence, verdict.
5. **Desirable features**, the centrepiece, led by those that let the model iterate. Each entry:
   what omp does; evidence; the Fontaine issue it feeds or a new one (or skip with reason);
   whether Claude Code does it.
6. **Ideas to adopt or adapt**: each with the security implication under Fontaine's invariants
   (guardian fail-closed, floor gates every mode, no regex on tool arguments, child never looser,
   project scope only tightens, every tool has a risk class).
7. Experiment log (E1–E10), with commands, results and failures.
8. Not tried, and why.

### Follow-ups and decisions

- For each feature worth adopting, file a GitHub issue (labels per area, linking the doc
  section) or comment on the existing issue it feeds (check #27 for editing/code intelligence).
  A bug found in Fontaine gets a `bug` issue with reproduction.
- A finding that argues for departing from Claude Code semantics gets a dated line in
  `tasks/fontaine_harness_plan.md` `## Decisions`. Adopting something Claude Code also does needs
  none.
- The doc and the PR body both carry the full findings list, negatives included.

## Implementation Steps

1. **Scaffold** `research/oh-my-pi-harness/` with `scripts/`, `fixtures/`, `captures/`, a
   `.gitignore` for `runs/`, and a `README.md` (reproduction steps, pinned commit, image digest).
2. **Source read.** Read the pinned checkout's docs and packages listed in Current State, with
   `path@dcafcef6` notes per question. Use Explore subagents for the broad sweeps; read cited
   files directly before quoting.
3. **Tooling** under `scripts/`: `omp.Dockerfile`, `net.Dockerfile` (`socat`, `tcpdump`),
   `record_proxy.py`, `scrub.py` (stdlib only), `run_omp.sh` (network setup, sidecars, run,
   teardown), `run_fontaine.sh`, and the fixture repos.
4. **E1–E9** on omp in order, recording each result in the doc as it lands.
5. **E10** `claude -p` baselines for rows where the Claude Code behaviour is in doubt.
6. **Write the doc**: summary, both lists, experiment log, Not tried.
7. **Follow-ups.** File and comment on issues; link them from the lists; add `## Decisions`
   lines only where the rule above applies.
8. **Index.** Add the doc to the research list in `tasks/fontaine_harness_plan.md` (~line 53-60)
   and fix the count there and in the workspace tree; add a short "oh-my-pi" subsection to
   `tasks/design/research-findings.md` pointing to the deep-dive; add a `CHANGELOG.md` entry.

## Files to Modify

- New: `research/research-oh-my-pi-harness.md`
- New: `research/oh-my-pi-harness/{README.md,.gitignore,scripts/*,fixtures/*,captures/*}`
- `tasks/fontaine_harness_plan.md`: research doc list and count; `## Decisions` only if needed
- `tasks/design/research-findings.md`: pointer subsection
- `CHANGELOG.md`

## Trade-offs

- **Docker rather than a host install.** The host has no Bun or Node, and the container gives the
  isolation `yolo`-by-default needs. The cost is a heavy build; native crates may be skipped if
  the prebuilt addon loads, and the doc records which.
- **Recording proxy over a gateway rather than a direct provider key.** Reuses the credentials
  Fontaine already holds and keeps real secrets out of the container. Gateway quirks may differ
  from the native provider API; each gap goes in Not tried.
- **Targeted reading, not a full audit.** The repo has dozens of subsystems (memory, IDA, TTS,
  collab, security scanning). Only those that answer an issue question are studied; the rest are
  listed in the doc as out of scope so the omission is explicit.

## Decisions

- 2026-10-26: The study pins `can1357/oh-my-pi` at `dcafcef6422c6a260649b879d75a5aabb1ea778e`; the repo moves daily and all citations use that commit.
- 2026-10-26: AFK: omp runs only in Docker with a dummy key and a host recording proxy that injects real auth and redacts it, because omp defaults to `yolo` approval and hostile-command experiments must find no real credential.

## Documentation

No `docs/` change; it describes shipped behaviour. The research doc, the research-list update in
the master plan, the `research-findings.md` pointer and the changelog entry are the
documentation deliverable.

## Testing Strategy

No product code, so nothing is added to `cargo test`. The scripts are research tooling, in the
tier of `research/typed-decision-screen/scripts/` and checked by running them: each experiment's
recorded output is the check. One automated check pays full price because a leak would be
silent: `scrub.py` runs over every committed capture, and before commit an `rg` for
credential-shaped patterns (for example `Bearer [A-Za-z0-9._-]{20,}`) plus `scrub.py --check`
for the literal dummy key, the current upstream tokens and omp's install-id must find nothing in
`research/oh-my-pi-harness/captures/`.

## Open Questions

- Which provider/base URL can omp be pointed at with the credentials available here? The
  expected route is the opencode-go gateway's OpenAI-compatible endpoint; if omp's provider
  config cannot take a custom base URL, E2-E8 fall back to the nearest supported path or to
  source-only, and the doc says so.
- Can the LSP and DAP integrations be exercised inside the image (language servers installable
  offline from the build)? If not, E4 records them as source-only.
