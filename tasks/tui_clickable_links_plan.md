# TUI: Clickable Links Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/83

## Overview
URLs, issue/PR references and file paths in the transcript are plain text today: they are
coloured (#206) but can only be opened by copying them out. This plan renders them as OSC 8
terminal hyperlinks. The display text stays exactly what the model or tool wrote, and OSC 8 only
adds a target. Terminals without OSC 8 support get the current output unchanged. Explicit
`http(s)://` URLs, markdown links, `owner/repo#N`, bare `#N` (resolved against a GitHub `origin`)
and `path:line` file references become links.

## Current State
- `rust/fontaine-tui/src/markdown.rs`: `link_spans(text, style, link)` (~line 656) is the
  hand-written bare-URL scanner (no regex), with `trim_url` for trailing punctuation. `Inline`
  styles markdown link text with the `link` style and drops the `Tag::Link` target
  (`Event::Start(Tag::Link { .. })`, ~line 610). Inline code (`Event::Code`) and fenced code
  (`code_rows`) are never linkified. `wrap_spans` wraps per char and keeps each char's `Style`, so
  a style survives wraps and hard splits.
- `rust/fontaine-tui/src/transcript.rs`: `link_style(theme)` (~1007) is fg `theme.link` +
  `UNDERLINED`. `plain_rows` (streaming tail), tool output rows (~1381) and `Entry::Shell` rows
  (~1136) run `link_spans` per wrapped row. Notices and errors are deliberately not linkified
  (test `call_rows_notices_and_errors_are_not_linkified`, ~2990).
- `rust/fontaine-tui/src/screen.rs`: settled rows reach scrollback through
  `Terminal::insert_before` + `Paragraph::render` (~530). The live area is drawn by
  `view::draw(frame, app)` (`view.rs:135`). Both go through ratatui 0.30.2 / ratatui-core 0.1.2
  buffers to `CrosstermBackend`, which writes each `Cell::symbol()` raw.
- ratatui-core 0.1.2 supports `CellDiffOption::ForcedWidth(NonZeroU16)` on a `Cell`. A symbol
  that carries escape bytes declares its real display width this way, and the buffer diff then
  neither over-skips nor miscounts. ratatui's own tests use it for OSC 8 (`merge_diff_link`).
- The `underline-color` feature is on (ratatui default) and nothing in the workspace sets
  `Style::underline_color`.
- `rust/fontaine-tui/src/linear.rs` (screen-reader renderer) writes flattened plain text.
- `fontaine_core::ledger::git_origin(root)` (`ledger.rs:834`) reads `origin`'s fetch URL through
  gix, credentials stripped, without spawning `git`. `fontaine_core::project_env::discover::github_slug(url)` (`project_env/discover.rs:209`) extracts the
  `owner/repo` of a GitHub URL. `App::set_cwd`
  (`app.rs:592`) caches per-directory git facts (`head_cache`).
- Master plan Decisions (`### TUI`, 2026-11-03, #206): links are coloured and underlined "without
  OSC 8 hyperlinks". This plan supersedes that. That notices and errors are not linkified is not a
  recorded decision, only the behaviour asserted by the test above, which the notices change here
  supersedes.

### Claude Code baseline (observed 2026-11-11, Claude Code 2.1.322, interactive, in tmux 3.6, repo with a `github.com` origin)
Raw output captured with `tmux pipe-pane` for the reply
`See https://example.com/a and #82 and owner/repo#7 and GH-82 and src/main.rs:1 and [docs](https://example.org/d) and `https://example.net/c``:
- `https://example.com/a` → `ESC]8;id=<rand>;https://example.com/a BEL` … `ESC]8;; BEL`, coloured.
- `owner/repo#7` → linked to `https://github.com/owner/repo/issues/7`.
- `[docs](https://example.org/d)` → shows `docs`, linked to the URL.
- `` `https://example.net/c` `` (inline code that is exactly a URL) → linked.
- `#82` (despite the GitHub origin), `GH-82` and `src/main.rs:1` → **not** linked.
- Each link is one open/close pair around the run, with an `id=` parameter and BEL terminator.

## Design

### 1. Tagging links through the existing row pipeline
Rows are `ratatui::text::Line<'static>`, and they cross some 190 call sites (`Transcript::lines`,
`take_retired`, `commit_within`, `live_lines`, view code, tests). A span has no metadata slot, so
the link target travels in the span's `Style::underline_color` as a **tag**:
`Some(Color::Rgb(r, g, b))` encodes a 24-bit id into a process-global interner. A post-pass over
every rendered buffer turns tagged cells into real output and clears the tag. The backend
therefore never sees a tag. Render signatures (`width, theme`) stay unchanged, and the change
stays inside one new module plus the places that create links.

New module `rust/fontaine-tui/src/hyperlink.rs`:

```rust
/// What the text refers to, before the session's context resolves it.
#[derive(Clone, PartialEq, Eq, Hash)]
pub(crate) enum Ref {
    Url(String),                                    // http(s) URL or markdown link target
    Issue { repo: Option<(String, String)>, number: u64 }, // owner/repo#N, or bare #N
    Path { path: String },                          // as written, `:line[:col]` stripped
}

/// Interns refs to ids `1..=0xFF_FFFF`; process-global so rendering needs no context.
static REFS: LazyLock<Mutex<Interner>> = ...;

pub(crate) fn tag(style: Style, r: Ref) -> Style;   // sets underline_color = Rgb(id); interner full → style unchanged
pub(crate) struct Links { enabled: bool, github: Option<(String, String)>, cwd: PathBuf,
                          resolved: Mutex<HashMap<u32, Option<String>>> }
impl Links {
    pub(crate) fn resolve(&self, id: u32) -> Option<String>;   // cached per id
    pub(crate) fn apply(&self, buf: &mut Buffer, area: Rect, link: Style);
}
pub(crate) fn supported(env: impl Fn(&str) -> Option<String>, windows: bool) -> bool;
```

- The interner and the resolution cache hold display state only: lock with `lock_recovering`,
  with a one-line reason at the call site (AGENTS.md lock rule).
- **Context-free refs** (`Url`, `Issue` with `repo: Some`) are styled with the link style at
  render time, as URLs are today, and tagged. So existing colour behaviour and tests hold, and the
  colour shows even where OSC 8 is off.
- **Context-dependent refs** (`Issue` with `repo: None`, `Path`) are only tagged at render time.
  The post-pass adds the link style (fg `theme.link`, `UNDERLINED`, patched onto the cell as
  `style.patch(link)` does) only when the ref resolves. An unresolvable `#82` or a path to no
  file stays plain text, as the issue requires.

### 2. Resolution (`Links::resolve`)
- `Url(u)` → `url::Url::parse(u)` serialized, `None` if malformed. Only `http`/`https` reach here: the scanner only finds those, and a markdown
  link target with another scheme (`javascript:`, `file:`, a relative path) is not tagged.
- `Issue { repo: Some((o, r)), n }` → `https://github.com/o/r/issues/n` (GitHub redirects issue
  URLs to PRs, so one form covers both, as Claude Code does).
- `Issue { repo: None, n }` → the same with `Links::github`, else `None`.
- `Path { path }` → join it to `cwd` unless absolute. If it names an existing regular file
  (`std::fs::metadata`, cached per id), the result is `url::Url::from_file_path(..)` (percent-
  encodes and handles Windows drive letters); otherwise `None`. The `:line` suffix is not put in the
  URL: there is no portable line syntax for `file://`.
- `Links::github` is `git_origin(cwd).and_then(|u| github_slug(&u))` split on `/`, using the
  existing `fontaine_core::project_env::discover::github_slug`.
- `App::set_cwd` refreshes `Links` (origin + cwd) when the directory changes, and clears the
  resolution cache.

### 3. The post-pass (`Links::apply`)
For each cell in `area` whose `underline_color` is `Color::Rgb(..)` with a known id:
1. Reset `underline_color` to `Color::Reset`.
2. Resolve. If `None`, leave the cell otherwise unchanged. If the ref is context-dependent and
   resolves, patch in the link style.
3. If `enabled` and resolved, rewrite the symbol to
   `ESC]8;id=<id-hex>;<target> ESC\ <symbol> ESC]8;; ESC\` and set
   `CellDiffOption::ForcedWidth(width of the original symbol)`. The shared
   `id=` makes terminals treat the run, and its wrapped continuation rows, as one link for hover
   and click. Targets are not pre-sanitised: assistant deltas and `!` shell output are stored raw
   and the scanner stops only at whitespace. `resolve` normalizes `Url` targets through
   `url::Url::parse` (percent-encodes non-ASCII, rejects malformed → `None`). `apply` still drops
   any target holding a byte outside `0x20..=0x7E` as the final guard against escape injection,
   and leaves that cell unlinked.

It is called at two places:
- `screen.rs` `draw_frame`, in the `insert_before` closure right after `Paragraph::render`
  (before the `one_row_region` HIDDEN pass, which only touches `Cell::EMPTY`).
- `view::draw`, as the last step, over `frame.area()`. This covers the live transcript tail, the
  Ctrl-O transcript view and any overlay text.

The cell's display width is unchanged: wrapping happens on text before any of this, and
`ForcedWidth` keeps ratatui's diff arithmetic identical. `linear.rs` never sees tags (it
flattens text), so screen-reader mode stays plain.

### 4. Detection (`hyperlink::supported`)
`App` gains `links: Links`, with `enabled = false` from `App::new`, so all existing tests render
as today. `lib.rs` sets it from `supported(...)` in the render closure's `Some(screen)` branch, and never in
screen-reader mode or headless runs (which have no TUI). Rules, in order:
1. `FORCE_HYPERLINK`: `0` → off, any other non-empty value → on (the `supports-hyperlinks`
   convention).
2. `TMUX` set → on. tmux 3.4+ parses OSC 8 and forwards it only to an outer terminal with the
   `hyperlinks` feature, and drops it otherwise, so nothing leaks either way.
3. Windows: on only with `WT_SESSION` (Windows Terminal) or `TERM_PROGRAM` set, regardless of
   `TERM`. Legacy conhost → off.
4. `TERM` unset, `dumb`, `linux`, or starting with `screen` (GNU screen without tmux) → off.
5. Otherwise → on. Terminals that do not implement OSC 8 ignore the OSC per ECMA-48.

### 5. What gets scanned where
A new scanner `scan(text, kinds) -> Vec<(Range<usize>, Ref)>` in `hyperlink.rs` replaces the URL
loop in `markdown::link_spans`. It keeps `trim_url` and the word-boundary rule, and adds the
`Ref` kinds. `link_spans(text, style, link, kinds)` tags (and, for context-free refs, styles) each
match. `kinds` is a small bitset: `URL`, `ISSUE`, `PATH`. `scan` takes the earliest match; when two
start at the same place it prefers URL over ISSUE over PATH, and never rescans bytes already
claimed (so `owner/repo#7` is one `Ref::Issue`, and `#82` inside a URL is not matched separately).

| Source | URL | `owner/repo#N`, `#N` | `path[:line]` |
|---|---|---|---|
| Assistant prose (markdown text, streaming tail, raw-row fallback) | yes | yes | yes |
| Markdown link `[text](url)` | target, on the text | — | — |
| Inline code whose whole content is one token | yes | no | yes |
| Fenced code block rows | whole-token only | no | no |
| Tool output, `!` shell output | yes | no | yes |
| Notices | yes | no | no |
| Errors, call rows, reasoning, user input | no | no | no |

Token rules (no regex, as for #206):
- `#N`: `#` at a word start (preceded by start or a char that is not alphanumeric, `_`, `/`, `&`
  or `#`), then 1–9 ASCII digits, then a non-word char or end. `owner/repo#N`: `owner` and `repo`
  are `[A-Za-z0-9_.-]+`, the token starts at a word start, and `#N` follows directly. `GH-N` is
  not recognised.
- Path: a whitespace-delimited token (after `trim_url`-style trimming and stripping one pair of
  surrounding quotes or backticks, or a leading `(`, `[`, `<` or `{` together with its matching
  trailing closer, before the trim), optionally ending `:line` or `:line:col`. It must contain a
  `/` or carry a `:line` suffix with a `.` in the file name. It must not contain `://`. Existence
  is checked at resolve time, not scan time.
- Fenced code: a row's whitespace-delimited token that is entirely an `http(s)` URL (after
  trimming) is tagged. Nothing else in code is.
- Markdown links: `Inline` keeps a stack of link targets. Text inside `Tag::Link` is tagged with
  `Ref::Url(dest_url)` when the destination is `http(s)`; otherwise it is only coloured, as today.
  `Event::Code` uses the top of the link-target stack when inside a link (replacing the `in_link`
  counter) and applies the whole-token URL/path rule otherwise.

Wrapped URLs keep their tag on every row in markdown rendering (per-char style). In the per-row
paths (`plain_rows`, tool output, `!` shell output, the `raw_rows` fallback), a continuation row has already lost colour since #206, and it
now also has no link. That caveat is unchanged and accepted.

### Architecture
```
markdown / transcript row builders
   └─ link_spans / Inline ── tag(style, Ref) ──► Line<'static> (underline_color = Rgb(id))
                                                    │
screen.rs insert_before ─ Paragraph::render ─┐      │
view::draw ─ widgets ────────────────────────┴─► Links::apply(buf)
                                                    ├─ resolve(id) (github origin, cwd, fs)
                                                    ├─ style context-dependent refs
                                                    └─ OSC 8 per cell + ForcedWidth (if enabled)
                                                         ▼
                                                 CrosstermBackend (writes symbols raw)
```

## Implementation Steps
1. **`hyperlink.rs`** (new, `mod hyperlink;` in `lib.rs`): `Ref`, interner + `tag`, `scan`,
   `Links` (`new`, `set_context`, `resolve`, `apply`), `supported`. Unit tests
   here (see Testing).
2. **`Cargo.toml`** (`fontaine-tui`): add `url.workspace = true` (already a workspace dependency,
   `rust/Cargo.toml:114`).
3. **`markdown.rs`**: move the URL loop into `hyperlink::scan` and give `link_spans` a `kinds`
   argument. `Inline` tags markdown link text with the target, and `Event::Code` whose whole
   content is one URL or path token gets tagged. `code_rows` tags whole-token URLs.
   `raw_rows`/`uncovered` pass prose kinds.
4. **`transcript.rs`**: assistant prose rows pass `URL|ISSUE|PATH`; the streaming-tail `plain_rows` passes `URL|ISSUE` only, since
   partial path tokens would be interned and stat'ed on every delta (settled rows go through
   markdown with full kinds). Tool
   output and `Entry::Shell` rows pass `URL|PATH`. Notices pass `URL`: call `link_spans` in the
   info-notice branch before `prefixed` (`prefixed` is shared with errors and user input and stays
   unlinkified) and in `warning_lines`/`hint_spans` for warning notices. This replaces the
   "notices are not linkified" assertion in `call_rows_notices_and_errors_are_not_linkified`
   (errors stay unlinkified).
5. **`app.rs`**: `pub(crate) links: Links` on `App`. `set_cwd` refreshes its context
   (`git_origin` + `project_env::discover::github_slug`) when the directory changes.
6. **`screen.rs`**: make `link_style` in `transcript.rs` `pub(crate)`, compute it from
   `app.theme` before the `insert_before` closure, and call `app.links.apply(.., link)` in the
   closure. The closure borrows `rows`, so take `&app.links` before the loop (the `shown_with_theme` borrows end before it).
7. **`view.rs`**: `view::draw` computes `link = link_style(&app.theme)` and ends with
   `app.links.apply(frame.buffer_mut(), area, link)`.
8. **`lib.rs`**: `App` is created in `drive()`, after the `Screen`. In the `run()` render
   closure's `Some(screen)` branch, set `app.links.enabled = hyperlink::supported(|k|
   std::env::var(k).ok(), cfg!(windows))`, mirroring `app.linear = true` in its `None` branch
   (~774). Screen-reader mode leaves it off.
9. **Docs** (see Documentation) and **master plan Decisions** (see Decisions): add the lines at
   the end of `### TUI`.

## Files to Modify
- `rust/fontaine-tui/src/hyperlink.rs` (new)
- `rust/fontaine-tui/src/lib.rs`, `app.rs`, `markdown.rs`, `transcript.rs`, `screen.rs`, `view.rs`
- `rust/fontaine-tui/Cargo.toml`
- `docs/tui.md`, `docs/terminal.md`, `CHANGELOG.md`
- `tasks/fontaine_harness_plan.md` (`### TUI` Decisions)

## Trade-offs
- **Tag in `underline_color` vs a typed row wrapper.** A `Row { line, links }` type is more type-
  safe, but it changes the return type of every transcript and view row builder and ~190 call
  sites. The tag keeps signatures intact and confines the mechanism to `hyperlink.rs` plus two
  `apply` calls. Its failure mode, a render path that skips `apply`, shows at worst a
  differently coloured underline on an already underlined link and never a wrong target, because
  ids map 1:1 to interned refs.
- **Per-cell OSC 8 vs one pair per run.** One pair per run (Claude Code's form) is smaller, but
  ratatui's diff redraws single cells, and a lone redrawn cell would lose its link. Per-cell
  wrapping with a shared `id=` behaves identically for the user and costs more bytes, only for
  link cells.
- **Resolving at the post-pass vs at render.** Resolving at render would need the session context
  in every row builder. At the post-pass, rendering stays context-free, and an origin or cwd
  change applies on the next frame.
- **Interning is bounded per distinct token.** Shell-output paths cost one cached stat each.
- **A `supports-hyperlinks` crate dependency** was rejected: it is allow-list based and turns links
  off inside tmux, the main target environment named by the issue.

## Decisions
- 2026-11-11 (#83): Transcript links are OSC 8 hyperlinks (explicit URLs, markdown link targets,
  `owner/repo#N`, bare `#N` against a `github.com` origin, existing `path[:line]` files as
  `file://` URLs without the line), emitted per cell with a shared `id=`; this supersedes the
  "without OSC 8 hyperlinks" part of the 2026-11-03 (#206) line.
- 2026-11-11 (#83): Bare `#N` and `path:line` are linked, where Claude Code 2.1.322 links neither
  (it links only URLs, markdown links and `owner/repo#N`); reason: the issue asks for them, and an
  unresolvable one stays plain text.
- 2026-11-11 (#83): `GH-N` and scheme-less `github.com/...` forms are not linked, as in Claude
  Code; reason: `GH-N` collides with ticket keys, and neither is unambiguous without a scheme.
- 2026-11-11 (#83): `#N` resolves only against an `origin` on `github.com`; other remotes and
  GitHub Enterprise hosts leave it plain.
- 2026-11-11 (#83): Issue references are linked only in assistant prose, not in tool output, code
  or notices; reason: `#N` in logs and code is rarely an issue.
- 2026-11-11 (#83): URLs in notices are linkified (coloured and linked); errors stay plain.
- 2026-11-11 (#83): Hyperlinks are auto-detected from the environment (`FORCE_HYPERLINK`, `TMUX`,
  `TERM`, Windows Terminal) with no config key; screen-reader mode and headless output never emit
  OSC 8; reason: `FORCE_HYPERLINK` already overrides detection by convention.

## Documentation
- `docs/tui.md`: around line 413 (markdown subset) and line 38 (tool output), say that links are
  clickable in terminals that support hyperlinks. List what is linked (URLs, markdown links,
  `owner/repo#N`, `#N` with a GitHub `origin`, `path:line` to an existing file) and what is not
  (code, except whole URLs and paths in inline code).
- `docs/terminal.md`: new `## Hyperlinks` section covering detection, `FORCE_HYPERLINK=0/1`, and
  under `## tmux` the setting `set -as terminal-features ",*:hyperlinks"` (tmux 3.4+). Add the
  section to the page's intro list, and cross-reference it from `## Colours` ("URLs and link text use
  an underlined blue in every theme").
- `CHANGELOG.md`: Unreleased > Added > TUI entry.

## Testing Strategy
Unit tests, no live APIs (all logic is reachable with constructed inputs):
- `hyperlink.rs`
  - `scan`: URL (existing #206 cases kept); `#82` at line start, after `(`, and not inside `a#82`,
    `&#82;`, `x/#82` or `##82`; `owner/repo#7` scans as `Ref::Issue`; `#82` inside a URL is not separately matched; `GH-82` not matched; `(src/main.rs:12)` scans as the path without the parentheses; `src/main.rs:12`,
    `src/main.rs:12:5`, `a/b`, and `` `src/x.rs` `` with quotes trimmed; `foo.rs` without `/` or
    `:line` not matched; `https://x/y` not taken as a path.
  - `resolve`: `#82` with and without a GitHub origin; `owner/repo#7` without an origin; a path
    to an existing temp file gives a `file://` URL with percent-encoding (a name with a space); a
    missing path gives `None`; a non-ASCII URL (`https://de.wikipedia.org/wiki/Müller`) resolves
    percent-encoded.
  - `supported`: each rule, including `FORCE_HYPERLINK` overriding `TERM=dumb`, `TMUX` with
    `TERM=screen-256color` on, bare `TERM=screen` off, and Windows with and without `WT_SESSION`, including `TERM` unset with `WT_SESSION` set (on).
  - `apply`: a tagged buffer row gets symbols `ESC]8;id=..;<target>ESC\<c>ESC]8;;ESC\` with
    `ForcedWidth`, and the stripped text equals the untagged row; `underline_color` is `Color::Reset` on
    every cell; disabled → no escapes, tag cleared, context-free styling kept; an unresolved `#N`
    → no link style; a target with a control byte → no link.
- **Acceptance (screen tests, `screen.rs` with `TestBackend`, `app.links.enabled = true`):** a
  reply holding a URL, a `#N` with a GitHub origin, a `#N` with no remote and a `path:line` to a
  real temp file. The scrollback cells carry the right OSC 8 target, or none for `#N` without a
  remote.
- **Wrapping width unchanged:** the same transcript rendered with links enabled and disabled
  wraps identically. Row count and the per-row text, with OSC 8 stripped from each symbol, are
  equal. A URL hard-split across rows carries the same `id=` on both rows.
- **Live diff:** two frames of `view::draw` where one cell inside a link changes. The diff entry
  for that cell alone still carries the OSC 8 target (the per-cell rationale).
- `markdown.rs`: markdown link text tagged with its target; inline code that is exactly a URL is
  tagged; a fenced code row `curl https://a` tags `https://a` (a whole token) but `x=https://a`
  tags nothing, and `#82` or a path in a fenced row is not tagged; `#82` in inline code is not
  tagged; a markdown target with a `javascript:` scheme is never tagged.
- `transcript.rs`: notice URL tagged and coloured; error still not; tool output path tagged,
  `#N` in tool output not.

## Manual Verification
The byte stream through a real terminal and tmux is the one thing `TestBackend` cannot show (and
AGENTS.md asks for tmux reproduction of terminal output issues):
1. `tmux -L t83 new -d -x 100 -y 30 'cargo run -p fontaine'` inside a GitHub-origin checkout,
   then `tmux -L t83 pipe-pane -o 'cat >> /tmp/t83.bin'`. Ask for a reply containing a URL, `#83`
   and `rust/Cargo.toml:1`. Expected: `/tmp/t83.bin` contains `ESC]8;id=` with
   `https://github.com/madesroches/fontaine/issues/83` and a `file://…/rust/Cargo.toml` target,
   and `tmux capture-pane -p` shows the text with no stray characters.
2. In a terminal with OSC 8 support (outside tmux), Ctrl/Cmd-click a URL; it opens. Clicking is
   manual because it is the terminal's own behaviour.

## Open Questions
None.
