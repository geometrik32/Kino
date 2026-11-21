//! Android-only glue (JNI) and ExoPlayer bridge.

use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    OnceLock,
};

use dioxus::mobile::wry::prelude::dispatch;
use jni::{
    objects::{GlobalRef, JString, JValue},
    JNIEnv, JavaVM,
};

static PLAYER_ACTIVE: AtomicBool = AtomicBool::new(false);
static NEXT_NATIVE_SEQ: AtomicU64 = AtomicU64::new(1);
static JAVA_VM: OnceLock<JavaVM> = OnceLock::new();
static ACTIVITY: OnceLock<GlobalRef> = OnceLock::new();

/// Opens a link with the system: `https://t.me/...` goes to the Telegram app when it is installed.
pub fn open_url(url: &str) -> Result<(), String> {
    let url = url.to_owned();
    // The closure runs on the Android UI thread, where the activity can start another one.
    dispatch(move |env, activity, _webview| {
        let result: jni::errors::Result<()> = (|| {
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
        })();
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
fn on_activity(
    what: &'static str,
    f: impl FnOnce(&mut JNIEnv, &jni::objects::JObject) -> jni::errors::Result<()> + Send + 'static,
) {
    dispatch(move |env, activity, _webview| {
        if JAVA_VM.get().is_none() {
            if let Ok(vm) = env.get_java_vm() {
                let _ = JAVA_VM.set(vm);
            }
        }
        if ACTIVITY.get().is_none() {
            if let Ok(g) = env.new_global_ref(activity) {
                let _ = ACTIVITY.set(g);
            }
        }
        if let Err(e) = f(env, activity) {
            let _ = env.exception_clear();
            eprintln!("{what}: {e}");
        }
    });
}

/// Dark or light status and navigation bars, matching the active theme.
pub fn style_bars(light: bool) {
    let color = if light { LIGHT_BAR_COLOR } else { DARK_BAR_COLOR };
    // SYSTEM_UI_FLAG_LIGHT_STATUS_BAR (0x2000) | SYSTEM_UI_FLAG_LIGHT_NAVIGATION_BAR (0x10).
    let flags: i32 = if light { 0x2000 | 0x10 } else { 0 };
    on_activity("style bars", move |env, activity| {
        let window = env
            .call_method(activity, "getWindow", "()Landroid/view/Window;", &[])?
            .l()?;
        env.call_method(&window, "setStatusBarColor", "(I)V", &[JValue::Int(color)])?;
        env.call_method(&window, "setNavigationBarColor", "(I)V", &[JValue::Int(color)])?;
        let decor = env
            .call_method(&window, "getDecorView", "()Landroid/view/View;", &[])?
            .l()?;
        if !PLAYER_ACTIVE.load(Ordering::Relaxed) {
            env.call_method(&decor, "setSystemUiVisibility", "(I)V", &[JValue::Int(flags)])?;
        }
        Ok(())
    });
}

/// Landscape and hidden system bars while a film plays; back to normal afterwards.
pub fn player_mode(on: bool) {
    PLAYER_ACTIVE.store(on, Ordering::Relaxed);
    on_activity("player mode", move |env, activity| {
        // SCREEN_ORIENTATION_SENSOR_LANDSCAPE = 6, SCREEN_ORIENTATION_UNSPECIFIED = -1.
        let orientation = if on { 6 } else { -1 };
        env.call_method(activity, "setRequestedOrientation", "(I)V", &[JValue::Int(orientation)])?;
        let window = env
            .call_method(activity, "getWindow", "()Landroid/view/Window;", &[])?
            .l()?;
        let sdk = env
            .get_static_field("android/os/Build$VERSION", "SDK_INT", "I")
            .and_then(|v| v.i())
            .unwrap_or(0);
        if sdk >= 28 {
            if let Ok(attrs) = env
                .call_method(
                    &window,
                    "getAttributes",
                    "()Landroid/view/WindowManager$LayoutParams;",
                    &[],
                )
                .and_then(|v| v.l())
            {
                // LAYOUT_IN_DISPLAY_CUTOUT_MODE_SHORT_EDGES = 1, DEFAULT = 0.
                let cutout = if on { 1 } else { 0 };
                let _ = env.set_field(&attrs, "layoutInDisplayCutoutMode", "I", JValue::Int(cutout));
                let _ = env.call_method(
                    &window,
                    "setAttributes",
                    "(Landroid/view/WindowManager$LayoutParams;)V",
                    &[(&attrs).into()],
                );
            }
            let _ = env.exception_clear();
        }
        let decor = env
            .call_method(&window, "getDecorView", "()Landroid/view/View;", &[])?
            .l()?;
        // IMMERSIVE_STICKY | HIDE_NAVIGATION | FULLSCREEN | LAYOUT_*: the bars slide in on a swipe and leave again.
        let flags: i32 = if on {
            0x1000 | 0x2 | 0x4 | 0x100 | 0x200 | 0x400
        } else {
            0
        };
        env.call_method(&decor, "setSystemUiVisibility", "(I)V", &[JValue::Int(flags)])?;
        if sdk >= 30 {
            let _ = env.call_method(
                &window,
                "setDecorFitsSystemWindows",
                "(Z)V",
                &[JValue::Bool(u8::from(!on))],
            );
            if let Ok(ctrl) = env
                .call_method(
                    &window,
                    "getInsetsController",
                    "()Landroid/view/WindowInsetsController;",
                    &[],
                )
                .and_then(|v| v.l())
            {
                if !ctrl.is_null() {
                    let status = env
                        .call_static_method("android/view/WindowInsets$Type", "statusBars", "()I", &[])
                        .and_then(|v| v.i())
                        .unwrap_or(1);
                    let nav = env
                        .call_static_method("android/view/WindowInsets$Type", "navigationBars", "()I", &[])
                        .and_then(|v| v.i())
                        .unwrap_or(2);
                    let types = status | nav;
                    if on {
                        // BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE = 2.
                        let _ = env.call_method(&ctrl, "setSystemBarsBehavior", "(I)V", &[JValue::Int(2)]);
                        let _ = env.call_method(&ctrl, "hide", "(I)V", &[JValue::Int(types)]);
                    } else {
                        let _ = env.call_method(&ctrl, "show", "(I)V", &[JValue::Int(types)]);
                    }
                }
            }
            let _ = env.exception_clear();
        }
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
}

pub fn native_open(
    url: &str,
    start_sec: f64,
    rate: f32,
    audio_idx: Option<i32>,
    sub_idx: Option<i32>,
    autoplay: bool,
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
            "(JLjava/lang/String;DFIIZ)V",
            &[
                JValue::Long(seq as i64),
                (&jurl).into(),
                JValue::Double(start_sec),
                JValue::Float(rate),
                JValue::Int(audio),
                JValue::Int(sub),
                JValue::Bool(u8::from(autoplay)),
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

pub fn native_close() {
    let seq = NEXT_NATIVE_SEQ.fetch_add(1, Ordering::Relaxed);
    on_activity("native close", move |env, activity| {
        env.call_method(activity, "kinoNativeClose", "(J)V", &[JValue::Long(seq as i64)])?;
        Ok(())
    });
}

pub fn poll_native_player() -> Option<NativePlayerSnapshot> {
    let vm = JAVA_VM.get()?;
    let activity = ACTIVITY.get()?;
    let mut env = vm.attach_current_thread().ok()?;
    let val = match env.call_method(activity.as_obj(), "kinoNativePoll", "()Ljava/lang/String;", &[]) {
        Ok(v) => v,
        Err(_) => {
            let _ = env.exception_clear();
            return None;
        }
    };
    let obj = match val.l() {
        Ok(o) => o,
        Err(_) => {
            let _ = env.exception_clear();
            return None;
        }
    };
    if obj.is_null() {
        return None;
    }
    let jstr = JString::from(obj);
    let raw: String = match env.get_string(&jstr) {
        Ok(s) => s.into(),
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
    })
}
