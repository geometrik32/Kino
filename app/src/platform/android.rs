//! Android-only glue (JNI).

use dioxus::mobile::wry::prelude::dispatch;

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
