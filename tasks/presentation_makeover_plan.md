# Presentation Makeover Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/530

## Overview

Rework how Fontaine presents itself so people can find it and understand it quickly. The root
`README.md` becomes a short front door that pitches the two original ideas, auto mode and AFK mode,
and routes readers to the reference docs. `docs/` is published as a searchable website on GitHub
Pages, built with mdBook, with `llms.txt` / `llms-full.txt` for coding agents. The repo metadata
(description, topics, homepage, social preview) and the awesome-list listings are maintainer
settings and post-release outreach; this branch prepares them but does not change them.

## Current State

- `README.md` is 157 lines. Lines 15–64 are install caveats already covered by
  `docs/installing.md` (receipt paths, sha256, proxies, SmartScreen, `--force`, `pg2`, crates.io).
  Lines 66–77 ("Build from source") repeat `CONTRIBUTING.md`. The mode table (lines 108–118) omits
  `afk` and repeats `docs/permissions.md#modes`. The auto-mode section (lines 120–137) repeats
  `docs/guardian.md`. The quickstart runs `cargo run -p fontaine -- -p ...` from `rust/`. The
  "Documentation" list (lines 139–150) is a stale subset of `docs/index.md`, and AFK mode is not
  mentioned anywhere in the README.
- `docs/` holds 25 topic pages plus `docs/schemas/headless-v1.schema.json`. `docs/index.md` is
  the hand-written landing page that lists every page with a description. There is no site
  tooling. `tasks/design/documentation.md` says "no site tooling ships in v1" and defines the
  README as "pitch, install, quickstart, mode table, auto-mode explainer".
- About 35 links in `docs/` leave the tree (`../evals/README.md`, `../evals/scorecards/*.json`,
  `../rust/fontaine/keys/*.pub`, `../research/*.md`, `../SECURITY.md`, `../CONTRIBUTING.md`,
  `../CHANGELOG.md`). They work on GitHub but would break on a site that serves only `docs/`.
- Release archives ship the README (`rust/fontaine/Cargo.toml:16`,
  `include = ["../../LICENSE", "../../README.md", "../../CHANGELOG.md"]`), so it must still read
  well offline.
- CI: `.github/workflows/ci.yml` (Rust matrix), `dev.yml` (dev channel, gated by the repository
  variable `FONTAINE_DEV_CHANNEL == 'enabled'` so `main` stays green before the environment
  exists), `release.yml`. Helper scripts live in `.github/scripts/`. Actions are referenced by
  major tag (`actions/checkout@v4`), and tools are installed exact-pinned through
  `taiki-e/install-action@v2`.
- Repo: description "rust cli harness", no topics, no homepage, Pages and Discussions off.

## Design

### README (target 50–70 lines)

The README has these sections, in this order:

1. `# Fontaine`, followed by a one-line tagline: "A terminal AI coding agent for any model, with
   a guardian that judges risky tool calls, and an AFK mode that runs unattended without YOLO."
2. Badges: CI (`ci.yml`), latest release (shields.io GitHub release badge, including
   prereleases), license.
3. A pitch of two short paragraphs:
   - **Auto mode.** A separate guardian model reviews each tool call that is not provably safe,
     with a deterministic floor of deny and ask rules, critical paths and protected paths beneath
     every mode. It fails closed.
   - **AFK mode.** Not YOLO, and not an `auto` session that stalls waiting for you. It decides
     exactly as `auto` does, but every would-prompt outcome becomes a denial with recovery
     guidance. When you return, a report shows everything the agents tried, and a session
     temperature stops the agent tree when denials pile up.
4. `## Install`: the two one-line installers (shell and PowerShell), plus one sentence linking
   [Installing Fontaine](docs/installing.md) for archives, source builds, verification, proxies
   and updating.
5. `## Quickstart`: the Ollama `config.toml` snippet, then `fontaine` (terminal UI) and
   `fontaine -p "..."` (headless) run as the installed binary, not with `cargo run`. One sentence
   links [Configuration](docs/configuration.md) for other providers.
6. `## Why Fontaine`: 5–6 bullets, each one sentence linking its page:
   - [AFK mode](docs/afk.md)
   - [the guardian and auto mode](docs/guardian.md), with the [scorecard](docs/evals.md)
   - [permission modes and the floor](docs/permissions.md), with one clause naming the six modes
   - [subagents and task boards](docs/agents.md)
   - Claude Code compatibility: `CLAUDE.md`/`AGENTS.md`, `.claude/` skills, plugins, hooks, MCP
     ([instructions](docs/instructions.md), [extensions](docs/extensions.md),
     [plugins](docs/plugins.md))
   - any provider: Anthropic, OpenAI, Gemini, OpenRouter, Ollama and OpenAI-compatible endpoints,
     plus a Claude subscription ([configuration](docs/configuration.md))
7. `## Documentation`: one link to the docs site, with [docs/index.md](docs/index.md) as the
   in-repo fallback.
8. `## Project`: contributing, security, changelog, license (as today), and a single line noting
   the project is under construction.

Links stay relative (`docs/x.md`), because GitHub is the primary place the README is read and
relative links follow the branch being viewed. The docs-site link is absolute. In an extracted
archive no link resolves either way, so the archive README depends on its prose and the one
absolute site URL.

Every fact removed from the README must already be in its target page. The research for this plan
confirmed that each installer caveat is in `docs/installing.md`, the Ollama `context_tokens`
default is in `docs/configuration.md`, and the mode table (with `afk`) is in
`docs/permissions.md#modes`. "Build from source" moves to `CONTRIBUTING.md`, which already has
the commands; it gains one sentence about `rust/rust-toolchain.toml` and `rustup`. The step that
deletes README text re-checks each removed sentence against its target page.

### Docs site (mdBook on GitHub Pages)

```
docs/                          (the book root and source: src = ".")
├── book.toml                  new: title, src, site-url, repo links, search
├── SUMMARY.md                 new: mdBook navigation (ordered as docs/index.md)
├── index.md                   landing page → index.html
├── *.md                       topic pages, unchanged apart from out-of-tree links
└── schemas/*.json             copied through as static files
.github/scripts/docs-site.sh   new: check + build + raw .md copy + llms.txt / llms-full.txt
.github/workflows/docs.yml     new: build+link-check on PRs, deploy to Pages on main
```

- **Tool**: mdBook `0.5.4` (released 2026-07-06), installed through
  `taiki-e/install-action@v2` with `tool: mdbook@0.5.4,lychee@<exact>`. The implementer pins the
  newest lychee release that is at least two weeks old and that the action supports.
- **`docs/book.toml`**:
  - `[book] title = "Fontaine"`, `src = "."`, with a description used as the meta description.
  - `[output.html]`: `site-url = "/fontaine/"`,
    `git-repository-url = "https://github.com/madesroches/fontaine"`,
    `edit-url-template = "https://github.com/madesroches/fontaine/edit/main/docs/{path}"`, and
    search enabled (the default).
  - `[build] create-missing = false`, so a SUMMARY entry without a file fails the build.

  The book lives in `docs/` because the root-directory list in AGENTS.md is closed, and
  `book.toml` is configuration for the docs tree.
- **`docs/SUMMARY.md`**: an untitled "Introduction" link to `index.md`, then every page, grouped as
  in `docs/index.md`. SUMMARY.md holds navigation only; descriptions stay in `index.md`, so the
  two lists do not duplicate content. `docs-site.sh` checks that they agree.
- **Out-of-tree links**: rewrite every `](../...)` link in `docs/` to an absolute
  `https://github.com/madesroches/fontaine/blob/main/...` URL (`tree/main/...` for directories).
  Rewriting the source keeps the build free of link-rewriting magic and the links work in both
  places. `docs-site.sh` fails if any `](../` link remains.
- **`.github/scripts/docs-site.sh <out-dir>`** (bash, `set -euo pipefail`, Linux CI only):
  1. Coverage check: every `docs/*.md` other than `SUMMARY.md` appears in `SUMMARY.md`, every
     SUMMARY page except `index.md` is linked from `index.md`, and no `](../` link appears in
     `docs/`. Each failure names the file.
  2. `mdbook build docs -d <out-dir>`.
  3. Copy each SUMMARY page's raw Markdown to `<out-dir>/<page>.md`, so agents can fetch clean
     Markdown.
  4. Write `<out-dir>/llms.txt` following the llmstxt.org format: `# Fontaine`, a blockquote
     summary, then a `## Docs` list of `[Title](https://madesroches.github.io/fontaine/<page>.md)`,
     in SUMMARY order, with titles read from SUMMARY. Write `<out-dir>/llms-full.txt` as the
     SUMMARY-ordered concatenation of the raw pages.

  The site base URL is one variable at the top of the script, used in no other place.
- **`.github/workflows/docs.yml`**:
  - Triggers: `pull_request` and `push` to `main`, both filtered to `docs/**`,
    `.github/scripts/docs-site.sh` and `.github/workflows/docs.yml`, plus `workflow_dispatch`.
    `permissions: contents: read` at the top.
  - Job `build` (ubuntu-latest): checkout, install mdbook and lychee, run `docs-site.sh _site`,
    run `lychee --offline --include-fragments --no-progress '_site/**/*.html'` to check internal
    links and anchors (offline, so CI never depends on external hosts), then upload with
    `actions/upload-pages-artifact@v3` (path `_site`).
  - Job `deploy`: `needs: build`, runs only on a push to `main` and only when
    `vars.FONTAINE_DOCS_SITE == 'enabled'` (the same pattern as `dev.yml`, so `main` stays green
    until Pages is switched on). It uses `environment: github-pages`,
    `permissions: pages: write, id-token: write`, `actions/deploy-pages@v4`, and
    `concurrency: { group: pages, cancel-in-progress: false }`.
- **Site URL**: `https://madesroches.github.io/fontaine/`.

### Maintainer steps (not automated by this branch)

The PR body lists these as exact commands:

- Pages: Settings → Pages → Source "GitHub Actions" (or
  `gh api -X POST repos/madesroches/fontaine/pages -f build_type=workflow`), then
  `gh variable set FONTAINE_DOCS_SITE --body enabled` and re-run the `Docs` workflow.
- `gh repo edit madesroches/fontaine --description "<the issue's description>" --homepage https://madesroches.github.io/fontaine/ --add-topic ...`
  with the topics from the issue plus `autonomous-agents`.

A follow-up issue (filed by the implementer) covers the work that needs tools, assets or a release
this branch cannot produce: the `vhs` terminal recording of auto and AFK mode, the 1280×640 social
preview image (web-UI upload only), enabling Discussions, the awesome-list PRs and the launch post
after 0.1.0.

## Implementation Steps

1. **Docs links**: rewrite the out-of-tree `](../...)` links in `docs/*.md` to absolute GitHub
   URLs (`blob/main` for files, `tree/main` for directories such as `evals/scorecards/`).
2. **Book config**: add `docs/book.toml` and `docs/SUMMARY.md`.
3. **Build script**: add `.github/scripts/docs-site.sh`. Run it locally (install mdBook 0.5.4 with
   `cargo install mdbook --version 0.5.4 --locked`, or the release binary) and fix anything the
   build or lychee reports (broken anchors, pages missing from SUMMARY).
4. **Workflow**: add `.github/workflows/docs.yml`.
5. **README**: rewrite `README.md` to the outline above. Move the toolchain sentence into
   `CONTRIBUTING.md` "Build and test", and add a short "Docs" paragraph there: preview with
   `mdbook serve docs`, check with `.github/scripts/docs-site.sh <dir>`.
6. **Design and plan records**:
   - `tasks/design/documentation.md`: replace "no site tooling ships in v1" with the mdBook/Pages
     site and `llms.txt`, and replace the README contents line with the new outline (pitch with
     auto and AFK mode, install, quickstart with the installed binary, why-list, links).
   - `tasks/fontaine_harness_plan.md`: add the `docs.yml` workflow and `docs-site.sh` to the
     workspace-layout paragraph (around lines 203–206), and add two dated lines at the end of the
     `### Project` Decisions subsection (see Decisions).
   - `AGENTS.md` Essential Commands: one line for `.github/scripts/docs-site.sh <dir>` (Linux;
     needs mdbook and lychee).
7. **Follow-up issue**: file it with `gh issue create` (labels `documentation`, `enhancement`),
   covering the recording, social preview, Discussions, listings and launch post. Link it in the
   PR body together with the maintainer commands.

## Files to Modify

- `README.md` (rewrite)
- `CONTRIBUTING.md`
- `AGENTS.md`
- `docs/book.toml` (new), `docs/SUMMARY.md` (new)
- `docs/*.md`: out-of-tree links only (`architecture.md`, `evals.md`, `guardian.md`,
  `verifying-releases.md`, `index.md` and any others the grep finds)
- `.github/scripts/docs-site.sh` (new)
- `.github/workflows/docs.yml` (new)
- `tasks/design/documentation.md`
- `tasks/fontaine_harness_plan.md`

## Trade-offs

- **mdBook vs Starlight vs Zensical.** mdBook is Rust-native and maintained by the Rust project,
  renders the existing Markdown tree unchanged, and has built-in search; it needs no Node or
  Python toolchain. Starlight has better navigation and SEO but brings Node into CI. Zensical is
  pre-1.0.
- **Rewriting out-of-tree links in the source vs at build time.** A build-time rewrite (a
  preprocessor or sed over a staging copy) keeps GitHub links branch-relative but adds a moving
  part that is easy to break. Absolute links are simple and work everywhere; their cost is that
  links on a branch view point at `main`.
- **Hand-written SUMMARY.md vs generating it from index.md.** Generating it would need parsing
  rules for index.md's prose. A checked pair is simpler and keeps index.md free to stay a
  descriptive landing page.
- **Relative README links vs site links.** Relative links work today, on every branch. Site links
  would be dead until the maintainer enables Pages.
- **Gating deploy on a repository variable vs deploying unconditionally.** Deploying would fail
  every `main` push until Pages is enabled. The variable follows the `dev.yml` precedent.

## Decisions

- 2026-11-08 (#530), master plan `### Project`: `docs/` is published as an mdBook site on GitHub
  Pages with `llms.txt`/`llms-full.txt`, built and link-checked in CI. This supersedes "no site
  tooling ships in v1" (`design/documentation.md`), because a site is what search engines and
  agents index.
- 2026-11-08 (#530), master plan `### Project`: the README is a short front door (pitch with auto
  and AFK mode, installers, quickstart with the installed binary, why-list, links). Install
  caveats, the mode table and build-from-source live only in `docs/` and `CONTRIBUTING.md`.
- 2026-11-08 (#530): repository settings (description, topics, homepage, Pages source, the
  `FONTAINE_DOCS_SITE` variable) are left to the maintainer, and the PR body lists the exact
  commands. Recording, social preview, Discussions and listings go to a follow-up issue.

## Documentation

This change is itself documentation. It touches `README.md`, `CONTRIBUTING.md` and `AGENTS.md`,
adds `docs/book.toml` and `docs/SUMMARY.md`, rewrites links in `docs/*.md`, and updates
`tasks/design/documentation.md`. `CHANGELOG.md` gains an Unreleased entry when the PR is opened.

## Testing Strategy

No Rust code changes. The checks are:

- `docs-site.sh` runs on every PR that touches `docs/`. It fails on a page missing from
  SUMMARY.md or index.md, a dangling SUMMARY entry (`create-missing = false`), or a remaining
  `](../` link.
- `lychee --offline --include-fragments` over the built HTML catches every broken internal link
  and anchor. This is the check that would otherwise fail silently as docs evolve.
- The implementer runs the script and lychee locally before committing (step 3), and also makes
  sure the existing CI still passes: Rust jobs are unaffected, and `README.md` is still included
  by `dist plan`.

## Manual Verification

1. Run `.github/scripts/docs-site.sh /tmp/site && python3 -m http.server -d /tmp/site 8000`, open
   `http://localhost:8000/`, and check that the landing page, sidebar order, search (try "AFK"),
   the `afk.html` anchors and the schema JSON link work. Rendering and navigation feel are not
   machine-checkable.
2. Open `/tmp/site/llms.txt` and check that every page is listed with a working `.md` URL.
3. View the rewritten `README.md` rendered on the PR branch on GitHub and check that the badges
   render and the relative links resolve.
