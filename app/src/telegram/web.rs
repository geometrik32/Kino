//! Browser and Telegram glue: Mini App `initData`, URL query, Login Widget.

use std::collections::BTreeMap;

use js_sys::Reflect;
use wasm_bindgen::JsValue;

fn get(obj: &JsValue, key: &str) -> Option<JsValue> {
    Reflect::get(obj, &JsValue::from_str(key))
        .ok()
        .filter(|v| !v.is_undefined() && !v.is_null())
}

/// `Telegram.WebApp`, when running inside Telegram.
fn web_app() -> Option<JsValue> {
    let window: JsValue = web_sys::window()?.into();
    get(&get(&window, "Telegram")?, "WebApp")
}

/// Signed `initData`; empty outside Telegram.
pub fn init_data() -> Option<String> {
    web_app()
        .and_then(|w| get(&w, "initData"))
        .and_then(|v| v.as_string())
        .filter(|s| !s.is_empty())
}

/// `start_param` of the Mini App link (used to carry invites as `inv_<code>`).
pub fn start_param() -> Option<String> {
    let unsafe_data = get(&web_app()?, "initDataUnsafe")?;
    get(&unsafe_data, "start_param")?.as_string()
}

/// Tells Telegram the app is ready and expands it to full height.
pub fn ready() {
    if let Some(w) = web_app() {
        for method in ["ready", "expand"] {
            if let Some(f) = get(&w, method).and_then(|f| f.dyn_into::<js_sys::Function>().ok()) {
                let _ = f.call0(&w);
            }
        }
    }
}

use wasm_bindgen::JsCast;

/// All query parameters of the current page.
pub fn query() -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let Some(search) = web_sys::window().and_then(|w| w.location().search().ok()) else {
        return out;
    };
    let Ok(params) = web_sys::UrlSearchParams::new_with_str(&search) else {
        return out;
    };
    let entries = js_sys::try_iter(&params.into()).ok().flatten();
    for entry in entries.into_iter().flatten().flatten() {
        let pair: js_sys::Array = entry.unchecked_into();
        if let (Some(k), Some(v)) = (pair.get(0).as_string(), pair.get(1).as_string()) {
            out.insert(k, v);
        }
    }
    out
}

/// Invite code from `?invite=` or the Mini App start parameter.
pub fn invite() -> Option<String> {
    query()
        .get("invite")
        .cloned()
        .or_else(|| start_param().and_then(|p| p.strip_prefix("inv_").map(str::to_owned)))
}

/// Login Widget fields if Telegram just redirected back here with them.
pub fn widget_fields() -> Option<BTreeMap<String, String>> {
    const WIDGET_KEYS: [&str; 7] = [
        "id",
        "first_name",
        "last_name",
        "username",
        "photo_url",
        "auth_date",
        "hash",
    ];
    let q = query();
    if !q.contains_key("hash") || !q.contains_key("id") {
        return None;
    }
    Some(
        q.into_iter()
            .filter(|(k, _)| WIDGET_KEYS.contains(&k.as_str()))
            .collect(),
    )
}

/// Removes login fields from the address bar after use.
pub fn clean_url() {
    if let Some(w) = web_sys::window() {
        if let (Ok(history), Ok(path)) = (w.history(), w.location().pathname()) {
            let _ = history.replace_state_with_url(&JsValue::NULL, "", Some(&path));
        }
    }
}

/// Inserts the Telegram Login Widget into the element with `container_id`.
/// After login Telegram redirects back to this page with the signed fields.
pub fn mount_login_widget(container_id: &str, bot_username: &str) {
    let Some(doc) = web_sys::window().and_then(|w| w.document()) else {
        return;
    };
    let Some(container) = doc.get_element_by_id(container_id) else {
        return;
    };
    if container.child_element_count() > 0 {
        return;
    }
    let Ok(script) = doc.create_element("script") else {
        return;
    };
    let here = web_sys::window()
        .and_then(|w| w.location().href().ok())
        .unwrap_or_default();
    for (k, v) in [
        ("src", "https://telegram.org/js/telegram-widget.js?22"),
        ("data-telegram-login", bot_username),
        ("data-size", "large"),
        ("data-auth-url", here.as_str()),
        ("data-request-access", "write"),
    ] {
        let _ = script.set_attribute(k, v);
    }
    let _ = script.set_attribute("async", "");
    let _ = container.append_child(&script);
}
