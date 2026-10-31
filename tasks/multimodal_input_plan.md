# Multimodal input plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/21

## Overview

`read` of an image or PDF returns the file to the model as a rig-core image/document
content part when the current model accepts that modality (rig's catalog `Modalities`
flag), and a text fallback otherwise. The TUI gains clipboard-image attach, so a pasted
or dragged screenshot reaches a vision-capable model the same way typed text does.
Workspace containment and the guardian pipeline are unchanged — resolution and gating
happen exactly where they do today.

## Current State

- Fontaine does not implement `read` itself: `rust/fontaine-tools/src/builtins.rs:680-697`
  constructs rig-tools' `Read` (`rig_tools::Read`) per root — confined via `base` +
  `confine`, with an unconfined external fallback — and `builtins.rs:859-866` registers it
  as `NotebookRead` (`rust/fontaine-tools/src/notebook.rs`), which only special-cases
  `.ipynb` cell rendering. `Read`'s `Output` is `String`; it has no MIME detection.
- rig 0.45.0 (pinned) already ships everything multimodal:
  `rig_core::message::UserContent::{image_base64, document_base64}` with
  `DocumentSourceKind::{Base64, Url, FileId, Raw}`; `ImageMediaType::sniff` (magic-byte
  detection); `DocumentMediaType::PDF`; `ToolResultContent::{Text, Json, Image}` and
  `ToolOutput::content(Vec<ToolResultContent>)` as the multimodal opt-in;
  `rig_core::catalog::Modalities { text, image, audio, video, pdf }` on
  `ModelSpec` (`DynModel::spec()`, `Option<&ModelSpec>`).
  `rust/fontaine-core/src/agent.rs:875-879` already reads `spec().context_window`.
- The user prompt is plain text end to end: `TurnParams { prompt: String }`
  (`rust/fontaine-core/src/turn.rs:83-98`), built into a message at `turn.rs:365`
  (`Message::user(params.prompt)`). Callers: `rust/fontaine-tui/src/ui/turn.rs:614-653`,
  `rust/fontaine/src/headless.rs:504-517`, and core tests.
- Tools are gated per call by `GuardedTool` (`Output = ToolOutput`,
  `rust/fontaine-tools/src/guarded.rs:334`); `read` is `RiskClass::ReadOnly` with the
  read-only fast path at `rust/fontaine-core/src/permissions.rs:790-794`. Nothing in the
  permission layer inspects file content type.
- The result pipeline (`rust/fontaine-tools/src/result_pipeline.rs`) passes every result
  through notifications, LSP diagnostics, instruction loading, the PromptGuard scanner
  (`rust/fontaine-core/src/scanner_stage.rs`), and `post_tool_use` hooks. Its
  `append_note` already handles multi-part content correctly, but
  `scanner_stage.rs:86-91` derives its scan text via `ToolOutput::render()`, which for
  non-text content falls back to serializing the parts as JSON — that would put raw
  base64 in front of the scanner, notifications and hooks.
- The TUI uses crossterm 0.29 (via ratatui) with bracketed paste enabled
  (`rust/fontaine-tui/src/lib.rs:486,543`); `Event::Paste` lands as `Input::Paste(String)`
  (`app.rs:44-47,646-666`) — text only, terminals deliver no image bytes through it.
  Clipboard support is write-only today (`rust/fontaine-tui/src/clipboard.rs`, OSC 52).
  rig-tools has the read side unused: `rig_tools::clipboard::read_image(launcher, into)`
  (wl-paste / xclip / pngpaste). There is no attachment concept; `@path` is completion
  only and the message goes out as typed (`tasks/completed/tui_input_modes_plan.md:75-92`).
- `rust/fontaine-core/src/export.rs:297` already renders non-text user content as
  `_[attachment omitted]_`; rig-memory persists messages as serde JSON, so base64 parts
  round-trip through the conversation log and checkpoints without a format change.
- MCP: fontaine-mcp is a client only; fontaine's own `read` is not exposed over MCP. No
  change.

## Design

### 1. Multimodal read (`fontaine-tools`)

Rename `NotebookRead` to `ReadView` (it now presents more than notebooks) and change its
`Tool::Output` from `String` to `ToolOutput`:

1. Resolve the path exactly as rig's `Read` does — via `rig_tools::fs::place` with the
   same `base`/`confine` fields — so confinement errors are byte-identical to today's.
2. Sniff the first bytes: `ImageMediaType::sniff` → image; `%PDF-` → PDF. Anything else,
   including `.ipynb`, delegates to the inner rig `Read` and wraps the string in
   `ToolOutput::text` (notebook cell rendering unchanged).
3. Read the file's bytes with a 5 MiB cap (rig-tools' attach constant). Over the cap → a
   text result saying the file is too large, naming the limit (no partial sends).
4. Modalities come from the call's `ToolContext`, not construction time, so a mid-session
   model switch takes effect on the next turn: fontaine-core defines
   `InputModalities(Modalities)` as a `ContextValue`, `drive_once` inserts it per turn,
   and the tool defaults to text-only when absent.
5. Supported: `ToolOutput::content([Text(header), part])` — the header names the file,
   kind, and size (e.g. `[image: PNG · 231 KB] /abs/path`), so every text-only consumer
   sees a description without touching the part; then `Image` (media type from sniff,
   `DocumentSourceKind::base64`) or `Document` (`DocumentMediaType::PDF`).
   Unsupported: a single text part explaining the file is an image/PDF, its size, and
   that the current model does not accept that input.

### 2. Prompts with attachments (`fontaine-core`)

- `TurnParams` gains `attachments: Vec<Attachment>` and `modalities: Modalities`.
  `Attachment { label: String, bytes: Vec<u8>, media: ImageMediaType }` carries an inline
  clipboard image (sniffed at attach time). File *paths* stay plain text — the agent
  reads them through `read`, which now sees the image. Headless runs pass none.
- `drive_once` builds `Message::User { content }` directly: a `Text` part for the prompt,
  then `UserContent::image_base64` per attachment when `modalities.image`, else the
  attachments collapse into a text note appended to the prompt (labels + sizes). This is
  the single gating point; the TUI warns at attach time but never decides alone.
- `InputModalities` is inserted into the `ToolContext` next to `AgentId` (`turn.rs:351`).
- Ledger: `Record::UserMessage` gains `attachments: Vec<String>` (labels) with
  `#[serde(default)]` so existing ledgers load unchanged.
- `tool_text` (`turn.rs:99-108`) renders non-text result parts as `[image]` / `[pdf]`
  placeholders (today: `[image]`) so transcripts, notifications and the audit trail never
  carry base64.

### 3. Result-pipeline text derivation

Add `text_view(&ToolOutput) -> String` in `fontaine-tools::result_pipeline`: `Text` parts
verbatim, `Json` parts serialized, `Image`/`Document` parts as `[image: png]` /
`[document: pdf]` placeholders — never base64. Switch `scanner_stage::text_of`, the
instructions stage, the notifications stage, and user hooks to it. The scanner thus
screens the text of a multimodal result and the image bytes are not scanned: they come
from the user's own clipboard or from files that already passed the gate (see Decisions).

### 4. TUI attach

Mockup A (attachment chips) is the chosen UX:

- `Ctrl+V` reads the clipboard image via `rig_tools::clipboard::read_image` into the
  session scratch directory (its designated place), sniffs the media type, and appends a
  `PendingAttachment { label, path, media_type, size }` to the input buffer. No image on
  the clipboard → a one-line notice, nothing else. Terminals that turn Ctrl+V into a
  bracketed paste never deliver the key event; the paste path already handled their text.
- The composer renders a removable chip row above the typed text; the input view's height
  accounts for it. `Backspace` on an empty input pops the last attachment (no mouse).
- On send, chips become `Attachment`s in `TurnParams`; the transcript shows
  `[image attached: <label> · <size>]` under the user's line.
- If the current model has `spec().input.image == false`, the statusbar shows
  "model does not accept images" on attach; the turn-level fallback (§2) is the backstop.

### 5. Persistence, export, context gauge

- No conversation-log or checkpoint format change (serde JSON already holds the parts);
  a cassette test pins the round-trip.
- Export keeps rendering non-text user content as omitted; the marker gains the count
  (`_[2 attachments omitted]_`).
- Known gap, accepted: the context-usage gauge counts text tokens; provider-reported
  image tokens are not subtracted (see Decisions).

## Mockups

- `tasks/multimodal_input_mockups/option-a-attachment-chips.html` — chips row above the
  typed line; removable, with kind/size; chosen. Matches Claude Code's mental model,
  scales to several attachments; costs a chips row and height math in the input view.
- `tasks/multimodal_input_mockups/option-b-inline-token.html` — attachment as a dim
  inline token in the text buffer; smallest change, but no thumbnail, awkward wrapping
  for long names, and removal-by-Backspace fights text editing.

## Implementation Steps

1. `fontaine-tools`: rename `NotebookRead` → `ReadView`; `Output = ToolOutput`; sniff +
   cap + parts per §1; `text_view` helper in `result_pipeline.rs`.
2. `fontaine-core`: `Attachment`, `InputModalities`, `TurnParams` fields; `drive_once`
   message build + ToolContext insert; `Record::UserMessage.attachments`; `tool_text`
   markers; switch scanner/instructions/notifications/hooks to `text_view`.
   `TurnParams` is a plain struct built with struct literals, so this step must also
   update every struct-literal site in `rust/fontaine-core/tests/` (16 sites across
   billing ×6, audit_telemetry ×2, and one each in output_styles, turn_recovery,
   rewind, memory, prefix_stability, agent_resume, native_search, agent_pipeline) to
   pass empty attachments/default modalities.
3. `fontaine-tui`: `PendingAttachment` on the input buffer; chip row in `view.rs`
   (`draw_input`, height calc); `Ctrl+V` handler (`app.rs`); send path in `ui/turn.rs`;
   transcript marker.
4. `fontaine` (headless): pass empty attachments; adjust `TurnParams` call sites.
5. Docs: `docs/tools.md` (read behavior with images/PDFs), `docs/tui.md` (attach key),
   parity row in `research/research-claude-code-parity.md`.

## Files to Modify

- `rust/fontaine-tools/src/notebook.rs` (→ `ReadView`), `builtins.rs`, `result_pipeline.rs`
- `rust/fontaine-core/src/turn.rs`, `scanner_stage.rs`, `instructions/on_demand.rs`,
  notifications/user-hook stage call sites, `ledger.rs`, `export.rs`
- `rust/fontaine-core/tests/`: `billing.rs`, `audit_telemetry.rs`, `output_styles.rs`,
  `turn_recovery.rs`, `rewind.rs`, `memory.rs`, `prefix_stability.rs`,
  `agent_resume.rs`, `native_search.rs`, `agent_pipeline.rs` (struct-literal
  `TurnParams` sites from step 2)
- `rust/fontaine-tui/src/input.rs`, `view.rs`, `app.rs`, `clipboard.rs`, `ui/turn.rs`
- `rust/fontaine/src/headless.rs`
- `docs/tools.md`, `docs/tui.md`, `research/research-claude-code-parity.md`

## Trade-offs

- **Base64 parts vs. provider file IDs.** Anthropic's Files API (and dsh's
  grid-resized uploads, issue #21 comment) are out of reach through some gateways and add
  upload lifecycle; base64 works on every provider rig supports and matches Claude Code's
  read behavior. Chosen: base64, 5 MiB cap, no resizing (no new image dependency).
- **Modalities via ToolContext vs. tool construction.** Tools are built once per session
  at `builtins.rs:680-697`; a `/model` switch would leave a construction-time flag stale.
  ToolContext is minted per turn at `turn.rs:351`, so it is always current.
- **One wrapper vs. a second image-only wrapper.** Extending `NotebookRead` keeps one
  dispatch point and one descriptor row; the rename pays for the broader role.

## Decisions

- 2026-10-27: Image/PDF parts travel as base64; provider file-ID uploads and image
  resizing are out of scope for this plan.
- 2026-10-27: The PromptGuard scanner and every text-deriving result stage see text parts
  only; image/PDF bytes are not scanned. Trust boundary: clipboard contents are user
  input, and files already passed the permission gate before `read` opened them.
- 2026-10-27: No PDF text extraction — on a model without PDF support the read falls
  back to a descriptive text stub.
- 2026-10-27: The context-usage gauge does not account image tokens; noted as an accepted
  gap rather than blocking the feature.

## Documentation

- `docs/tools.md`: `read` behavior for images/PDFs, the size cap, and the text fallback.
- `docs/tui.md`: the Ctrl+V attach key, chip rendering, removal, model-capability warning.
- `research/research-claude-code-parity.md`: close the "Read of images and PDFs; image
  paste" backlog row (~line 323).

## Testing Strategy

- Unit (`fontaine-tools`): sniff dispatch (PNG/JPEG/GIF/WebP magic, `%PDF-`, text,
  empty); the 5 MiB cap; confined resolution — an image outside the root is denied with
  the same error text as today's `Read`; modalities present → parts, absent → fallback
  text; `.ipynb` rendering unchanged.
- Unit (`fontaine-core`): `text_view` placeholders and the no-base64 invariant;
  `drive_once` message build for the four combinations (attachments × modality);
  `tool_text` markers; old ledgers without `attachments` deserialize.
- Unit (`fontaine-tui`, `TestBackend`): chip row and input height; Backspace-on-empty
  pops an attachment; transcript marker line.
- Integration (cassette, CI): a headless run reading a fixture PNG and a fixture PDF
  replays a recorded cassette whose request JSON carries the base64 part — this catches
  wiring regressions (part dropped between tool and request) that unit tests cannot see
  because they never build a provider request; plus the text-fallback run against a
  no-image fixture model.

## Manual Verification

1. In a real tmux with a clipboard image (wl-paste, xclip, or pngpaste available), run
   `cargo run -p fontaine`, press Ctrl+V, and confirm the chip appears and a vision model
   receives the image. Not automated: a live clipboard and terminal paste semantics are
   outside any test backend.
2. `cargo run -p fontaine -- -p "read <fixture>.png and describe it"` against a live
   vision model; expect a description, not a base64 dump. Not automated: needs a live
   provider key (the eval harness's tier, not CI's).

## Open Questions

- None blocking. The Ctrl+V binding assumes the terminal passes the key event through;
  if review finds a common terminal where it collides with bracketed paste, the fallback
  is a different chord (decided at implementation, recorded here).
