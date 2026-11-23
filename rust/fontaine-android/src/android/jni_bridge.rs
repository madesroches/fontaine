//! The few Java calls the app needs: the camera permission, the device model for the pairing
//! label, the clipboard (read and write), the window insets, the display rotation, and the
//! deep-link text of the intent that started the activity.

use android_activity::AndroidApp;
use jni::JavaVM;
use jni::objects::{JObject, JString, JValue};

const CAMERA: &str = "android.permission.CAMERA";
const GRANTED: i32 = 0;

/// Runs `body` on a thread attached to the VM, with the activity object.
pub(super) fn with_activity<T>(
    app: &AndroidApp,
    body: impl FnOnce(&mut jni::JNIEnv<'_>, &JObject<'_>) -> jni::errors::Result<T>,
) -> Option<T> {
    // SAFETY: android-activity hands out the process's JavaVM and the activity's global
    // reference, both of which stay valid for the life of the activity.
    let vm = unsafe { JavaVM::from_raw(app.vm_as_ptr().cast()) }.ok()?;
    let mut env = vm.attach_current_thread().ok()?;
    // SAFETY: `activity_as_ptr` is a live global reference to the activity.
    let activity = unsafe { JObject::from_raw(app.activity_as_ptr().cast()) };
    // The frame frees the local references `body` makes: a thread attached from native code never
    // returns to Java, so nothing else would release them.
    match env.with_local_frame(16, |env| body(env, &activity)) {
        Ok(value) => Some(value),
        Err(_) => {
            // A pending Java exception must be cleared or the next JNI call aborts the process.
            let _ = env.exception_clear();
            None
        }
    }
}

pub fn camera_granted(app: &AndroidApp) -> bool {
    with_activity(app, |env, activity| {
        let permission = env.new_string(CAMERA)?;
        env.call_method(
            activity,
            "checkSelfPermission",
            "(Ljava/lang/String;)I",
            &[JValue::Object(&permission)],
        )?
        .i()
    })
    .is_some_and(|result| result == GRANTED)
}

pub fn request_camera(app: &AndroidApp) {
    with_activity(app, |env, activity| {
        let permission = env.new_string(CAMERA)?;
        let array = env.new_object_array(1, "java/lang/String", &permission)?;
        env.call_method(
            activity,
            "requestPermissions",
            "([Ljava/lang/String;I)V",
            &[JValue::Object(&array), JValue::Int(1)],
        )?;
        Ok(())
    });
}

/// `Build.MODEL`, which labels the device on the host's confirmation line.
pub fn device_model(app: &AndroidApp) -> String {
    with_activity(app, |env, _| {
        let value = env
            .get_static_field("android/os/Build", "MODEL", "Ljava/lang/String;")?
            .l()?;
        let text: String = env.get_string(&JString::from(value))?.into();
        Ok(text)
    })
    .filter(|model| !model.trim().is_empty())
    .unwrap_or_else(|| "Android phone".to_string())
}

/// The clipboard's first item as text. Android 10+ only serves it while the app has focus, which
/// holds when the user has just tapped the Paste entry of the field's long-press popup.
pub fn clipboard_text(app: &AndroidApp) -> Option<String> {
    with_activity(app, |env, activity| {
        let name = env.new_string("clipboard")?;
        let manager = env
            .call_method(
                activity,
                "getSystemService",
                "(Ljava/lang/String;)Ljava/lang/Object;",
                &[JValue::Object(&name)],
            )?
            .l()?;
        if manager.is_null() {
            return Ok(None);
        }
        let clip = env
            .call_method(
                &manager,
                "getPrimaryClip",
                "()Landroid/content/ClipData;",
                &[],
            )?
            .l()?;
        if clip.is_null() {
            return Ok(None);
        }
        let count = env.call_method(&clip, "getItemCount", "()I", &[])?.i()?;
        if count < 1 {
            return Ok(None);
        }
        let item = env
            .call_method(
                &clip,
                "getItemAt",
                "(I)Landroid/content/ClipData$Item;",
                &[JValue::Int(0)],
            )?
            .l()?;
        let text = env
            .call_method(&item, "getText", "()Ljava/lang/CharSequence;", &[])?
            .l()?;
        if text.is_null() {
            return Ok(None);
        }
        let text = env
            .call_method(&text, "toString", "()Ljava/lang/String;", &[])?
            .l()?;
        let text: String = env.get_string(&JString::from(text))?.into();
        Ok(Some(text))
    })
    .flatten()
}

/// Puts `text` on the clipboard as plain text. `None` when Java refused or cannot be reached, so
/// the caller can keep what it was about to move.
pub fn set_clipboard_text(app: &AndroidApp, text: &str) -> Option<()> {
    with_activity(app, |env, activity| {
        let name = env.new_string("clipboard")?;
        let manager = env
            .call_method(
                activity,
                "getSystemService",
                "(Ljava/lang/String;)Ljava/lang/Object;",
                &[JValue::Object(&name)],
            )?
            .l()?;
        if manager.is_null() {
            return Ok(false);
        }
        let label = env.new_string("Fontaine")?;
        let body = env.new_string(text)?;
        let clip = env
            .call_static_method(
                "android/content/ClipData",
                "newPlainText",
                "(Ljava/lang/CharSequence;Ljava/lang/CharSequence;)Landroid/content/ClipData;",
                &[JValue::Object(&label), JValue::Object(&body)],
            )?
            .l()?;
        env.call_method(
            &manager,
            "setPrimaryClip",
            "(Landroid/content/ClipData;)V",
            &[JValue::Object(&clip)],
        )?;
        Ok(true)
    })
    .filter(|written| *written)
    .map(|_| ())
}

/// The data string of the launching intent, when the activity was opened by a link.
pub fn intent_link(app: &AndroidApp) -> Option<String> {
    with_activity(app, |env, activity| {
        let intent = env
            .call_method(activity, "getIntent", "()Landroid/content/Intent;", &[])?
            .l()?;
        if intent.is_null() {
            return Ok(None);
        }
        let data = env
            .call_method(&intent, "getDataString", "()Ljava/lang/String;", &[])?
            .l()?;
        if data.is_null() {
            return Ok(None);
        }
        let text: String = env.get_string(&JString::from(data))?.into();
        Ok(Some(text))
    })
    .flatten()
}

/// `Display.getRotation()`: 0..=3 quarter turns of the display from its natural orientation, or
/// `None` when Java cannot say.
pub fn display_rotation(app: &AndroidApp) -> Option<i32> {
    with_activity(app, |env, activity| {
        let manager = env
            .call_method(
                activity,
                "getWindowManager",
                "()Landroid/view/WindowManager;",
                &[],
            )?
            .l()?;
        let display = env
            .call_method(
                &manager,
                "getDefaultDisplay",
                "()Landroid/view/Display;",
                &[],
            )?
            .l()?;
        env.call_method(&display, "getRotation", "()I", &[])?.i()
    })
}

/// What the system covers on each edge of the window, as the platform reports it.
#[derive(Clone, Copy, Debug)]
pub struct RawInsets {
    pub top: i32,
    pub left: i32,
    pub right: i32,
    pub bottom: i32,
    /// The soft keyboard is part of the values; before API 30 only the content rect knows it.
    pub includes_keyboard: bool,
}

/// The system bars, display cutout and (API 30+) keyboard over the window, or `None` while the
/// decor view is not attached yet. `NativeActivity`'s surface does not shrink for the keyboard, so
/// the app has to subtract it itself.
pub fn system_insets(app: &AndroidApp) -> Option<RawInsets> {
    let modern = AndroidApp::sdk_version() >= 30;
    with_activity(app, |env, activity| {
        let window = env
            .call_method(activity, "getWindow", "()Landroid/view/Window;", &[])?
            .l()?;
        let decor = env
            .call_method(&window, "getDecorView", "()Landroid/view/View;", &[])?
            .l()?;
        let insets = env
            .call_method(
                &decor,
                "getRootWindowInsets",
                "()Landroid/view/WindowInsets;",
                &[],
            )?
            .l()?;
        if insets.is_null() {
            return Ok(None);
        }
        if modern {
            let mut mask = 0;
            for kind in ["systemBars", "displayCutout", "ime"] {
                mask |= env
                    .call_static_method("android/view/WindowInsets$Type", kind, "()I", &[])?
                    .i()?;
            }
            let found = env
                .call_method(
                    &insets,
                    "getInsets",
                    "(I)Landroid/graphics/Insets;",
                    &[JValue::Int(mask)],
                )?
                .l()?;
            let mut edge = |name: &str| env.get_field(&found, name, "I")?.i();
            Ok(Some(RawInsets {
                top: edge("top")?,
                left: edge("left")?,
                right: edge("right")?,
                bottom: edge("bottom")?,
                includes_keyboard: true,
            }))
        } else {
            let mut edge = |name: &str| env.call_method(&insets, name, "()I", &[])?.i();
            Ok(Some(RawInsets {
                top: edge("getSystemWindowInsetTop")?,
                left: edge("getSystemWindowInsetLeft")?,
                right: edge("getSystemWindowInsetRight")?,
                bottom: edge("getSystemWindowInsetBottom")?,
                includes_keyboard: false,
            }))
        }
    })
    .flatten()
}
