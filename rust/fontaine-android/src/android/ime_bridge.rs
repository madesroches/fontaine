//! The two ends of the input-method connection in `FontaineActivity`: the native methods Java
//! calls on the UI thread to report what the keyboard did, which only store it for the next frame,
//! and the calls this app makes into Java to mirror the field and show or hide the keyboard.

use std::sync::Mutex;

use android_activity::AndroidApp;
use jni::objects::{JClass, JString, JValue};
use jni::sys::jint;
use jni::{EnvUnowned, jni_sig, jni_str};

use super::jni_bridge::with_activity;
use crate::ime::{ImeState, Pending};

// The critical sections are plain assignments and `take`, so the lock cannot poison.
static PENDING: Mutex<Pending> = Mutex::new(Pending::new());

fn pending() -> std::sync::MutexGuard<'static, Pending> {
    PENDING.lock().expect("the pending IME slot cannot poison")
}

/// Everything Java reported since the last call: the newest state with its generation, and whether
/// the keyboard asked to submit.
pub fn take() -> (Option<(ImeState, u32)>, bool) {
    pending().take()
}

/// Negative means no span, which is how Java says there is no composing region.
fn units(at: jint) -> usize {
    usize::try_from(at).unwrap_or(0)
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_fontaine_remote_FontaineActivity_nativeImeState(
    mut env: EnvUnowned,
    _class: JClass,
    text: JString,
    sel_start: jint,
    sel_end: jint,
    comp_start: jint,
    comp_end: jint,
    generation: jint,
) {
    let text = match env
        .with_env(|env| {
            text.try_to_string(env).inspect_err(|_| {
                // A pending Java exception must be cleared or the next JNI call aborts the process.
                env.exception_clear();
            })
        })
        .into_outcome()
    {
        jni::Outcome::Ok(text) => text,
        jni::Outcome::Err(e) => {
            log::warn!("cannot read the keyboard's text: {e}");
            return;
        }
        jni::Outcome::Panic(_) => {
            log::warn!("panic while reading the keyboard's text");
            return;
        }
    };
    let composing =
        (comp_start >= 0 && comp_end >= 0).then(|| (units(comp_start), units(comp_end)));
    let state = ImeState {
        text,
        sel: (units(sel_start), units(sel_end)),
        composing,
    };
    pending().set_state(state, u32::try_from(generation).unwrap_or(0));
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_fontaine_remote_FontaineActivity_nativeImeSubmit(
    _env: EnvUnowned,
    _class: JClass,
) {
    pending().set_submit();
}

/// Replaces the keyboard's mirror of the field. False when Java could not be reached, so the
/// caller offers the same state again next frame.
pub fn set_state(app: &AndroidApp, state: &ImeState, generation: u32, wholesale: bool) -> bool {
    with_activity(app, |env, activity| {
        let text = env.new_string(&state.text)?;
        let (comp_start, comp_end) = state
            .composing
            .map_or((-1, -1), |(start, end)| (start as jint, end as jint));
        env.call_method(
            activity,
            jni_str!("setImeState"),
            jni_sig!("(Ljava/lang/String;IIIIIZ)V"),
            &[
                JValue::Object(&text),
                JValue::Int(state.sel.0 as jint),
                JValue::Int(state.sel.1 as jint),
                JValue::Int(comp_start),
                JValue::Int(comp_end),
                JValue::Int(generation as jint),
                JValue::Bool(wholesale),
            ],
        )?;
        Ok(())
    })
    .is_some()
}

pub fn show(app: &AndroidApp) {
    with_activity(app, |env, activity| {
        env.call_method(activity, jni_str!("showKeyboard"), jni_sig!("()V"), &[])?;
        Ok(())
    });
}

pub fn hide(app: &AndroidApp) {
    with_activity(app, |env, activity| {
        env.call_method(activity, jni_str!("hideKeyboard"), jni_sig!("()V"), &[])?;
        Ok(())
    });
}
