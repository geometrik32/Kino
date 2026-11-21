//! Android-only glue (JNI) and ExoPlayer bridge.

use std::sync::{
    atomic::{AtomicU64, Ordering},
    OnceLock, RwLock,
};

use dioxus::mobile::wry::prelude::dispatch;
use jni::{
    objects::{GlobalRef, JObject, JString, JValue},
    JNIEnv, JavaVM,
};

static NEXT_NATIVE_SEQ: AtomicU64 = AtomicU64::new(1);
static JAVA_VM: OnceLock<JavaVM> = OnceLock::new();
static ACTIVITY: RwLock<Option<GlobalRef>> = RwLock::new(None);

fn cache_vm_and_activity(env: &mut JNIEnv, activity: &JObject) {
    if JAVA_VM.get().is_none() {
        if let Ok(vm) = env.get_java_vm() {
            let _ = JAVA_VM.set(vm);
        }
    }
    let needs_update = {
        let guard = ACTIVITY.read().unwrap_or_else(|e| e.into_inner());
        match guard.as_ref() {
            Some(old) => !env.is_same_object(old.as_obj(), activity).unwrap_or(false),
            None => true,
        }
    };
    if needs_update {
        if let Ok(g) = env.new_global_ref(activity) {
            *ACTIVITY.write().unwrap_or_else(|e| e.into_inner()) = Some(g);
        }
    }
}

/// Opens a link with the system: `https://t.me/...` goes to the Telegram app when it is installed.
pub fn open_url(url: &str) -> Result<(), String> {
    if !super::is_safe_external_url(url) {
        return Err("disallowed URL scheme".into());
    }
    let url = url.to_owned();
    // The closure runs on the Android UI thread, where the activity can start another one.
    dispatch(move |env, activity, _webview| {
        let result: jni::errors::Result<()> = env.with_local_frame(16, |env| {
            let jurl = env.new_string(&url)?;
            let uri = env
                .call_static_method(
                    "android/net/Uri",
                    "parse",
                    "(Ljava/lang/String;)Landroid/net/Uri;",
                    &[(&jurl).into()],
                )?
                .l()?;
            let action = env.new_string("android.intent.action.VIEW")?;
            let intent = env.new_object(
                "android/content/Intent",
                "(Ljava/lang/String;Landroid/net/Uri;)V",
                &[(&action).into(), (&uri).into()],
            )?;
            env.call_method(
                activity,
                "startActivity",
                "(Landroid/content/Intent;)V",
                &[(&intent).into()],
            )?;
            Ok(())
        });
        if let Err(e) = result {
            let _ = env.exception_clear();
            eprintln!("cannot open {url}: {e}");
        }
    });
    Ok(())
}

/// The page's own colour (`--bg`), so the system bars do not show up as grey stripes.
const DARK_BAR_COLOR: i32 = 0xFF0E1621_u32 as i32;
const LIGHT_BAR_COLOR: i32 = 0xFFF7F2E8_u32 as i32;

/// Runs `f` on the Android UI thread with the activity; failures are only logged.
fn on_activity(what: &'static str, f: impl FnOnce(&mut JNIEnv, &JObject) -> jni::errors::Result<()> + Send + 'static) {
    dispatch(move |env, activity, _webview| {
        cache_vm_and_activity(env, activity);
        let result = env.with_local_frame(16, |env| f(env, activity));
        if let Err(e) = result {
            let _ = env.exception_clear();
            eprintln!("{what}: {e}");
        }
    });
}

/// Dark or light status and navigation bars, matching the active theme.
pub fn style_bars(light: bool) {
    let color = if light { LIGHT_BAR_COLOR } else { DARK_BAR_COLOR };
    on_activity("style bars", move |env, activity| {
        env.call_method(
            activity,
            "kinoStyleBars",
            "(ZI)V",
            &[JValue::Bool(u8::from(light)), JValue::Int(color)],
        )?;
        Ok(())
    });
}

/// Enables or disables the Android hardware back-button interception callback.
pub fn set_back_enabled(enabled: bool) {
    on_activity("set back enabled", move |env, activity| {
        env.call_method(
            activity,
            "kinoSetBackEnabled",
            "(Z)V",
            &[JValue::Bool(u8::from(enabled))],
        )?;
        Ok(())
    });
}

/// Landscape and hidden system bars while a film plays; back to normal afterwards.
pub fn player_mode(on: bool) {
    on_activity("player mode", move |env, activity| {
        env.call_method(activity, "kinoSetPlayerMode", "(Z)V", &[JValue::Bool(u8::from(on))])?;
        Ok(())
    });
}

/// Snapshot polled from `MainActivity.kinoNativePoll()`.
#[derive(Clone, Copy, Debug, Default)]
pub struct NativePlayerSnapshot {
    pub open_seq: u64,
    pub seek_seq: u64,
    pub position_sec: f64,
    pub duration_sec: f64,
    pub paused: bool,
    pub buffering: bool,
    pub ready: bool,
    pub ended: bool,
    pub error: bool,
    pub scale_mode: u8,
}

pub fn native_open(
    url: &str,
    start_sec: f64,
    rate: f32,
    audio_idx: Option<i32>,
    sub_idx: Option<i32>,
    autoplay: bool,
    scale_mode: u8,
) -> u64 {
    let seq = NEXT_NATIVE_SEQ.fetch_add(1, Ordering::Relaxed);
    let url = url.to_owned();
    let audio = audio_idx.unwrap_or(-1);
    let sub = sub_idx.unwrap_or(-1);
    on_activity("native open", move |env, activity| {
        let jurl = env.new_string(&url)?;
        env.call_method(
            activity,
            "kinoNativeOpen",
            "(JLjava/lang/String;DFIIZI)V",
            &[
                JValue::Long(seq as i64),
                (&jurl).into(),
                JValue::Double(start_sec),
                JValue::Float(rate),
                JValue::Int(audio),
                JValue::Int(sub),
                JValue::Bool(u8::from(autoplay)),
                JValue::Int(i32::from(scale_mode)),
            ],
        )?;
        Ok(())
    });
    seq
}

pub fn native_play() {
    on_activity("native play", |env, activity| {
        env.call_method(activity, "kinoNativePlay", "()V", &[])?;
        Ok(())
    });
}

pub fn native_pause() {
    on_activity("native pause", |env, activity| {
        env.call_method(activity, "kinoNativePause", "()V", &[])?;
        Ok(())
    });
}

pub fn native_seek(sec: f64) {
    on_activity("native seek", move |env, activity| {
        env.call_method(activity, "kinoNativeSeek", "(D)V", &[JValue::Double(sec)])?;
        Ok(())
    });
}

pub fn native_set_rate(rate: f32) {
    on_activity("native rate", move |env, activity| {
        env.call_method(activity, "kinoNativeSetRate", "(F)V", &[JValue::Float(rate)])?;
        Ok(())
    });
}

pub fn native_set_volume(vol: f32) {
    on_activity("native volume", move |env, activity| {
        env.call_method(activity, "kinoNativeSetVolume", "(F)V", &[JValue::Float(vol)])?;
        Ok(())
    });
}

pub fn native_set_muted(muted: bool) {
    on_activity("native muted", move |env, activity| {
        env.call_method(activity, "kinoNativeSetMuted", "(Z)V", &[JValue::Bool(u8::from(muted))])?;
        Ok(())
    });
}

pub fn native_select_audio(audio_idx: Option<i32>) {
    let idx = audio_idx.unwrap_or(-1);
    on_activity("native audio", move |env, activity| {
        env.call_method(activity, "kinoNativeSelectAudio", "(I)V", &[JValue::Int(idx)])?;
        Ok(())
    });
}

pub fn native_select_subtitle(sub_idx: Option<i32>) {
    let idx = sub_idx.unwrap_or(-1);
    on_activity("native subtitle", move |env, activity| {
        env.call_method(activity, "kinoNativeSelectSubtitle", "(I)V", &[JValue::Int(idx)])?;
        Ok(())
    });
}

pub fn native_set_scale(mode: u8) {
    on_activity("native scale", move |env, activity| {
        env.call_method(activity, "kinoNativeSetScale", "(I)V", &[JValue::Int(i32::from(mode))])?;
        Ok(())
    });
}

pub fn native_close() {
    let seq = NEXT_NATIVE_SEQ.fetch_add(1, Ordering::Relaxed);
    on_activity("native close", move |env, activity| {
        env.call_method(activity, "kinoNativeClose", "(J)V", &[JValue::Long(seq as i64)])?;
        Ok(())
    });
}

pub fn poll_native_player() -> Option<NativePlayerSnapshot> {
    let vm = JAVA_VM.get()?;
    let activity = ACTIVITY.read().unwrap_or_else(|e| e.into_inner()).clone()?;
    let mut env = vm.attach_current_thread_as_daemon().ok()?;
    let raw: String = match env.with_local_frame(16, |env| -> jni::errors::Result<Option<String>> {
        let obj = env
            .call_method(activity.as_obj(), "kinoNativePoll", "()Ljava/lang/String;", &[])?
            .l()?;
        if obj.is_null() {
            return Ok(None);
        }
        let jstr = JString::from(obj);
        let text: String = env.get_string(&jstr)?.into();
        Ok(Some(text))
    }) {
        Ok(Some(s)) => s,
        Ok(None) => return None,
        Err(_) => {
            let _ = env.exception_clear();
            return None;
        }
    };
    let mut p = raw.split('|');
    let open_seq = p.next()?.parse().ok()?;
    let seek_seq = p.next()?.parse().ok()?;
    let position_sec = p.next()?.parse().unwrap_or(0.0);
    let duration_raw: f64 = p.next()?.parse().unwrap_or(f64::NAN);
    let duration_sec = if duration_raw.is_finite() && duration_raw > 0.0 {
        duration_raw
    } else {
        f64::NAN
    };
    let flag = |s: Option<&str>| s == Some("1");
    Some(NativePlayerSnapshot {
        open_seq,
        seek_seq,
        position_sec,
        duration_sec,
        paused: flag(p.next()),
        buffering: flag(p.next()),
        ready: flag(p.next()),
        ended: flag(p.next()),
        error: flag(p.next()),
        scale_mode: p.next().and_then(|s| s.parse().ok()).unwrap_or(1),
    })
}
