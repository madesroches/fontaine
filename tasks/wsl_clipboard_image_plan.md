# WSL Clipboard Image Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/586

## Overview

On WSL2 (WSLg) a screenshot copied on the Windows side never reaches Fontaine: Ctrl+V
reports "no image on the clipboard" because WSLg bridges only *text* clipboard content to
the Linux side, so the `wl-paste`/`xclip` probes find no `image/png` target even though
`powershell.exe` can read the image from the Windows clipboard. This plan re-implements the
clipboard-image read in `fontaine-tui` with per-probe diagnostics and a WSL fallback that
reads the Windows clipboard through `powershell.exe Get-Clipboard -Format Image`, and makes
the "no image" notice name every probe it tried and what each one found.

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
  "no image on the clipboard" (`ui/mod.rs:971`).
- On WSL2/WSLg, `WAYLAND_DISPLAY` and `DISPLAY` are both set, so both `wl-paste` and
  `xclip` are probed; `xclip` exits 0 for text targets but the clipboard exposes no
  `image/png` target, so the probe either fails or yields bytes that do not sniff as an
  image. Either way the user gets "no image on the clipboard" with no hint that the Windows
  side holds a screenshot. `wl-paste` is typically not installed.
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
small probe table plus a WSL fallback, returning per-probe results so the notice can be
diagnosable. `attach_clipboard` keeps its current shape (blocking work on
`spawn_blocking`, same scrubbed `Launcher`, same sniff-and-chip logic, same scratch
directory, same `MAX_ATTACHMENT_BYTES` cap); only the call and the no-image notice change.

### Types

```rust
/// How one clipboard probe ended.
enum ProbeOutcome { Missing, Failed, NoImage, Image }

/// One program tried against the clipboard.
struct Probe { program: &'static str, detail: &'static str, outcome: ProbeOutcome }

/// The result of reading the clipboard image.
pub struct ImageRead {
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
3. macOS → `pngpaste -`.
4. WSL (see below) → `powershell.exe` ("Windows clipboard through WSL interop").

Each probe runs with the scrubbed `Launcher`, `stdin`/`stderr` nulled, and a timeout of 10
seconds (powershell.exe startup on a cold WSL interop can take a few seconds; a hung
interop must not freeze the UI). A probe that cannot be spawned is `Missing`; one that
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
`NotFound` read on non-Linux yields an empty string). Environment variables are checked
from the passed-in `env` lookup, not the child's, so the detection itself is unaffected by
the scrub. This matches WSL1 and WSL2, native Linux, and WSL sessions where interop is
disabled (env vars may be absent there, but os-release still says microsoft; powershell.exe
then probes as `Missing` and shows up in the notice — which is the diagnosability the issue
asks for).

### The Windows-clipboard probe

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
- Base64 over stdout instead of saving to a file: WSL path translation
  (`wslpath -w`, `\\wsl$` UNC) is avoided entirely, and base64 sidesteps the encoding
  hazards of piping binary through the WSL interop console. Fontaine decodes with
  `base64::decode` (new tiny dependency; `base64` is already in the tree via rig — check
  `rust/Cargo.toml` and reuse the same crate/version the workspace already resolves).
- Output is sniffed like any other probe's bytes, so a surprise non-PNG answer degrades to
  `NoImage` instead of attaching garbage.
- Timeout per the probe table; `powershell.exe` that hangs (interop disabled mid-call) is
  `Failed` and named in the notice.

### Notice

`attach_clipboard` renders `ImageRead { path: None, probes }` as a single line:

```
no image on the clipboard (tried wl-paste: no image, xclip: no image, powershell.exe: failed)
```

`Missing` renders as `wl-paste: not installed` when it is the only thing wrong, otherwise
the bare program name. The exact wording is a formatting detail; the requirement is that
every probe tried appears with its outcome. Error arms (`Err(e)` from the read, spawn
failure of `spawn_blocking`) keep today's messages.

## Implementation Steps

1. `rust/fontaine-tui/src/clipboard.rs`: add the read side — `ProbeOutcome`, `Probe`,
   `ImageRead`, `on_wsl`, `read_image_with`, `read_image`, and the powershell probe
   (command construction as a separate `fn powershell_probe_command() -> (&'static str,
   Vec<&str>)`-shaped helper so it is unit-testable). Reuse `ImageMediaType::sniff` for the
   type detection and keep rig's `pasted-<millis>.<ext>` naming.
2. `rust/fontaine-tui/src/ui/mod.rs:948-951`: switch `attach_clipboard` from
   `rig_tools::clipboard::read_image` to `crate::clipboard::read_image`; map the new result
   onto the existing chip logic; replace the `"no image on the clipboard"` arm with the
   probe-naming notice. The viewing-agent guard, scratch guard, and `spawn_blocking` stay.
3. Add `base64` to `rust/fontaine-tui/Cargo.toml` if the workspace does not already give
   fontaine-tui a base64 decoder; match the version the lockfile already resolves.
4. `docs/tui.md` "Attaching files": document the WSL behavior — Ctrl+V also tries the
   Windows clipboard through `powershell.exe` when running under WSL, and a failed attach
   names the probes tried and what each found.
5. Unit tests (in `clipboard.rs`'s `#[cfg(test)]` module):
   - probe-table selection for each env combination (Wayland only, X11 only, both, neither,
     macOS cfg, WSL on/off) via the injected `env` lookup;
   - `on_wsl` truth table over env vars and os-release strings;
   - powershell script construction: program name, `-NoProfile -NonInteractive`, STA-safe
     command, exit-1-on-empty path, base64 output;
   - base64 decode → sniff → save writes `pasted-*.png` whose bytes round-trip;
   - notice formatting for the no-image cases (all `Missing`, all `NoImage`, a `Failed`
     mixed in).
6. End-to-end regression test (unix-only, `#[cfg(all(test, unix))]` in the same module):
   build a temp dir holding fake `wl-paste` (exit 1), `xclip` (exit 1), and
   `powershell.exe` (a shell script printing the base64 of a known 1×1 PNG), point the
   *child's* PATH at it with `Launcher::with_env("PATH", …)`, inject
   `WAYLAND_DISPLAY`/`DISPLAY`/`WSL_INTEROP` via the `env` parameter, and assert
   `read_image_with` returns the image saved as PNG with the three probes recorded. This
   pins the exact failure from the issue — WSLg exposing no image target to Linux while the
   Windows clipboard holds one — without a live WSL. Gate to unix because a script standing
   in for `powershell.exe` cannot execute on Windows.
7. `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test -p
   fontaine-tui` from `rust/`.

## Files to Modify

- `rust/fontaine-tui/src/clipboard.rs` — read side with probes, WSL fallback, tests.
- `rust/fontaine-tui/src/ui/mod.rs` — `attach_clipboard` call site and notice.
- `rust/fontaine-tui/Cargo.toml` — base64 dependency if not already available.
- `docs/tui.md` — WSL clipboard behavior and diagnosable notice.

`rig_tools::clipboard::read_image` stops being called; the rig dependency itself stays
(`Launcher` and other tools remain in use).

## Trade-offs

- **Re-implementing rig's probe table in fontaine-tui instead of wrapping rig's
  `read_image`.** rig's API cannot report per-probe outcomes ("which of them are missing"
  is the issue's own diagnosability ask), and a wrapper could only guess at what rig tried.
  The duplicated logic is ~30 lines of env-ordered program selection; rig is version-pinned,
  so drift is visible at upgrade time. The alternative — patching rig or forking the read
  — breaks the pinned-dependency rule and lifts rig into this repo.
- **base64 through stdout instead of `powershell.exe` writing a PNG file.** File-based
  transfer needs WSL path translation (either `wslpath -w` round-trips or `\\wsl$` UNC,
  both of which have interop edge cases and quoting hazards through two command lines).
  Base64 costs ~33% size on a transient transfer of at most a few MB and is robust.
- **A single `read_image` in fontaine-tui rather than upstreaming a diagnostics-bearing API
  into rig.** Upstreaming would serve other harnesses but this is the only read_image call
  site, and the WSL fallback is Fontaine-specific policy. Revisit if a second consumer
  appears.

## Decisions

## Documentation

- `docs/tui.md` "Attaching files" — the Ctrl+V bullet gains the WSL `powershell.exe`
  fallback and the probe-naming notice.
- `CHANGELOG.md` — bug-fix entry under the next release (the `pr` skill writes it).

## Testing Strategy

Unit tests cover the probe table, WSL detection, the powershell command, base64
decode-sniff-save, and notice formatting — all pure or file-local, no live APIs. The
end-to-end test with fake clipboard programs on the child's PATH reproduces the issue's
exact scenario (Linux probes find no image; the interop probe does) deterministically and
is the regression pin; a unit test against a fake `powershell.exe` catches what a live-WSL
check could not only in one respect — real WSLg interop latency and a real Windows
clipboard, which the manual step covers. Nothing here touches live providers, matching the
CI rule.

## Manual Verification

1. On the WSL2 machine from the issue, copy a screenshot on the Windows side, run
   `cargo run -p fontaine` in WSL, press Ctrl+V, and confirm the chip appears and a vision
   model receives the image. Then clear the clipboard and press Ctrl+V again: the notice
   must name wl-paste, xclip, and powershell.exe with their outcomes. Not automated: needs
   a live WSLg session and a real Windows clipboard, outside any test backend.

## Open Questions

- How Claude Code reads the clipboard on WSL could not be settled from this machine: the
  installed `claude` is a native binary (PE) whose bundle is not inspectable, and public
  material is inconclusive. The `powershell.exe Get-Clipboard -Format Image` fallback
  follows the issue's own suggested direction, which is also the widely used approach in
  other WSL clipboard tooling; if Claude Code is later found to use a different mechanism,
  matching it is a follow-up.
