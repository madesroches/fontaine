//! The few Java calls the app needs: the camera permission, the device model for the pairing
//! label, the clipboard (read and write), the window insets, the display rotation, and the
//! deep-link text of the intent that started the activity.

use android_activity::AndroidApp;
use jni::objects::{JObject, JObjectArray, JString, JValue};
use jni::strings::JNIString;
use jni::{Env, JavaVM, jni_sig, jni_str};

const CAMERA: &str = "android.permission.CAMERA";
const GRANTED: i32 = 0;

/// Runs `body` on a thread attached to the VM, with the activity object.
pub(super) fn with_activity<T>(
    app: &AndroidApp,
    body: impl FnOnce(&mut Env<'_>, &JObject<'_>) -> jni::errors::Result<T>,
) -> Option<T> {
    // SAFETY: android-activity hands out the process's JavaVM, which stays valid for the life of
    // the activity.
    let vm = unsafe { JavaVM::from_raw(app.vm_as_ptr().cast()) };
    // The attachment pushes a frame that frees the local references `body` makes, and clears a
    // pending Java exception on failure: a thread attached from native code never returns to
    // Java, so nothing else would release either.
    vm.attach_current_thread(|env| {
        // SAFETY: `activity_as_ptr` is a live global reference to the activity.
        let activity = unsafe { JObject::from_raw(env, app.activity_as_ptr().cast()) };
        body(env, &activity)
    })
    .ok()
}

pub fn camera_granted(app: &AndroidApp) -> bool {
    with_activity(app, |env, activity| {
        let permission = env.new_string(CAMERA)?;
        env.call_method(
            activity,
            jni_str!("checkSelfPermission"),
            jni_sig!("(Ljava/lang/String;)I"),
            &[JValue::Object(&permission)],
        )?
        .i()
    })
    .is_some_and(|result| result == GRANTED)
}

pub fn request_camera(app: &AndroidApp) {
    with_activity(app, |env, activity| {
        let permission = env.new_string(CAMERA)?;
        let array = JObjectArray::<JString>::new(env, 1, &permission)?;
        env.call_method(
            activity,
            jni_str!("requestPermissions"),
            jni_sig!("([Ljava/lang/String;I)V"),
            &[JValue::Object(&array), JValue::Int(1)],
        )?;
        Ok(())
    });
}

/// `Build.MODEL`, which labels the device on the host's confirmation line.
pub fn device_model(app: &AndroidApp) -> String {
    with_activity(app, |env, _| {
        let value = env
            .get_static_field(
                jni_str!("android/os/Build"),
                jni_str!("MODEL"),
                jni_sig!("Ljava/lang/String;"),
            )?
            .l()?;
        let text = JString::cast_local(env, value)?.try_to_string(env)?;
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
                jni_str!("getSystemService"),
                jni_sig!("(Ljava/lang/String;)Ljava/lang/Object;"),
                &[JValue::Object(&name)],
            )?
            .l()?;
        if manager.is_null() {
            return Ok(None);
        }
        let clip = env
            .call_method(
                &manager,
                jni_str!("getPrimaryClip"),
                jni_sig!("()Landroid/content/ClipData;"),
                &[],
            )?
            .l()?;
        if clip.is_null() {
            return Ok(None);
        }
        let count = env
            .call_method(&clip, jni_str!("getItemCount"), jni_sig!("()I"), &[])?
            .i()?;
        if count < 1 {
            return Ok(None);
        }
        let item = env
            .call_method(
                &clip,
                jni_str!("getItemAt"),
                jni_sig!("(I)Landroid/content/ClipData$Item;"),
                &[JValue::Int(0)],
            )?
            .l()?;
        let text = env
            .call_method(
                &item,
                jni_str!("getText"),
                jni_sig!("()Ljava/lang/CharSequence;"),
                &[],
            )?
            .l()?;
        if text.is_null() {
            return Ok(None);
        }
        let text = env
            .call_method(
                &text,
                jni_str!("toString"),
                jni_sig!("()Ljava/lang/String;"),
                &[],
            )?
            .l()?;
        let text = JString::cast_local(env, text)?.try_to_string(env)?;
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
                jni_str!("getSystemService"),
                jni_sig!("(Ljava/lang/String;)Ljava/lang/Object;"),
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
                jni_str!("android/content/ClipData"),
                jni_str!("newPlainText"),
                jni_sig!(
                    "(Ljava/lang/CharSequence;Ljava/lang/CharSequence;)Landroid/content/ClipData;"
                ),
                &[JValue::Object(&label), JValue::Object(&body)],
            )?
            .l()?;
        env.call_method(
            &manager,
            jni_str!("setPrimaryClip"),
            jni_sig!("(Landroid/content/ClipData;)V"),
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
            .call_method(
                activity,
                jni_str!("getIntent"),
                jni_sig!("()Landroid/content/Intent;"),
                &[],
            )?
            .l()?;
        if intent.is_null() {
            return Ok(None);
        }
        let data = env
            .call_method(
                &intent,
                jni_str!("getDataString"),
                jni_sig!("()Ljava/lang/String;"),
                &[],
            )?
            .l()?;
        if data.is_null() {
            return Ok(None);
        }
        let text = JString::cast_local(env, data)?.try_to_string(env)?;
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
                jni_str!("getWindowManager"),
                jni_sig!("()Landroid/view/WindowManager;"),
                &[],
            )?
            .l()?;
        let display = env
            .call_method(
                &manager,
                jni_str!("getDefaultDisplay"),
                jni_sig!("()Landroid/view/Display;"),
                &[],
            )?
            .l()?;
        env.call_method(&display, jni_str!("getRotation"), jni_sig!("()I"), &[])?
            .i()
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
            .call_method(
                activity,
                jni_str!("getWindow"),
                jni_sig!("()Landroid/view/Window;"),
                &[],
            )?
            .l()?;
        let decor = env
            .call_method(
                &window,
                jni_str!("getDecorView"),
                jni_sig!("()Landroid/view/View;"),
                &[],
            )?
            .l()?;
        let insets = env
            .call_method(
                &decor,
                jni_str!("getRootWindowInsets"),
                jni_sig!("()Landroid/view/WindowInsets;"),
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
                    .call_static_method(
                        jni_str!("android/view/WindowInsets$Type"),
                        JNIString::new(kind),
                        jni_sig!("()I"),
                        &[],
                    )?
                    .i()?;
            }
            let found = env
                .call_method(
                    &insets,
                    jni_str!("getInsets"),
                    jni_sig!("(I)Landroid/graphics/Insets;"),
                    &[JValue::Int(mask)],
                )?
                .l()?;
            let mut edge = |name: &str| {
                env.get_field(&found, JNIString::new(name), jni_sig!("I"))?
                    .i()
            };
            Ok(Some(RawInsets {
                top: edge("top")?,
                left: edge("left")?,
                right: edge("right")?,
                bottom: edge("bottom")?,
                includes_keyboard: true,
            }))
        } else {
            let mut edge = |name: &str| {
                env.call_method(&insets, JNIString::new(name), jni_sig!("()I"), &[])?
                    .i()
            };
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
