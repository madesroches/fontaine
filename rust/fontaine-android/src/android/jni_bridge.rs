//! The few Java calls the app needs: the camera permission, the device model for the pairing
//! label, and the deep-link text of the intent that started the activity.

use android_activity::AndroidApp;
use jni::JavaVM;
use jni::objects::{JObject, JString, JValue};

const CAMERA: &str = "android.permission.CAMERA";
const GRANTED: i32 = 0;

/// Runs `body` on a thread attached to the VM, with the activity object.
fn with_activity<T>(
    app: &AndroidApp,
    body: impl FnOnce(&mut jni::JNIEnv<'_>, &JObject<'_>) -> jni::errors::Result<T>,
) -> Option<T> {
    // SAFETY: android-activity hands out the process's JavaVM and the activity's global
    // reference, both of which stay valid for the life of the activity.
    let vm = unsafe { JavaVM::from_raw(app.vm_as_ptr().cast()) }.ok()?;
    let mut env = vm.attach_current_thread().ok()?;
    // SAFETY: `activity_as_ptr` is a live global reference to the activity.
    let activity = unsafe { JObject::from_raw(app.activity_as_ptr().cast()) };
    match body(&mut env, &activity) {
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
