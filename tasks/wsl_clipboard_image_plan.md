# WSL Clipboard Image Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/586

## Overview

On WSL2 (WSLg) a screenshot copied on the Windows side never reaches Fontaine: Ctrl+V
reports "no image on the clipboard" because WSLg bridges only *text* clipboard content to
the Linux side, so the `wl-paste`/`xclip` probes find no `image/png` target even though
`powershell.exe` can read the image from the Windows clipboard. On native Windows (Fontaine
under cmd.exe or PowerShell, no WSL) Ctrl+V image attach fails too: rig's probe table is
empty there, so `read_image` errors "no clipboard here". This plan re-implements the
clipboard-image read in `fontaine-tui` with per-probe diagnostics, a native-Windows probe,
and a WSL fallback, both reading the Windows clipboard through `powershell.exe
Get-Clipboard -Format Image`, and makes the "no image" notice name every probe it tried and
what each one found.

## Current State

- Ctrl+V maps to `TuiAction::AttachClipboard` (`rust/fontaine-core/src/keybindings.rs:228`,
  default binding `rust/fontaine-tui/src/keymap.rs:46`) and lands as `Action::AttachClipboard`,
  handled by `Ui::attach_clipboard` (`rust/fontaine-tui/src/ui/mod.rs:934-986`).
- `attach_clipboard` calls `rig_tools::clipboard::read_image(&launcher, &scratch)` off the
  executor's threads (`ui/mod.rs:948-951`). The `Launcher` carries the session's
  `scrub_env` so clipboard readers never inherit credential variables (`ui/mod.rs:946-947`).
- rig's read side (`rig-tools 0.46.0`, `src/clipboard.rs`) probes, in env order, Wayland
  (`wl-paste --type image/png`), X11 (`xclip -selection clipboard -target image/png -out`),
  and macOS (`pngpaste -`). The first program that exits 0 wins; its stdout is sniffed with
  `ImageMediaType::sniff` and saved under `into` as `pasted-<millis>.<ext>`. No program
  succeeding means `Ok(None)`, which `attach_clipboard` renders as the one-line notice
  "no image on the clipboard" (`ui/mod.rs:986`). When no program applies at all (headless
  Linux; native Windows) rig returns `Err(Unsupported, "no clipboard here: it needs wl-paste
  and wl-copy, xclip, or pbpaste and pbcopy")` instead.
- On WSL2/WSLg, `WAYLAND_DISPLAY` and `DISPLAY` are both set, so both `wl-paste` and
  `xclip` are probed; `xclip` exits 0 for text targets but the clipboard exposes no
  `image/png` target, so the probe either fails or yields bytes that do not sniff as an
  image. Either way the user gets "no image on the clipboard" with no hint that the Windows
  side holds a screenshot. `wl-paste` is typically not installed.
- On native Windows no rig probe applies, so Ctrl+V always hits the "no clipboard here"
  error even with an image on the clipboard.
- `rust/fontaine-tui/src/clipboard.rs` today only *writes* text (tmux `load-buffer` + OSC
  52); the read side lives entirely in rig.
- `docs/tui.md:367-379` documents Ctrl+V as reading "the clipboard image with the desktop's
  own tool (`wl-paste`, `xclip` or `pngpaste`)" and says a missing image is "a one-line
  notice, nothing more".
- The Ctrl+V key event itself is a plain crossterm key event, not bracketed paste
  (`rust/fontaine-tui/src/app.rs:3359-3365` tests the mapping directly), so the failure is
  in the read path, not the input path.

## Design

Move the clipboard-image read from rig into `rust/fontaine-tui/src/clipboard.rs`, as a
small probe table plus a Windows-clipboard probe (native Windows and WSL), returning
per-probe results so the notice can be diagnosable. `attach_clipboard` keeps its current shape (blocking work on
`spawn_blocking`, same scrubbed `Launcher`, same sniff-and-chip logic, same scratch
directory, same `MAX_ATTACHMENT_BYTES` cap); only the call and the no-image notice change.

### Types

```rust
/// How one clipboard probe ended.
pub(crate) enum ProbeOutcome { Missing, Failed, NoImage, Image }

/// One program tried against the clipboard.
pub(crate) struct Probe { program: &'static str, detail: &'static str, outcome: ProbeOutcome }

/// The result of reading the clipboard image.
pub(crate) struct ImageRead {
    /// Where the image was saved (sniffed type, `pasted-<millis>.<ext>`), if any.
    pub path: Option<PathBuf>,
    /// Every probe tried, in order.
    pub probes: Vec<Probe>,
}
```

`clipboard::read_image(launcher: &Launcher, into: &Path) -> io::Result<ImageRead>` is the
entry point; internally it delegates to `read_image_with(launcher, into, env)` where `env`
is a `&dyn Fn(&str) -> Option<OsString>` lookup, so tests inject the environment instead of
mutating the process's (tests run in parallel; `std::env::set_var` is process-global and
racy). The production call site passes `std::env::var_os`.

### Probe table

The probe list mirrors rig's env-ordered selection, recomputed per read:

1. `WAYLAND_DISPLAY` set → `wl-paste --no-newline --type image/png` ("Wayland clipboard").
2. `DISPLAY` set → `xclip -selection clipboard -target image/png -out` ("X11 clipboard").
3. macOS → `pngpaste -`, chosen by `cfg!(target_os = "macos")` (not injectable through
   `env`).
4. Native Windows → `powershell.exe` ("Windows clipboard"), chosen by
   `cfg!(target_os = "windows")`, same non-injectable pattern as the macOS row.
5. WSL (see below) → `powershell.exe` ("Windows clipboard through WSL interop").

An empty probe table (headless Linux, not WSL) keeps today's `Err(Unsupported, "no clipboard
here: it needs wl-paste and wl-copy, xclip, or pbpaste and pbcopy")` with that message
unchanged; `Ok(ImageRead { path: None, .. })` is only for a non-empty table.

Each probe runs with the scrubbed `Launcher`, `stdin`/`stderr` nulled. The program is
spawned directly through the child-process API, never through a parent shell, so launching
Fontaine from cmd.exe or PowerShell makes no difference. Each probe pipes stdout, drains it
on a reader thread into a `Vec<u8>`, polls `try_wait` until the deadline, and kills and
reaps the child on timeout (`Failed`). rig's `Process::output(Some(timeout))` is unsuitable
because it decodes stdout as lossy UTF-8, which corrupts the PNG bytes from
`wl-paste`/`xclip`/`pngpaste`. `attach_clipboard` is awaited inline by the UI loops
(`ui/mod.rs:781`, `ui/turn.rs:433`/`1159`), so the timeout bounds how long the UI can
freeze: all probes of one read share a single deadline of 15 seconds (powershell.exe
startup on a cold WSL interop can take a few seconds), a probe starting after it has passed
is `Failed` without being spawned. A probe that cannot be spawned is `Missing`; one that
exits non-zero or times out is `Failed`; one that exits 0 but whose output does not sniff
as an image is `NoImage`; otherwise its bytes are sniffed, saved under `into` as
`pasted-<millis>.<ext>` (same naming rig uses, including `create_dir_all(into)`), and the
probe is `Image`. Probes stop at the first `Image`. When no probe finds an image the result
is `Ok(ImageRead { path: None, probes })` — not an error; "nothing on the clipboard" is a
normal answer, matching today's `Ok(None)` semantics.

### WSL detection

`on_wsl(env, osrelease)` is a pure function: WSL when `WSL_INTEROP` or `WSL_DISTRO_NAME`
is present in `env`, or the os-release string contains `microsoft`/`WSL`
(case-insensitive). The production `read_image` reads `/proc/sys/kernel/osrelease` (a
`NotFound` read on non-Linux yields an empty string). The WSL row is skipped when the
native-Windows row applies. Environment variables are checked
from the passed-in `env` lookup, not the child's, so the detection itself is unaffected by
the scrub. This matches WSL1 and WSL2, native Linux, and WSL sessions where interop is
disabled (env vars may be absent there, but os-release still says microsoft; powershell.exe
then probes as `Missing` and shows up in the notice — which is the diagnosability the issue
asks for).

### The Windows-clipboard probe

Used on native Windows and under WSL. The executable is `powershell.exe` resolved by PATH
lookup (System32 is on PATH on native Windows; WSL interop provides it under WSL), `Missing`
otherwise. fontaine's existing `fontaine_tools::powershell` discovery prefers `pwsh.exe` and
is deliberately NOT reused here. Command:

`powershell.exe -NoProfile -NonInteractive -Command <script>` where the script is:

```powershell
Add-Type -AssemblyName System.Drawing
$image = Get-Clipboard -Format Image
if ($null -eq $image) { exit 1 }
$stream = New-Object System.IO.MemoryStream
$image.Save($stream, [System.Drawing.Imaging.ImageFormat]::Png)
[Console]::Out.Write([Convert]::ToBase64String($stream.ToArray()))
```

Rationale for the pieces:

- `powershell.exe` (Windows PowerShell 5.1), never `pwsh`: `Get-Clipboard` needs an STA
  apartment and `powershell.exe` defaults to STA; pwsh defaults to MTA and breaks
  `Get-Clipboard -Format Image`.
- Base64 over stdout: Fontaine trims the output and decodes it with
  `base64::engine::general_purpose::STANDARD.decode`; a decode failure is `NoImage`.
- Output is sniffed like any other probe's bytes, so a surprise non-PNG answer degrades to
  `NoImage` instead of attaching garbage.
- Timeout per the probe table; `powershell.exe` that hangs (interop disabled mid-call) is
  `Failed` and named in the notice.

### Notice

A function in `clipboard.rs` over `ImageRead` builds the notice text; `attach_clipboard`
calls it and renders `ImageRead { path: None, probes }` as a single line:

```
no image on the clipboard (tried wl-paste: not installed, xclip: no image, powershell.exe: failed)
```

Every outcome has a fixed label: `not installed` for `Missing`, `failed` for `Failed`
(including timeout), `no image` for `NoImage`. Every probe tried appears with its label. On
native Windows the notice names `powershell.exe` and its outcome. Error arms (`Err(e)` from the read, spawn
failure of `spawn_blocking`) keep today's messages.

## Implementation Steps

1. `rust/fontaine-tui/src/clipboard.rs`: add the read side — `ProbeOutcome`, `Probe`,
   `ImageRead` (all `pub(crate)`), `on_wsl`, `read_image_with`, `read_image`, the notice
   formatting function, the byte-safe probe runner (piped stdout, reader thread, shared
   deadline, kill and reap on timeout), the native-Windows row and the powershell probe
   (command construction as a separate `fn powershell_probe_command() -> (&'static str,
   Vec<&str>)`-shaped helper so it is unit-testable). Reuse `ImageMediaType::sniff` for the
   type detection and keep rig's `pasted-<millis>.<ext>` naming.
2. `rust/fontaine-tui/src/ui/mod.rs:948-951`: switch `attach_clipboard` from
   `rig_tools::clipboard::read_image` to `crate::clipboard::read_image`; map the new result
   onto the existing chip logic; replace the `"no image on the clipboard"` arm with the
   probe-naming notice. The viewing-agent guard, scratch guard, and `spawn_blocking` stay.
3. Add `base64.workspace = true` to `rust/fontaine-tui/Cargo.toml`.
4. `docs/tui.md` "Attaching files": document the Windows behavior — Ctrl+V also tries the
   Windows clipboard through `powershell.exe` on native Windows and under WSL, and a failed
   attach names the probes tried and what each found.
5. Unit tests (in `clipboard.rs`'s `#[cfg(test)]` module):
   - probe-table selection for each env combination (Wayland only, X11 only, both, neither,
     WSL on/off) via the injected `env` lookup; the macOS row only under its own
   `#[cfg(target_os = "macos")]` test and the native-Windows row only under
   `#[cfg(windows)]`, since neither is injectable; an empty table returns the
   `Unsupported` error;
   - `on_wsl` truth table over env vars and os-release strings;
   - powershell script construction: program name, `-NoProfile -NonInteractive`, STA-safe
     command, exit-1-on-empty path, base64 output;
   - base64 decode → sniff → save writes `pasted-*.png` whose bytes round-trip; a base64
     decode failure and surrounding whitespace (trimmed);
   - notice formatting for the no-image cases (all `Missing`, all `NoImage`, a `Failed`
     mixed in);
   - the probe runner keeps non-UTF-8 stdout bytes intact and kills a hung child at the
     deadline.
6. End-to-end regression test (`#[cfg(all(test, target_os = "linux"))]` in the same module):
   build a temp dir holding fake `wl-paste` (exit 1), `xclip` (exit 1), and
   `powershell.exe` (a shell script printing the base64 of a known 1×1 PNG), point the
   *child's* PATH at it with `Launcher::with_env("PATH", …)`, inject
   `WAYLAND_DISPLAY`/`DISPLAY`/`WSL_INTEROP` via the `env` parameter, and assert
   `read_image_with` returns the image saved as PNG with the three probes recorded. This
   pins the exact failure from the issue — WSLg exposing no image target to Linux while the
   Windows clipboard holds one — without a live WSL. Gate to Linux: on Windows a script named
   `powershell.exe` cannot execute, and on macOS the extra `pngpaste` row would record a
   fourth probe.
7. `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test -p
   fontaine-tui` from `rust/`.

## Files to Modify

- `rust/fontaine-tui/src/clipboard.rs` — read side with probes, native-Windows probe, WSL
  fallback, notice formatting, tests.
- `rust/fontaine-tui/src/ui/mod.rs` — `attach_clipboard` call site and notice.
- `rust/fontaine-tui/Cargo.toml` — `base64.workspace = true`.
- `docs/tui.md` — Windows and WSL clipboard behavior and diagnosable notice.

`rig_tools::clipboard::read_image` stops being called; the rig dependency itself stays
(`Launcher` and other tools remain in use).

## Decisions

- 2026-11-11: re-implement the probe table in fontaine-tui instead of wrapping rig's
  `read_image`, because rig's API cannot report per-probe diagnostics.
- 2026-11-11: base64 over stdout instead of a PNG file, to avoid WSL path translation and
  binary-through-interop hazards.
- 2026-11-11: keep the read local rather than upstreaming a diagnostics API into rig, since
  this is the only `read_image` call site.
- 2026-11-11: the bug is missing on native Windows too, so the fix covers a native-Windows
  probe, not only the WSL fallback.

## Documentation

- `docs/tui.md` "Attaching files" — the Ctrl+V bullet gains the Windows/WSL `powershell.exe`
  probe and the probe-naming notice.
- `CHANGELOG.md` — bug-fix entry under the next release (the `pr` skill writes it).

## Testing Strategy

Unit tests cover the probe table, WSL detection, the powershell command, base64
decode-sniff-save, the byte-safe probe runner, and notice formatting — all pure or
file-local, no live APIs. The
end-to-end test with fake clipboard programs on the child's PATH reproduces the issue's
exact scenario (Linux probes find no image; the interop probe does) deterministically and
is the regression pin; the manual step covers real WSLg interop latency and a real Windows
clipboard. The fake-executable e2e test stays Linux-gated: on Windows CI a fake
`powershell.exe` cannot stand in for the real interpreter (a script named .exe won't
execute), so native Windows coverage is the manual step plus the pure-function unit tests.
Nothing here touches live providers, matching the
CI rule.

## Manual Verification

1. On the WSL2 machine from the issue, copy a screenshot on the Windows side, run
   `cargo run -p fontaine` in WSL, press Ctrl+V, and confirm the chip appears and a vision
   model receives the image. Then clear the clipboard and press Ctrl+V again: the notice
   must name wl-paste, xclip, and powershell.exe with their outcomes. Not automated: needs
   a live WSLg session and a real Windows clipboard, outside any test backend.
2. On native Windows, copy a screenshot, run `cargo run -p fontaine` in cmd.exe, press
   Ctrl+V, and confirm the chip appears. Then clear the clipboard and press Ctrl+V again:
   the notice must name `powershell.exe` and its outcome.

## Open Questions

- How Claude Code reads the clipboard on WSL could not be settled from this machine: the
  installed `claude` is a native binary (PE) whose bundle is not inspectable, and public
  material is inconclusive. The `powershell.exe Get-Clipboard -Format Image` fallback
  follows the issue's own suggested direction, which is also the widely used approach in
  other WSL clipboard tooling; if Claude Code is later found to use a different mechanism,
  matching it is a follow-up.
