//! Android-only glue (JNI).

use std::sync::atomic::{AtomicBool, Ordering};

use dioxus::mobile::wry::prelude::dispatch;
use jni::{objects::JValue, JNIEnv};

static PLAYER_ACTIVE: AtomicBool = AtomicBool::new(false);

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
