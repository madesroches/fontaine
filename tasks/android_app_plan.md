# Android App Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/710

## Overview

A native Android app that scans the QR code a Fontaine host shows (`/remote-control`, `/remote on`,
`fontaine --remote`), pairs with the host through the unchanged host-terminal confirmation, and then
drives the session: transcript, prompts, approvals, questions, interrupt. It is surface 4 of #288.
The UI is Dear ImGui on a NativeActivity with the Rust `fontaine-remote` client linked in directly
(no Kotlin or Compose UI). It ships as a sideloaded APK; there is no Play Store listing. The source
lives in this repo.

## Current State

- The device side is `fontaine_remote::Client` (`rust/fontaine-remote/src/client.rs`): it takes a
  `Target` (`Ticket` for first pairing, `StoredHost` after), `AttachOptions` and an
  `Arc<dyn ClientStore>` (`device_key`, `hosts`, `update_hosts`; `paired.rs`), and yields
  `ClientEvent`s while accepting `ClientMsg`s (`Prompt`, `ApprovalAnswer`, `QuestionReply`,
  `Interrupt`, `Slash`). Reconnect with resume-from-sequence, and snapshot reassembly, are inside it.
  It is tokio-based and takes any iroh target, so it builds for Android as is.
- `ClientEvent::Snapshot { state }` and `ClientEvent::Event { event }` carry JSON that is decoded by
  `fontaine-tui` into `Snapshot` / `RemoteEvent` / `Dialog` / `Status`
  (`rust/fontaine-tui/src/remote/event.rs`, all `pub(crate)`), whose `Entry` and `Dialog` types
  pull in `fontaine-sdk` and ratatui. The Android app cannot depend on `fontaine-tui`.
- `Ticket` (`rust/fontaine-remote/src/ticket.rs`) is `fontaine-attach1` + base32. `qr_payload()` is the
  uppercased ticket so the QR stays in the alphanumeric set. PR #709 (open) moves it to a compact
  postcard form, version 3, with id-only tickets for relayed hosts; this plan assumes #709 has merged.
  `Ticket::parse` is case-insensitive on the prefix.
- The terminal QR is drawn by `rust/fontaine-tui/src/qr.rs` (`qrcode` crate). Nothing decodes QR codes.
- `rust/Cargo.toml` has `members = ["*"]`: every directory directly under `rust/` is a workspace member and
  is built, linted and tested by CI on all three OSes. Root directories are restricted by AGENTS.md.
- `ClientStore` has a file-backed impl in `fontaine-tui` (`remote/store.rs`) and a memory impl.

## Design

### Where the code lives

A new crate `rust/fontaine-android` (a `cdylib` + `rlib`). Everything Android-specific is behind `#[cfg(target_os = "android")]` and
`cfg(target_os = "android")` dependencies, so on Linux, macOS and Windows the crate compiles to its
portable part only and its tests run in the existing host CI. The portable part holds everything that
can be tested without a phone:

- `link`: the deep-link URI parser/builder.
- `model`: the read-only mirror types the app decodes (below).
- `app`: the session state machine (screens: Scan, Pairing, Session, Error), driven by input events and
  `ClientEvent`s, producing a view model; ImGui code only renders it and forwards taps.
- `store`: `ClientStore` over a directory the activity hands in (the app's private files dir), with
  the device key kept owner-only. It is
  a small first-party file store (atomic write, 0600 on unix, a `cfg(unix)` permission assertion in its
  test), since `fontaine-tui`'s `FileStore` uses sdk helpers this crate cannot import.

The ImGui renderer, camera, IME bridge and activity glue are the thin Android-only layer
(`android` module). Build tooling is `cargo-ndk`/`cargo-apk`-style with `android-activity`; no
Gradle project. The `AndroidManifest.xml` and APK metadata sit in the crate.

### Deep link and QR payload

The QR payload becomes a URI, `fontaine://pair/<ticket>`, so the system camera can open the app and the
in-app scanner parses the same text. The app declares an intent filter for the `fontaine` scheme.

- `fontaine-remote` gains `Ticket::link()` (`fontaine://pair/<ticket>`, the ticket text in the
  current lowercase form); `qr_payload()` returns `fontaine://pair/` plus the uppercased ticket; `parse`
  accepts a bare ticket or the link. `fontaine attach` accepts both
  forms, so a link pasted from the phone works. The shown text ticket stays the bare form.
- Pairing never trusts the link: any app or page can open it. The link only carries a ticket, which is
  still a bearer secret that merely asks to pair; the host-terminal `y` is what authorizes. The app
  therefore shows "Pair with `<host short id>`?" before connecting, and discards the link text from
  memory after parsing.

### Reading the session

The app keeps its own lenient, read-only mirror types in `model` (`Snapshot`, `RemoteEvent`,
`Status`, `Dialog`, and a simplified `Entry` with the tool, user, assistant, notice and error cases it
renders; unknown entry kinds decode to a placeholder, unknown fields are ignored). The host `Entry` is an
externally tagged serde enum, so `#[serde(other)]` cannot absorb unknown kinds: the mirror `Entry`
needs a hand-written `Deserialize` (or decoding entries as `serde_json::Value` and mapping the known
kinds). Drift is caught by a contract test: a unit test inside
`rust/fontaine-tui/src/remote/` (the types are `pub(crate)`) serializes real host `Snapshot` and
`RemoteEvent` values and decodes them with `fontaine-android`'s `model` (`fontaine-android` as a
dev-dependency of `fontaine-tui`; the portable part has no cycle because it does not depend on
`fontaine-tui`).

The mirror types also include simplified `Question` and `DiffPreview` shapes. Questions are one option
per question; multi-select is answered at the host.

### Interaction

- **Scan**: NDK camera frames, luma plane, decoded by a pure-Rust QR decoder (`rqrr`-class crate,
  chosen in step 1). Permission `CAMERA`, requested at first scan through JNI. A paste field is the
  fallback when the camera is denied.
- **Pairing**: `Client::start(Target::Ticket, ..)`; the `Client` already stores the host through
  the `ClientStore` before emitting `ClientEvent::Paired`; the app moves to Session only on that event.
  Device label defaults to the Android device model. Failures map
  `Reason` to the same messages as `Reason::describe`.
- **Session**: transcript list, status line, prompt input (soft keyboard through
  `android-activity`'s input-method API), interrupt button, and the approval/question dialog with
  the choices the host offered. "Always allow" is never shown (the host refuses it anyway).
  Host-only dialogs show "the host is waiting on ...". Reconnect indicator on
  `ClientEvent::Reconnecting`.
- **Reattach**: the app lists stored hosts and connects with `Target::Host`; no re-scan.
- Slash lines: a prompt starting with `/` is sent as `Slash`, subject to the host's device list.

Nothing about authority changes: the app sends the same `ClientMsg`s as `fontaine attach`, and the host
floor, guardian, device-command list and attribution by node id apply unchanged.

### Out of scope

FCM push, PWA, LAN mode, multi-session list, iOS, Keystore-wrapped key (tracked as a follow-up),
Play Store publishing.

## Implementation Steps

1. **Spike and choices** (`rust/fontaine-android/examples` or a scratch branch, not committed as
   product code): pick the ImGui binding that supports a GLES3 NativeActivity backend, the QR decoder
   crate, and confirm `iroh` and `fontaine-remote` build for `aarch64-linux-android` with `tls-ring` (locally via
   `rustup target add aarch64-linux-android`; the `rust-toolchain.toml` edit stays in step 3).
   Record the choices in `## Decisions`.
2. **Link format** in `fontaine-remote`: `Ticket::link`, the new `qr_payload()`, link-aware `parse`;
   update `TicketError::NotATicket`'s Display (it says "expected `fontaine-attach1...`") to mention the
   link form, and its matching tests. `Grid::encode` keeps taking a `&str`. The `ticket_prompt.rs` and
   `qr.rs` tests and `qr::sample_payload` are re-measured against the longer payload (e.g. the
   `body.len()` and the 41/42-row fit boundary), as is the `crate::qr::sample_payload(639)` use in
   `fontaine-tui/src/view.rs`; `fontaine-tui/src/ui/remote.rs`
   calls `TicketPrompt::new` with `ticket.qr_payload()` and is updated to match. Update `fontaine
   attach`: `attach_cmd.rs` routes to `Ticket::parse` only when the text starts with `fontaine-`; the
   check also accepts `fontaine://pair/`.
3. **Crate skeleton**: `rust/fontaine-android` with the portable modules, `Cargo.toml` (android-only
   deps under `cfg(target_os = "android")`), `link`, `model`, `store`. Pin new deps exactly in
   `rust/Cargo.toml` `[workspace.dependencies]`, update `rust/Cargo.lock` and `rust/deny.toml` if
   needed, add the android target to `rust/rust-toolchain.toml` for the clippy step, and run
   `cargo deny check`.
4. **App state machine** (`app`) with the view model and `ClientEvent` handling, host-tested.
5. **Contract test** (unit test in `rust/fontaine-tui/src/remote/`) between `fontaine-tui` host mirror
   output and `model`.
6. **Android layer**: activity loop, GLES context, ImGui renderer, camera + QR scan, IME bridge,
   manifest with the intent filter and `CAMERA` permission.
7. **Build and release**: `.github/workflows/android.yml` builds the APK on Linux for pull requests
   that touch `rust/fontaine-android`, `rust/fontaine-remote` or the workflow (cargo-ndk, Android NDK
   from the runner image, `cargo clippy --target aarch64-linux-android`). Release signing and
   attaching to GitHub Releases is a separate step in `release.yml`, using a signing key stored as a
   repository secret; the debug-signed APK from PRs is not published.
8. **Docs and changelog** (below).

## Files to Modify

- `rust/fontaine-remote/src/ticket.rs`, `rust/fontaine-remote/src/lib.rs`
- `rust/fontaine-tui/src/qr.rs`, `rust/fontaine-tui/src/ticket_prompt.rs`,
  `rust/fontaine-tui/src/view.rs`, `rust/fontaine-tui/src/ui/remote.rs`, `rust/fontaine-tui/Cargo.toml` (dev-dependency), a new unit
  test in `rust/fontaine-tui/src/remote/`
- `rust/fontaine/src/attach_cmd.rs`
- `rust/fontaine-android/**` (new)
- `rust/Cargo.toml`, `rust/Cargo.lock`, `rust/deny.toml` (if needed), `rust/rust-toolchain.toml`
- `.github/workflows/android.yml` (new), `.github/workflows/release.yml`
- `docs/remote.md`, `docs/tui.md`, `docs/terminal.md` (check), `docs/installing.md`, `docs/SUMMARY.md`, `docs/index.md`, `docs/android.md` (new)
- `tasks/fontaine_harness_plan.md` (workspace layout, Decisions), `AGENTS.md` (crate list),
  `CHANGELOG.md`

## Decisions

- ImGui UI, sideloaded APK, source in this repo, and a custom `fontaine://` scheme were decided on #710
  by the owner.
- The crate lives under `rust/`: it avoids an AGENTS.md layout exception, and `members = ["*"]` gives
  host CI coverage of the portable part.
- App-owned lenient mirror types plus a contract test, not moving `fontaine-tui`'s types, which pull in
  the sdk and ratatui.
- QR payload: the link prefix stays lowercase and the ticket uppercase, so the scheme matches on
  Android and the code stays small.
- v1 keeps the device key in the app-private files dir (owner-only) and defers Keystore wrapping,
  because wrapping adds JNI/Keystore plumbing that the first version does not need to pair and attach.

## Documentation

- `docs/android.md`: install the APK, scan, pair, reattach, permissions, limits; linked from
  `docs/SUMMARY.md` and `docs/index.md`.
- `docs/remote.md`: the QR carries `fontaine://pair/...`, `fontaine attach` accepts it, and the
  Android app as a device.
- `docs/tui.md`: the QR terminal-size figures, re-measured for the longer payload; check
  `docs/terminal.md` for the same.
- `docs/installing.md`: sideloading, and verifying the APK signature.
- `CHANGELOG.md`, and the plan's Decisions and workspace-layout entry on landing.

## Testing Strategy

Automated, in host CI, no live services:

- `link`/`Ticket`: `link()` round trip; a link with an uppercase ticket parses; wrong scheme or path is
  `NotATicket`; `qr_payload()` parses back via `Ticket::parse`. The existing ticket tests cover the rest.
- `model`: decoding of unknown entry kinds and extra fields, with the other entries in the same
  snapshot still decoding; the contract test against real host
  `Snapshot`/`RemoteEvent` output.
- `app`: the state machine over scripted `ClientEvent`s, including mid-approval disconnect leaving no
  answer sent, "always allow" never offered, a rejected pairing reason surfaced, revoked and not-paired rejections after pairing showing the
  `Reason::describe` text, replayed link
  after a consumed secret, and reconnect.
- `store`: device key persisted owner-only and stable across reopen; hosts file round trip.
- Loopback pairing and approval through `fontaine_remote::Client` against a `RemoteServer` already
  exist in `rust/fontaine-remote/tests/loopback.rs`; the app reuses `Client` unchanged.
- Android CI: the APK builds and clippy passes on `aarch64-linux-android`.

## Manual Verification

Camera, IME, GLES rendering and real NAT/relay behavior cannot run in CI, so they are checked by hand
on a phone:

1. `fontaine --remote` on a desktop; open the app, scan the terminal QR; expected: the app shows the
   host id, the desktop shows the pairing confirmation, `y` pairs, the transcript appears.
2. Scan the same QR with the system camera; expected: Android offers to open Fontaine and lands on the
   same pairing screen.
3. Send a prompt, answer an approval from the phone, interrupt; expected: the host ledger records the
   phone's node id.
4. Toggle wifi to cellular mid-session; expected: the session resumes without a re-pair.
5. `/remote revoke` on the host; expected: the phone is closed with `revoked`.

## Open Questions

- Accessibility (TalkBack) coverage for the ImGui UI, and when it becomes a requirement.
