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
use crate::store::DirStore;
use camera::Camera;
use error::{Context, Error, chain};
use jni_bridge::RawInsets;
use ui::{Tap, UiState};

/// How often a camera frame is decoded: a QR decode costs more than a frame's budget.
const SCAN_INTERVAL: Duration = Duration::from_millis(150);

/// How often the window insets are asked of Java between the events that change them.
const INSETS_INTERVAL: Duration = Duration::from_millis(500);

/// Android touch slop in dp.
const TOUCH_SLOP_DP: f32 = 8.0;

type EglInstance = egl::DynamicInstance<egl::EGL1_4>;

type GlClearColor = unsafe extern "system" fn(f32, f32, f32, f32);
type GlClear = unsafe extern "system" fn(u32);
type Made = (egl::Context, egl::Surface, GlClearColor, GlClear);

struct Gl {
    egl: EglInstance,
    display: egl::Display,
    surface: egl::Surface,
    context: egl::Context,
    clear_color: GlClearColor,
    clear: GlClear,
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
            Ok((context, surface, clear_color, clear)) => Ok(Gl {
                egl,
                display,
                surface,
                context,
                clear_color,
                clear,
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
            Ok((surface, clear_color, clear)) => Ok((context, surface, clear_color, clear)),
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
    ) -> Result<(egl::Surface, GlClearColor, GlClear), Error> {
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
            Ok((proc("glClearColor")?, proc("glClear")?))
        })();
        let (clear_color, clear) = match loaded {
            Ok(procs) => procs,
            Err(e) => {
                let _ = egl.make_current(display, None, None, None);
                let _ = egl.destroy_surface(display, surface);
                return Err(e);
            }
        };
        Ok((
            surface,
            // SAFETY: both are the GLES entry points of those names, whose signatures these are.
            unsafe { std::mem::transmute::<extern "system" fn(), GlClearColor>(clear_color) },
            unsafe { std::mem::transmute::<extern "system" fn(), GlClear>(clear) },
        ))
    }

    fn swap(&self) {
        let _ = self.egl.swap_buffers(self.display, self.surface);
    }
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

// `imgui` first: fields drop in declaration order and its backends need the GL context current.
struct Surface {
    imgui: Imgui,
    gl: Gl,
}

/// What `render` needs beyond the app: the typed text, the camera state and the raw numbers the
/// platform reported about the window.
struct FrameParams<'a> {
    field: &'a str,
    camera_ready: bool,
    camera_denied: bool,
    raw_insets: Option<RawInsets>,
    content_rect: Rect,
    scale: f32,
}

/// The insets for this frame. The window insets are primary; the content rect, which the
/// activity reports without a Java call, covers the time before the first read and the keyboard
/// before API 30.
fn resolve_insets(raw: Option<RawInsets>, rect: Rect, display: (f32, f32)) -> Insets {
    let from_rect =
        Insets::from_content_rect(rect.left, rect.top, rect.right, rect.bottom, display);
    let insets = match raw {
        Some(raw) => Insets {
            top: raw.top as f32,
            left: raw.left as f32,
            right: raw.right as f32,
            bottom: if raw.includes_keyboard {
                raw.bottom as f32
            } else {
                (raw.bottom as f32).max(from_rect.bottom)
            },
        },
        None => from_rect,
    };
    insets.clamped(display)
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
            scroll_to_end: true,
        };
        let taps = ui::draw(&view, &frame, state);
        sys::igRender();
        (surface.gl.clear_color)(0.06, 0.06, 0.08, 1.0);
        (surface.gl.clear)(0x4000);
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
}

impl Activity {
    fn refresh_insets(&mut self) {
        if !self.insets_stale && self.insets_read.elapsed() < INSETS_INTERVAL {
            return;
        }
        self.insets_stale = false;
        self.insets_read = Instant::now();
        self.insets = jni_bridge::system_insets(&self.android);
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
                self.camera = None;
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
            self.camera = None;
            return;
        }
        if self.camera.as_ref().is_some_and(Camera::is_dead) {
            self.camera = None;
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
        if self.last_scan.elapsed() < SCAN_INTERVAL {
            return;
        }
        self.last_scan = Instant::now();
        if let Some(text) = self.camera.as_mut().and_then(Camera::poll) {
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
        let params = FrameParams {
            field: &self.field,
            camera_ready: self.camera.is_some(),
            camera_denied: self.camera_denied,
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
            Ok((gl, imgui)) => self.surface = Some(Surface { gl, imgui }),
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
                        activity.camera = None;
                        activity.surface = None;
                    }
                    MainEvent::WindowResized { .. } | MainEvent::ContentRectChanged { .. } => {
                        activity.insets_stale = true;
                    }
                    MainEvent::Pause => {
                        activity.resumed = false;
                        activity.camera = None;
                    }
                    MainEvent::Resume { .. } => {
                        activity.resumed = true;
                        activity.insets_stale = true;
                        activity.camera = None;
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
