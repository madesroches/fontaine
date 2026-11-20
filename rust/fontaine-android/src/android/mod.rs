//! The activity: a NativeActivity loop that owns the EGL surface, Dear ImGui, the camera and the
//! network client, and feeds the portable `App`. It holds no session logic; each frame it moves
//! input into the app, client events into the app, and the app's view onto the screen.

mod camera;
mod error;
mod jni_bridge;
mod ui;

use std::ffi::c_void;
use std::sync::Arc;
use std::time::{Duration, Instant};

use android_activity::input::{
    InputEvent, KeyAction, KeyMapChar, Keycode, MotionAction, TextInputAction, TextInputState,
};
use android_activity::{AndroidApp, InputStatus, MainEvent, PollEvent, Rect};
use dear_imgui_sys as sys;
use fontaine_remote::{AttachOptions, Client, ClientStore, RelayMode, Target};
use khronos_egl as egl;
use sha2::{Digest, Sha256};

use crate::app::{Action, App, Screen};
use crate::keys::{self, Key};
use crate::layout::{self, Insets};
use crate::link::is_pair_link;
use crate::scan::decode_luma;
use crate::store::DirStore;
use camera::{Camera, Luma};
use error::{Context, Error, chain};
use jni_bridge::RawInsets;
use ui::{PreviewView, Tap, UiState};

/// How often a camera frame is decoded: a QR decode costs more than a frame's budget.
const SCAN_INTERVAL: Duration = Duration::from_millis(150);

/// How often the window insets are asked of Java between the events that change them.
const INSETS_INTERVAL: Duration = Duration::from_millis(500);

/// Android touch slop in dp.
const TOUCH_SLOP_DP: f32 = 8.0;

type EglInstance = egl::DynamicInstance<egl::EGL1_4>;

const GL_TEXTURE_2D: u32 = 0x0DE1;
const GL_UNSIGNED_BYTE: u32 = 0x1401;
const GL_RED: u32 = 0x1903;
const GL_R8: i32 = 0x8229;
const GL_ONE: i32 = 1;
const GL_LINEAR: i32 = 0x2601;
const GL_CLAMP_TO_EDGE: i32 = 0x812F;
const GL_TEXTURE_MAG_FILTER: u32 = 0x2800;
const GL_TEXTURE_MIN_FILTER: u32 = 0x2801;
const GL_TEXTURE_WRAP_S: u32 = 0x2802;
const GL_TEXTURE_WRAP_T: u32 = 0x2803;
const GL_UNPACK_ALIGNMENT: u32 = 0x0CF5;
const GL_TEXTURE_SWIZZLE_R: u32 = 0x8E42;
const GL_TEXTURE_SWIZZLE_G: u32 = 0x8E43;
const GL_TEXTURE_SWIZZLE_B: u32 = 0x8E44;
const GL_TEXTURE_SWIZZLE_A: u32 = 0x8E45;

/// The GLES entry points the app calls itself; ImGui's backend loads its own.
#[derive(Clone, Copy)]
struct Procs {
    clear_color: unsafe extern "system" fn(f32, f32, f32, f32),
    clear: unsafe extern "system" fn(u32),
    gen_textures: unsafe extern "system" fn(i32, *mut u32),
    bind_texture: unsafe extern "system" fn(u32, u32),
    tex_image_2d: unsafe extern "system" fn(u32, i32, i32, i32, i32, i32, u32, u32, *const c_void),
    tex_sub_image_2d:
        unsafe extern "system" fn(u32, i32, i32, i32, i32, i32, u32, u32, *const c_void),
    tex_parameteri: unsafe extern "system" fn(u32, u32, i32),
    pixel_storei: unsafe extern "system" fn(u32, i32),
    delete_textures: unsafe extern "system" fn(i32, *const u32),
}

type Made = (egl::Context, egl::Surface, Procs);

struct Gl {
    egl: EglInstance,
    display: egl::Display,
    surface: egl::Surface,
    context: egl::Context,
    procs: Procs,
}

impl Gl {
    fn new(window: &ndk::native_window::NativeWindow) -> Result<Gl, Error> {
        // SAFETY: loading the system's EGL library; its symbols are used only through `egl`.
        let egl = unsafe { EglInstance::load_required_from_filename("libEGL.so") }
            .context("cannot load EGL")?;
        // SAFETY: the default display needs no native handle.
        let display = unsafe { egl.get_display(egl::DEFAULT_DISPLAY) }
            .ok_or_else(|| Error::new("EGL has no display"))?;
        egl.initialize(display).context("cannot initialize EGL")?;
        match Self::with_display(&egl, display, window) {
            Ok((context, surface, procs)) => Ok(Gl {
                egl,
                display,
                surface,
                context,
                procs,
            }),
            Err(e) => {
                let _ = egl.terminate(display);
                Err(e)
            }
        }
    }

    /// Everything after `initialize`; the caller terminates the display when this fails, so each
    /// path here releases only what it made itself.
    fn with_display(
        egl: &EglInstance,
        display: egl::Display,
        window: &ndk::native_window::NativeWindow,
    ) -> Result<Made, Error> {
        let config = egl
            .choose_first_config(
                display,
                &[
                    egl::RENDERABLE_TYPE,
                    egl::OPENGL_ES3_BIT,
                    egl::SURFACE_TYPE,
                    egl::WINDOW_BIT,
                    egl::RED_SIZE,
                    8,
                    egl::GREEN_SIZE,
                    8,
                    egl::BLUE_SIZE,
                    8,
                    egl::NONE,
                ],
            )
            .context("cannot choose an EGL config")?
            .ok_or_else(|| Error::new("no EGL config supports GLES 3"))?;
        egl.bind_api(egl::OPENGL_ES_API)
            .context("cannot bind the GLES API")?;
        let context = egl
            .create_context(
                display,
                config,
                None,
                &[egl::CONTEXT_CLIENT_VERSION, 3, egl::NONE],
            )
            .context("cannot create the EGL context")?;
        match Self::with_context(egl, display, config, context, window) {
            Ok((surface, procs)) => Ok((context, surface, procs)),
            Err(e) => {
                let _ = egl.destroy_context(display, context);
                Err(e)
            }
        }
    }

    fn with_context(
        egl: &EglInstance,
        display: egl::Display,
        config: egl::Config,
        context: egl::Context,
        window: &ndk::native_window::NativeWindow,
    ) -> Result<(egl::Surface, Procs), Error> {
        // SAFETY: the pointer is the live ANativeWindow, which outlives the surface because the
        // surface is destroyed before the window is terminated.
        let surface = unsafe {
            egl.create_window_surface(
                display,
                config,
                window.ptr().as_ptr().cast::<c_void>() as egl::NativeWindowType,
                None,
            )
        }
        .context("cannot create the EGL surface")?;
        let loaded = (|| -> Result<_, Error> {
            egl.make_current(display, Some(surface), Some(surface), Some(context))
                .context("cannot make the EGL context current")?;
            let proc = |name: &str| {
                egl.get_proc_address(name)
                    .ok_or_else(|| Error::new(format!("GLES has no {name}")))
            };
            // SAFETY: each name is the GLES entry point whose signature the field of `Procs` it
            // fills declares.
            unsafe {
                Ok(Procs {
                    clear_color: load(proc("glClearColor")?),
                    clear: load(proc("glClear")?),
                    gen_textures: load(proc("glGenTextures")?),
                    bind_texture: load(proc("glBindTexture")?),
                    tex_image_2d: load(proc("glTexImage2D")?),
                    tex_sub_image_2d: load(proc("glTexSubImage2D")?),
                    tex_parameteri: load(proc("glTexParameteri")?),
                    pixel_storei: load(proc("glPixelStorei")?),
                    delete_textures: load(proc("glDeleteTextures")?),
                })
            }
        })();
        let procs = match loaded {
            Ok(procs) => procs,
            Err(e) => {
                let _ = egl.make_current(display, None, None, None);
                let _ = egl.destroy_surface(display, surface);
                return Err(e);
            }
        };
        Ok((surface, procs))
    }

    fn swap(&self) {
        let _ = self.egl.swap_buffers(self.display, self.surface);
    }
}

/// Types a loaded GLES entry point as its real signature.
///
/// # Safety
/// `T` must be the function pointer type of the symbol `f` was loaded for.
unsafe fn load<T: Copy>(f: extern "system" fn()) -> T {
    assert_eq!(
        std::mem::size_of::<T>(),
        std::mem::size_of::<extern "system" fn()>()
    );
    // SAFETY: the caller vouches for the type, and the assert holds both to pointer size.
    unsafe { std::mem::transmute_copy(&f) }
}

impl Drop for Gl {
    fn drop(&mut self) {
        let _ = self.egl.make_current(self.display, None, None, None);
        let _ = self.egl.destroy_surface(self.display, self.surface);
        let _ = self.egl.destroy_context(self.display, self.context);
        let _ = self.egl.terminate(self.display);
    }
}

/// Dear ImGui with the Android and GLES3 backends, alive from window creation to termination.
struct Imgui {
    context: *mut sys::ImGuiContext,
}

impl Imgui {
    fn new(window: &ndk::native_window::NativeWindow, scale: f32) -> Result<Imgui, Error> {
        // SAFETY: called on the one UI thread with a current GLES context and a live window; the
        // shutdown pairs are in `Drop`, which runs before the surface and window go away.
        unsafe {
            let context = sys::igCreateContext(std::ptr::null_mut());
            let io = sys::igGetIO_Nil();
            (*io).IniFilename = std::ptr::null();
            let style = sys::igGetStyle();
            sys::igStyleColorsDark(style);
            let color = |r: f32, g: f32, b: f32| sys::ImVec4_c {
                x: r,
                y: g,
                z: b,
                w: 1.0,
            };
            (*style).FontSizeBase = 18.0;
            (*style).FrameRounding = 8.0;
            (*style).FramePadding = sys::ImVec2_c { x: 14.0, y: 12.0 };
            (*style).ItemSpacing = sys::ImVec2_c { x: 10.0, y: 12.0 };
            (*style).FrameBorderSize = 1.0;
            (*style).ScrollbarSize = 14.0;
            let colors = &mut (*style).Colors;
            colors[sys::ImGuiCol_Border as usize] = color(0.42, 0.45, 0.52);
            colors[sys::ImGuiCol_Button as usize] = color(0.17, 0.42, 0.82);
            colors[sys::ImGuiCol_ButtonHovered as usize] = color(0.23, 0.50, 0.92);
            colors[sys::ImGuiCol_ButtonActive as usize] = color(0.13, 0.34, 0.70);
            sys::ImGuiStyle_ScaleAllSizes(style, scale);
            (*style).FontScaleDpi = scale;
            // ImGui's default drag threshold is physical pixels and ScaleAllSizes leaves it, so
            // finger jitter would cancel both a tap and a long press.
            (*io).MouseDragThreshold = TOUCH_SLOP_DP * scale;
            if !sys::backend_shim::android::dear_imgui_backend_android_init(
                window.ptr().as_ptr().cast(),
            ) {
                sys::igDestroyContext(context);
                return Err(Error::new("the Android backend did not start"));
            }
            if !sys::backend_shim::opengl3::dear_imgui_backend_opengl3_init(
                c"#version 300 es".as_ptr(),
            ) {
                sys::backend_shim::android::dear_imgui_backend_android_shutdown();
                sys::igDestroyContext(context);
                return Err(Error::new("the GLES backend did not start"));
            }
            Ok(Imgui { context })
        }
    }

    fn display_size(&self) -> (f32, f32) {
        // SAFETY: the context is current and the io struct lives as long as it.
        let size = unsafe { (*sys::igGetIO_Nil()).DisplaySize };
        (size.x, size.y)
    }

    fn mouse(&self, x: f32, y: f32, down: Option<bool>) {
        // SAFETY: the context is current; these only queue events.
        unsafe {
            let io = sys::igGetIO_Nil();
            sys::ImGuiIO_AddMousePosEvent(io, x, y);
            if let Some(down) = down {
                sys::ImGuiIO_AddMouseButtonEvent(io, sys::ImGuiMouseButton_Left, down);
            }
        }
    }
}

impl Drop for Imgui {
    fn drop(&mut self) {
        // SAFETY: undoes `new` in reverse order, on the same thread, before the window dies.
        unsafe {
            sys::backend_shim::opengl3::dear_imgui_backend_opengl3_shutdown();
            sys::backend_shim::android::dear_imgui_backend_android_shutdown();
            sys::igDestroyContext(self.context);
        }
    }
}

/// The newest camera frame as a GL texture, made on the first upload and deleted when the scan
/// screen has no live camera.
struct Preview {
    procs: Procs,
    /// Zero while there is no texture.
    texture: u32,
    size: (usize, usize),
}

impl Preview {
    fn new(procs: Procs) -> Preview {
        Preview {
            procs,
            texture: 0,
            size: (0, 0),
        }
    }

    /// Needs the GL context current.
    fn upload(&mut self, luma: &Luma) {
        let (Ok(width), Ok(height)) = (i32::try_from(luma.width), i32::try_from(luma.height))
        else {
            return;
        };
        if luma.width == 0 || luma.data.len() != luma.width * luma.height {
            return;
        }
        let p = self.procs;
        // SAFETY: the GLES context is current on this thread, and `luma.data` holds exactly
        // width * height bytes, which is what a GL_R8 upload at alignment 1 reads.
        unsafe {
            (p.pixel_storei)(GL_UNPACK_ALIGNMENT, 1);
            if self.texture == 0 {
                (p.gen_textures)(1, &mut self.texture);
                (p.bind_texture)(GL_TEXTURE_2D, self.texture);
                (p.tex_parameteri)(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_LINEAR);
                (p.tex_parameteri)(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_LINEAR);
                (p.tex_parameteri)(GL_TEXTURE_2D, GL_TEXTURE_WRAP_S, GL_CLAMP_TO_EDGE);
                (p.tex_parameteri)(GL_TEXTURE_2D, GL_TEXTURE_WRAP_T, GL_CLAMP_TO_EDGE);
                // A GL_R8 texture samples as (r, 0, 0, 1); gray needs r in all three channels.
                for channel in [
                    GL_TEXTURE_SWIZZLE_R,
                    GL_TEXTURE_SWIZZLE_G,
                    GL_TEXTURE_SWIZZLE_B,
                ] {
                    (p.tex_parameteri)(GL_TEXTURE_2D, channel, GL_RED as i32);
                }
                (p.tex_parameteri)(GL_TEXTURE_2D, GL_TEXTURE_SWIZZLE_A, GL_ONE);
                self.size = (0, 0);
            } else {
                (p.bind_texture)(GL_TEXTURE_2D, self.texture);
            }
            let data = luma.data.as_ptr().cast::<c_void>();
            if self.size == (luma.width, luma.height) {
                (p.tex_sub_image_2d)(
                    GL_TEXTURE_2D,
                    0,
                    0,
                    0,
                    width,
                    height,
                    GL_RED,
                    GL_UNSIGNED_BYTE,
                    data,
                );
            } else {
                (p.tex_image_2d)(
                    GL_TEXTURE_2D,
                    0,
                    GL_R8,
                    width,
                    height,
                    0,
                    GL_RED,
                    GL_UNSIGNED_BYTE,
                    data,
                );
                self.size = (luma.width, luma.height);
            }
        }
    }

    fn texture(&self) -> Option<u32> {
        (self.texture != 0).then_some(self.texture)
    }

    /// Needs the GL context current.
    fn release(&mut self) {
        if self.texture != 0 {
            // SAFETY: the GLES context is current and `texture` is a name this made.
            unsafe { (self.procs.delete_textures)(1, &self.texture) };
            self.texture = 0;
            self.size = (0, 0);
        }
    }
}

impl Drop for Preview {
    fn drop(&mut self) {
        self.release();
    }
}

// `imgui` first: fields drop in declaration order and its backends need the GL context current.
// `preview` before `gl` so its texture is deleted while the context is still current.
struct Surface {
    imgui: Imgui,
    preview: Preview,
    gl: Gl,
}

/// What `render` needs beyond the app: the typed text, the camera state and the raw numbers the
/// platform reported about the window.
struct FrameParams<'a> {
    field: &'a str,
    camera_ready: bool,
    camera_denied: bool,
    preview: Option<PreviewView>,
    raw_insets: Option<RawInsets>,
    content_rect: Rect,
    scale: f32,
}

fn resolve_insets(raw: Option<RawInsets>, rect: Rect, display: (f32, f32)) -> Insets {
    layout::resolve_insets(
        raw.map(|raw| layout::WindowInsets {
            top: raw.top,
            left: raw.left,
            right: raw.right,
            bottom: raw.bottom,
            includes_keyboard: raw.includes_keyboard,
        }),
        (rect.left, rect.top, rect.right, rect.bottom),
        display,
    )
}

fn render(surface: &Surface, app: &App, state: &mut UiState, params: &FrameParams<'_>) -> Vec<Tap> {
    let view = app.view();
    // SAFETY: one frame on the UI thread with the context and the GLES context current.
    let taps = unsafe {
        sys::backend_shim::opengl3::dear_imgui_backend_opengl3_new_frame();
        sys::backend_shim::android::dear_imgui_backend_android_new_frame();
        sys::igNewFrame();
        let display = surface.imgui.display_size();
        let frame = ui::Frame {
            display,
            insets: resolve_insets(params.raw_insets, params.content_rect.clone(), display),
            margin: layout::margin(params.scale),
            field: params.field,
            camera_ready: params.camera_ready,
            camera_denied: params.camera_denied,
            preview: params.preview,
            scroll_to_end: true,
        };
        let taps = ui::draw(&view, &frame, state);
        sys::igRender();
        (surface.gl.procs.clear_color)(0.06, 0.06, 0.08, 1.0);
        (surface.gl.procs.clear)(0x4000);
        sys::backend_shim::opengl3::dear_imgui_backend_opengl3_render_draw_data(
            sys::igGetDrawData(),
        );
        taps
    };
    surface.gl.swap();
    taps
}

struct Activity {
    android: AndroidApp,
    app: App,
    store: Arc<DirStore>,
    runtime: tokio::runtime::Runtime,
    client: Option<Client>,
    surface: Option<Surface>,
    camera: Option<Camera>,
    /// The newest camera frame, kept for the preview and the decoder; cleared with `camera`.
    latest: Option<Luma>,
    /// `latest` arrived since the texture was last filled.
    latest_unuploaded: bool,
    /// `latest` arrived since the decoder last looked.
    latest_undecoded: bool,
    last_scan: Instant,
    /// What the soft keyboard has typed into the paste or prompt field.
    field: String,
    camera_denied: bool,
    camera_granted: bool,
    /// The permission is asked of Java only when it may have changed, not every frame.
    permission_stale: bool,
    /// A failed start is not retried until the permission is checked again.
    camera_failed: bool,
    /// The camera is held only while the activity is in the foreground.
    resumed: bool,
    /// Digest of the last pairing link taken from the intent, so a resume does not process it
    /// twice; a digest because the link carries a bearer secret.
    last_link: Option<[u8; 32]>,
    label: String,
    scale: f32,
    ui_state: UiState,
    /// The window insets as last read from Java, in physical pixels.
    insets: Option<RawInsets>,
    insets_read: Instant,
    /// An event that moves the bars or keyboard arrived since the last read.
    insets_stale: bool,
    /// `Display.getRotation()` as last read, refreshed with the insets; 0 when Java cannot say.
    display_quadrant: i32,
}

impl Activity {
    fn refresh_insets(&mut self) {
        if !self.insets_stale && self.insets_read.elapsed() < INSETS_INTERVAL {
            return;
        }
        self.insets_stale = false;
        self.insets_read = Instant::now();
        self.insets = jni_bridge::system_insets(&self.android);
        self.display_quadrant = jni_bridge::display_rotation(&self.android).unwrap_or(0);
    }

    /// The frame is dropped with the camera, which is what keeps the texture from outliving it.
    fn drop_camera(&mut self) {
        self.camera = None;
        self.latest = None;
        self.latest_unuploaded = false;
        self.latest_undecoded = false;
    }

    /// Fills the texture from the newest frame and says how to draw it, or deletes the texture
    /// when the scan screen has no live camera.
    fn upload_preview(&mut self) -> Option<PreviewView> {
        let surface = self.surface.as_mut()?;
        let (Some(camera), Some(luma)) = (self.camera.as_ref(), self.latest.as_ref()) else {
            surface.preview.release();
            return None;
        };
        if self.latest_unuploaded {
            surface.preview.upload(luma);
            self.latest_unuploaded = false;
        }
        let texture = surface.preview.texture()?;
        let rotation = layout::preview_rotation(
            camera.sensor_orientation(),
            camera.front_facing(),
            self.display_quadrant,
        );
        Some(PreviewView {
            texture,
            uv: layout::fit_cover((luma.width as u32, luma.height as u32), rotation),
        })
    }

    fn take_intent_link(&mut self) {
        let Some(link) = jni_bridge::intent_link(&self.android).filter(|l| is_pair_link(l)) else {
            return;
        };
        let digest: [u8; 32] = Sha256::digest(link.as_bytes()).into();
        if self.last_link == Some(digest) {
            return;
        }
        self.app.submit_link(&link);
        self.last_link = Some(digest);
    }

    fn refresh_hosts(&mut self) {
        match self.store.hosts() {
            Ok(hosts) => self.app.set_hosts(hosts.hosts),
            Err(e) => {
                log::warn!("cannot read the paired hosts: {}", chain(&e));
                self.app
                    .set_notice(format!("cannot read the paired hosts: {}", chain(&e)));
            }
        }
    }

    fn clear_field(&mut self) {
        self.field.clear();
        self.android.set_text_input_state(TextInputState::default());
    }

    fn perform(&mut self, action: Option<Action>) {
        match action {
            Some(Action::Start(target)) => {
                let _enter = self.runtime.enter();
                let store: Arc<dyn ClientStore> = self.store.clone();
                self.drop_camera();
                let mut options = AttachOptions::new(self.label.clone());
                // A stored host has no address in hand, so it is found through the relay, as
                // `fontaine attach` does by default.
                if matches!(target, Target::Host(_)) {
                    options.relay = Some(RelayMode::Default);
                }
                self.client = Some(Client::start(target, options, store));
            }
            Some(Action::Send(message)) => {
                if let Some(client) = &self.client {
                    client.send(message);
                }
            }
            Some(Action::Stop) => {
                self.client = None;
                self.refresh_hosts();
            }
            None => {}
        }
    }

    fn drain_client(&mut self) {
        loop {
            let Some(client) = self.client.as_mut() else {
                return;
            };
            let Ok(event) = client.events.try_recv() else {
                return;
            };
            let paired = matches!(event, fontaine_remote::ClientEvent::Paired);
            let action = self.app.on_client_event(event);
            if paired {
                self.refresh_hosts();
            }
            self.perform(action);
        }
    }

    fn handle_tap(&mut self, tap: Tap) {
        match tap {
            Tap::FocusField => self.android.show_soft_input(true),
            Tap::PasteField => {
                if let Some(text) = jni_bridge::clipboard_text(&self.android) {
                    self.field = text.trim().to_string();
                    self.android.set_text_input_state(TextInputState::default());
                }
            }
            Tap::SubmitField => {
                let text = std::mem::take(&mut self.field);
                let action = if *self.app.screen() == Screen::Session {
                    self.app.send_prompt(&text)
                } else {
                    self.app.submit_link(&text);
                    None
                };
                self.clear_field();
                self.android.hide_soft_input(false);
                self.perform(action);
            }
            Tap::GrantCamera => {
                jni_bridge::request_camera(&self.android);
                self.camera_denied = false;
                self.permission_stale = true;
            }
            Tap::ConfirmPairing => {
                let action = self.app.confirm_pairing();
                self.perform(action);
            }
            Tap::DeclinePairing => self.app.decline_pairing(),
            Tap::ConnectHost(node) => {
                let action = self.app.connect_host(&node);
                self.perform(action);
            }
            Tap::Interrupt => {
                let action = self.app.interrupt();
                self.perform(action);
            }
            Tap::Approval(choice) => {
                let action = self.app.answer_approval(&choice, None);
                self.perform(action);
            }
            Tap::Question(option) => {
                let action = self.app.answer_question(option);
                self.perform(action);
            }
            Tap::Back => {
                let action = self.app.back_to_scan();
                self.refresh_hosts();
                self.perform(action);
            }
        }
    }

    fn scan(&mut self) {
        let want_camera =
            *self.app.screen() == Screen::Scan && self.surface.is_some() && self.resumed;
        if !want_camera {
            self.drop_camera();
            return;
        }
        if self.camera.as_ref().is_some_and(Camera::is_dead) {
            self.drop_camera();
        }
        if self.permission_stale {
            self.permission_stale = false;
            self.camera_granted = jni_bridge::camera_granted(&self.android);
            self.camera_failed = false;
        }
        if self.camera.is_none() {
            if !self.camera_granted || self.camera_failed {
                self.camera_denied = true;
                return;
            }
            match Camera::start() {
                Ok(camera) => {
                    self.camera = Some(camera);
                    self.camera_denied = false;
                }
                Err(e) => {
                    log::warn!("the camera did not start: {}", chain(&e));
                    self.camera_failed = true;
                    self.camera_denied = true;
                    return;
                }
            }
        }
        if let Some(luma) = self.camera.as_mut().and_then(Camera::latest_luma) {
            self.latest = Some(luma);
            self.latest_unuploaded = true;
            self.latest_undecoded = true;
        }
        if self.last_scan.elapsed() < SCAN_INTERVAL {
            return;
        }
        self.last_scan = Instant::now();
        if !std::mem::take(&mut self.latest_undecoded) {
            return;
        }
        let text = self
            .latest
            .as_ref()
            .and_then(|luma| decode_luma(luma.width, luma.height, &luma.data));
        if let Some(text) = text {
            self.app.submit_link(&text);
        }
    }

    fn input(&mut self) {
        let Ok(mut events) = self.android.input_events_iter() else {
            return;
        };
        let mut typed: Option<String> = None;
        let mut submitted = false;
        let mut pressed: Vec<Key> = Vec::new();
        let imgui = self.surface.as_ref().map(|s| &s.imgui);
        while events.next(|event| match event {
            InputEvent::MotionEvent(motion) => {
                if let Some(imgui) = imgui
                    && motion.pointer_count() > 0
                {
                    let pointer = motion.pointer_at_index(0);
                    let (x, y) = (pointer.x(), pointer.y());
                    match motion.action() {
                        MotionAction::Down => imgui.mouse(x, y, Some(true)),
                        MotionAction::Up | MotionAction::Cancel => imgui.mouse(x, y, Some(false)),
                        _ => imgui.mouse(x, y, None),
                    }
                }
                InputStatus::Handled
            }
            InputEvent::KeyEvent(key) => {
                if key.action() != KeyAction::Down {
                    return InputStatus::Handled;
                }
                let code = key.key_code();
                pressed.push(match code {
                    Keycode::Enter | Keycode::NumpadEnter => Key::Enter,
                    Keycode::Del => Key::Backspace,
                    _ => match self
                        .android
                        .device_key_character_map(key.device_id())
                        .and_then(|map| map.get(code, key.meta_state()))
                    {
                        Ok(KeyMapChar::Unicode(c)) => Key::Char(c),
                        _ => return InputStatus::Unhandled,
                    },
                });
                InputStatus::Handled
            }
            InputEvent::TextEvent(state) => {
                typed = Some(state.text.clone());
                InputStatus::Handled
            }
            InputEvent::TextAction(action) => {
                submitted = matches!(
                    action,
                    TextInputAction::Send | TextInputAction::Done | TextInputAction::Go
                );
                InputStatus::Handled
            }
            _ => InputStatus::Unhandled,
        }) {}
        if let Some(text) = typed {
            self.field = text;
        }
        for key in pressed {
            if keys::apply(&mut self.field, key) {
                submitted = true;
            }
        }
        if submitted && !self.field.is_empty() {
            self.handle_tap(Tap::SubmitField);
        }
    }

    fn frame(&mut self) {
        self.input();
        self.drain_client();
        self.scan();
        self.refresh_insets();
        let preview = self.upload_preview();
        let params = FrameParams {
            field: &self.field,
            camera_ready: self.camera.is_some(),
            camera_denied: self.camera_denied,
            preview,
            raw_insets: self.insets,
            content_rect: self.android.content_rect(),
            scale: self.scale,
        };
        let taps = match &self.surface {
            Some(surface) => render(surface, &self.app, &mut self.ui_state, &params),
            None => Vec::new(),
        };
        for tap in taps {
            self.handle_tap(tap);
        }
    }

    fn open_surface(&mut self) {
        let Some(window) = self.android.native_window() else {
            return;
        };
        let built = Gl::new(&window)
            .and_then(|gl| Imgui::new(&window, self.scale).map(|imgui| (gl, imgui)));
        match built {
            Ok((gl, imgui)) => {
                let preview = Preview::new(gl.procs);
                self.surface = Some(Surface { imgui, preview, gl });
            }
            Err(e) => log::error!("the screen could not start: {}", chain(&e)),
        }
    }
}

#[unsafe(no_mangle)]
fn android_main(app: AndroidApp) {
    android_logger::init_once(
        android_logger::Config::default()
            .with_max_level(log::LevelFilter::Info)
            .with_tag("fontaine"),
    );
    let Some(files) = app.internal_data_path() else {
        log::error!("the app has no private files directory");
        return;
    };
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(e) => {
            log::error!("cannot start the async runtime: {e}");
            return;
        }
    };
    let scale = app
        .config()
        .density()
        .map_or(2.0, |dpi| (dpi as f32 / 160.0).max(1.0));
    let label = jni_bridge::device_model(&app);
    let mut activity = Activity {
        android: app.clone(),
        app: App::new(),
        store: Arc::new(DirStore::new(&files)),
        runtime,
        client: None,
        surface: None,
        camera: None,
        latest: None,
        latest_unuploaded: false,
        latest_undecoded: false,
        last_scan: Instant::now(),
        field: String::new(),
        camera_denied: false,
        camera_granted: false,
        permission_stale: true,
        camera_failed: false,
        resumed: false,
        last_link: None,
        label,
        scale,
        ui_state: UiState::default(),
        insets: None,
        insets_read: Instant::now(),
        insets_stale: true,
        display_quadrant: 0,
    };
    activity.refresh_hosts();
    activity.take_intent_link();

    let mut running = true;
    while running {
        let timeout = if activity.surface.is_some() {
            Duration::from_millis(16)
        } else {
            Duration::from_millis(250)
        };
        app.poll_events(Some(timeout), |event| {
            if let PollEvent::Main(event) = event {
                match event {
                    MainEvent::InitWindow { .. } => activity.open_surface(),
                    MainEvent::TerminateWindow { .. } => {
                        activity.drop_camera();
                        activity.surface = None;
                    }
                    MainEvent::WindowResized { .. } | MainEvent::ContentRectChanged { .. } => {
                        activity.insets_stale = true;
                    }
                    MainEvent::Pause => {
                        activity.resumed = false;
                        activity.drop_camera();
                    }
                    MainEvent::Resume { .. } => {
                        activity.resumed = true;
                        activity.insets_stale = true;
                        activity.drop_camera();
                        activity.camera_failed = false;
                        activity.permission_stale = true;
                        // getIntent() stays the launch intent, so this only catches a launch link
                        // after the activity was recreated; a link opened while running is not seen.
                        activity.take_intent_link();
                    }
                    MainEvent::Destroy => running = false,
                    _ => {}
                }
            }
        });
        activity.frame();
    }
}
