# Android Scan Camera Preview Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/728

## Overview

The Android scan screen's card says "Scanning" over an empty box, so the user cannot see what the
camera sees or aim it at the host's QR code. This plan draws the live camera image inside the scan
card. The pairing flow, `App` state machine and QR decoding do not change.

## Current State

- `rust/fontaine-android/src/android/camera.rs`: the back camera streams `YUV_420_888` frames
  (1280x720) into an `AImageReader`. `Camera::poll` acquires the latest image, copies the luma plane
  row by row (dropping stride padding) and hands it to `decode_luma`; the image is deleted right
  away. The module doc states "Nothing is shown or stored".
- `mod.rs` `Activity::scan` calls `poll` at most every `SCAN_INTERVAL` (150 ms), so frames are only
  touched ~7 times a second.
- `ui.rs` `scan_card` (`:432`) draws a bordered child window of `side` x `side`; when
  `frame.camera_ready` it centres a pulsing "Scanning" label and a hint inside it. No texture code
  exists anywhere: `Gl` loads only `glClearColor` and `glClear` via `egl.get_proc_address`
  (`mod.rs:149`), and `Imgui` uses the GLES3 backend shim.
- `docs/android.md:43` says "The card shows no camera image."; `docs/android.md:41` describes the card.

## Design

Show the luma plane the decoder already receives as a grayscale preview. This reuses the existing
reader and needs no new camera output, no JNI and no external-OES shader (ImGui cannot draw
`GL_TEXTURE_EXTERNAL_OES` textures). A QR code needs no colour to be aimed.

### Portable layout (host-tested): `rust/fontaine-android/src/layout.rs`

- `fit_cover(src: (u32, u32), dst_side: f32, rotation: u16) -> [(f32, f32); 4]`: the UV corners that
  fill a square card with the rotated, centre-cropped frame (cover, not letterbox, so the card
  stays full). `rotation` is the sensor orientation in {0, 90, 180, 270}; anything else is treated
  as 0.
- Unit tests: each rotation, a landscape source into a square, a square source, a bad rotation.

### Camera (`camera.rs`)

- Read `ACAMERA_SENSOR_ORIENTATION` for the chosen camera in `back_camera_id` (the same metadata
  entry pattern as `ACAMERA_LENS_FACING`), store it as `rotation` on `Camera`, and expose it.
- Split `poll` into `latest_luma(&mut self) -> Option<Luma>` (acquire, copy rows, delete image;
  `Luma { width, height, data }`) and keep decoding in the caller through the existing
  `decode_luma`. The frame is acquired every UI frame instead of every 150 ms; decoding stays
  throttled by `SCAN_INTERVAL` and runs on the newest `Luma` held.
- Update the module doc: the newest frame is shown on the scan card and kept in memory only until
  the next one replaces it; nothing is stored.

### GL texture (`mod.rs`)

- `Gl` loads `glGenTextures`, `glBindTexture`, `glTexImage2D`, `glTexSubImage2D`,
  `glTexParameteri`, `glPixelStorei`, `glDeleteTextures` the same way as the two it has.
- New `Preview` owned by `Surface`, which declares `preview` before `gl` so the texture is deleted
  while the GL context is still current; `Preview` uses the function pointers loaded on `Gl`. A single `GL_R8`
  texture, (re)allocated with `glTexImage2D` when the frame size changes and updated with
  `glTexSubImage2D` otherwise, `GL_LINEAR` filtering, `GL_CLAMP_TO_EDGE`, `GL_UNPACK_ALIGNMENT` 1.
  Sampling `GL_R8` gives `(r, 0, 0)`, so the texture swizzles to `(R, R, R, 1)` via
  `GL_TEXTURE_SWIZZLE_*` (GLES 3.0 core).
- `Activity` uploads the newest `Luma` in `frame()` before `render`, only while the scan screen has a
  live camera; leaving the scan screen or losing the camera drops the texture. `FrameParams` gains
  `preview: Option<PreviewView { texture: u32, uv: [(f32, f32); 4] }>`, and `ui::Frame` gains the same.

### UI (`ui.rs`)

- `scan_card`, when `frame.preview` is `Some`, draws the image into the card's inner rect with
  `ImDrawList_AddImageQuad` (a square quad with the four corners and UVs from `fit_cover`), then overlays the
  existing "Scanning" label and hint over a translucent dark strip at the bottom so they stay
  legible on a bright image. With `None` (no camera, permission off, or no frame yet) the card is
  as today.
- `dear-imgui-sys` 0.22: `ImDrawList_AddImageQuad(self_, tex_ref: ImTextureRef_c, p1..p4: ImVec2_c,
  uv1..uv4: ImVec2_c, col: ImU32)`. Build `ImTextureRef_c { _TexData: null_mut(), _TexID:
  texture_name as u64, .. }` (`ImTextureID = ImU64`; the struct has a `Default` impl) and pass `col`
  as opaque white.

## Implementation Steps

1. `layout.rs`: `fit_cover` with tests.
2. `camera.rs`: sensor orientation, `Luma`, `latest_luma`; `Activity::scan` decodes from it.
3. `mod.rs`: GL function loading, `Preview` texture, upload in `frame()`, plumb `PreviewView`.
4. `ui.rs`: draw the image and the legibility strip in `scan_card`.
5. `docs/android.md` and `CHANGELOG.md`.

## Files to Modify

- `rust/fontaine-android/src/layout.rs`
- `rust/fontaine-android/src/android/camera.rs`
- `rust/fontaine-android/src/android/mod.rs`
- `rust/fontaine-android/src/android/ui.rs`
- `docs/android.md`
- `CHANGELOG.md`

## Trade-offs

- **Luma texture (chosen)** vs. a `SurfaceTexture`/`ASurfaceTexture` camera output: the latter gives
  colour at display rate but needs an OES shader outside ImGui's draw path, a second capture target
  and API 28+. Grayscale is enough to aim at a QR code and keeps the one reader.
- **Upload per UI frame** vs. per decode (150 ms): per decode would look like ~7 fps video.

## Decisions

- Preview is grayscale and in-memory only; nothing is written to disk.

## Documentation

- `docs/android.md`: replace "The card shows no camera image." (line 43) with a description of the
  live grayscale preview and that frames are not stored, and update the card description (line 41)
  to mention the live preview behind the "Scanning" label.
- `CHANGELOG.md`: an Android entry.

## Testing Strategy

- Unit tests for `fit_cover` (host-run; covers the rotation and crop maths, the part that can be
  silently wrong).
- The NDK clippy job (`cargo clippy --target aarch64-linux-android`) compiles `mod.rs`, `ui.rs` and
  `camera.rs`.
- `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test` from `rust/`.

## Manual Verification

On a phone with the dev APK (`rust/fontaine-android/build-apk.sh`): open the app on the scan screen,
grant the camera, and confirm the card shows the live, upright image filling the square with the
"Scanning" label legible; rotate the phone and confirm it stays upright; scan a host's QR code and
confirm pairing still starts; deny the camera and confirm the card shows the "Camera access is off"
state. Orientation and GL output cannot be reached by a host test.
