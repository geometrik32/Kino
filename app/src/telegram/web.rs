//! Browser and Telegram glue: Mini App `initData`, URL query, Login Widget.

use std::{
    cell::{Cell, RefCell},
    collections::BTreeMap,
    rc::Rc,
};

use js_sys::Reflect;
use wasm_bindgen::{closure::Closure, JsCast, JsValue};

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

/// Property chain on `Telegram.WebApp`, e.g. `["BackButton"]`.
fn node(path: &[&str]) -> Option<JsValue> {
    path.iter().try_fold(web_app()?, |obj, key| get(&obj, key))
}

/// Calls `method` on `Telegram.WebApp.<path>`; clients that lack the method are skipped.
/// (The SDK only logs for features older clients do not have, so nothing needs a version check.)
fn call(path: &[&str], method: &str, args: &[JsValue]) {
    let Some(target) = node(path) else { return };
    let Some(f) = get(&target, method).and_then(|f| f.dyn_into::<js_sys::Function>().ok()) else {
        return;
    };
    let args: js_sys::Array = args.iter().collect();
    let _ = f.apply(&target, &args);
}

/// Dark palette of the app, used where Telegram's own colours cannot be.
const APP_BG: &str = "#0d0f14";

/// Makes the page behave like part of Telegram: full height, no swipe-to-close while
/// scrolling, native bars in the page colour, and CSS classes (`tg`, `tg-dark`/`tg-light`,
/// `tg-<platform>`) that the stylesheet uses. Does nothing in a normal browser.
pub fn init() {
    let Some(w) = web_app() else { return };
    let Some(root) = web_sys::window()
        .and_then(|w| w.document())
        .and_then(|d| d.document_element())
    else {
        return;
    };
    let scheme = get(&w, "colorScheme")
        .and_then(|v| v.as_string())
        .unwrap_or_else(|| "dark".into());
    let platform = get(&w, "platform").and_then(|v| v.as_string()).unwrap_or_default();
    let classes = root.class_list();
    let _ = classes.add_1("tg");
    let _ = classes.add_1(if scheme == "light" { "tg-light" } else { "tg-dark" });
    if !platform.is_empty() {
        let _ = classes.add_1(&format!("tg-{platform}"));
    }
    cover_notch();
    call(&[], "ready", &[]);
    call(&[], "expand", &[]);
    call(&[], "disableVerticalSwipes", &[]);
    paint_bars();
    // The user can change the Telegram theme while the app is open.
    let on_theme = Closure::<dyn FnMut()>::new(move || {
        if let Some(root) = web_sys::window()
            .and_then(|w| w.document())
            .and_then(|d| d.document_element())
        {
            let light = web_app()
                .and_then(|w| get(&w, "colorScheme"))
                .and_then(|v| v.as_string())
                .is_some_and(|s| s == "light");
            let classes = root.class_list();
            let _ = classes.remove_1(if light { "tg-dark" } else { "tg-light" });
            let _ = classes.add_1(if light { "tg-light" } else { "tg-dark" });
        }
        paint_bars();
    });
    call(&[], "onEvent", &["themeChanged".into(), on_theme.as_ref().clone()]);
    on_theme.forget();
}

/// Lets the page use the whole screen (`viewport-fit=cover`), so `env(safe-area-inset-*)`
/// reports the notch and the home bar and the stylesheet can keep clear of them.
fn cover_notch() {
    let Some(meta) = web_sys::window()
        .and_then(|w| w.document())
        .and_then(|d| d.query_selector("meta[name=viewport]").ok().flatten())
    else {
        return;
    };
    let content = meta.get_attribute("content").unwrap_or_default();
    if !content.contains("viewport-fit") {
        let _ = meta.set_attribute("content", &format!("{content}, viewport-fit=cover"));
    }
}

/// Header, background and bottom bar of Telegram in the colour of the page.
fn paint_bars() {
    let Some(w) = web_app() else { return };
    let params = get(&w, "themeParams");
    let dark = get(&w, "colorScheme").and_then(|v| v.as_string()).as_deref() != Some("light");
    let from_theme = |key: &str| params.as_ref().and_then(|p| get(p, key)).and_then(|v| v.as_string());
    let bg = from_theme("bg_color").unwrap_or_else(|| if dark { APP_BG.into() } else { "#ffffff".into() });
    for method in ["setHeaderColor", "setBackgroundColor", "setBottomBarColor"] {
        call(&[], method, &[JsValue::from_str(&bg)]);
    }
}

// --- Back button -----------------------------------------------------------

type Handler = Rc<RefCell<dyn FnMut()>>;

thread_local! {
    /// Screens that want the Back button, newest last; the newest one gets the click.
    static BACKS: RefCell<Vec<(u64, Handler)>> = const { RefCell::new(Vec::new()) };
    static NEXT_ID: Cell<u64> = const { Cell::new(1) };
    static BACK_WIRED: Cell<bool> = const { Cell::new(false) };
    static MAIN: RefCell<Option<Closure<dyn FnMut()>>> = const { RefCell::new(None) };
}

/// Takes over the native Back button until [`pop_back`]. Returns the id to give back.
/// Outside Telegram it only records the handler.
pub fn push_back(handler: impl FnMut() + 'static) -> u64 {
    let id = NEXT_ID.with(|n| {
        let id = n.get();
        n.set(id + 1);
        id
    });
    BACKS.with(|b| b.borrow_mut().push((id, Rc::new(RefCell::new(handler)))));
    sync_back();
    id
}

/// Lets go of the Back button taken with [`push_back`].
pub fn pop_back(id: u64) {
    BACKS.with(|b| b.borrow_mut().retain(|(i, _)| *i != id));
    sync_back();
}

fn sync_back() {
    if !BACK_WIRED.with(|w| w.replace(true)) {
        let click = Closure::<dyn FnMut()>::new(|| {
            let top = BACKS.with(|b| b.borrow().last().map(|(_, h)| h.clone()));
            if let Some(h) = top {
                (h.borrow_mut())();
            }
        });
        call(&["BackButton"], "onClick", &[click.as_ref().clone()]);
        click.forget();
    }
    let any = BACKS.with(|b| !b.borrow().is_empty());
    call(&["BackButton"], if any { "show" } else { "hide" }, &[]);
}

// --- Main button -----------------------------------------------------------

/// Shows the big native button at the bottom with `text`; `on_click` runs when it is pressed.
/// Calling it again replaces both. Outside Telegram nothing is shown.
pub fn show_main_button(text: &str, on_click: impl FnMut() + 'static) {
    let Some(button) = node(&["MainButton"]) else { return };
    hide_main_button();
    let closure = Closure::<dyn FnMut()>::new(on_click);
    call(&["MainButton"], "onClick", &[closure.as_ref().clone()]);
    MAIN.with(|m| *m.borrow_mut() = Some(closure));
    let params = js_sys::Object::new();
    let _ = Reflect::set(&params, &"text".into(), &text.into());
    let _ = Reflect::set(&params, &"color".into(), &ACCENT.into());
    let _ = Reflect::set(&params, &"text_color".into(), &"#ffffff".into());
    let _ = Reflect::set(&params, &"is_visible".into(), &true.into());
    let _ = Reflect::set(&params, &"is_active".into(), &true.into());
    set_root_class("tg-main", true);
    if let Some(f) = get(&button, "setParams").and_then(|f| f.dyn_into::<js_sys::Function>().ok()) {
        let _ = f.call1(&button, &params);
    }
}

/// Hides the native main button and forgets its handler.
pub fn hide_main_button() {
    if let Some(closure) = MAIN.with(|m| m.borrow_mut().take()) {
        call(&["MainButton"], "offClick", &[closure.as_ref().clone()]);
    }
    call(&["MainButton"], "hide", &[]);
    set_root_class("tg-main", false);
}

/// Adds or removes a class on `<html>`; the stylesheet reacts to it.
fn set_root_class(name: &str, on: bool) {
    if let Some(root) = web_sys::window()
        .and_then(|w| w.document())
        .and_then(|d| d.document_element())
    {
        let classes = root.class_list();
        let _ = if on {
            classes.add_1(name)
        } else {
            classes.remove_1(name)
        };
    }
}

/// Brand red, as in the stylesheet.
const ACCENT: &str = "#e5484d";

// --- Haptics -----------------------------------------------------------------

#[derive(Clone, Copy)]
pub enum Haptic {
    /// A tap on a tab or a button.
    Tap,
    /// A choice changed (status, rating, track).
    Select,
    /// Something started or finished well.
    Success,
}

/// A short vibration on phones; nothing elsewhere.
pub fn haptic(kind: Haptic) {
    let path = ["HapticFeedback"];
    match kind {
        Haptic::Tap => call(&path, "impactOccurred", &["light".into()]),
        Haptic::Select => call(&path, "selectionChanged", &[]),
        Haptic::Success => call(&path, "notificationOccurred", &["success".into()]),
    }
}

// --- Full screen for the player ----------------------------------------------

/// Whether Telegram can show the app full screen (Bot API 8.0+).
pub fn can_fullscreen() -> bool {
    let Some(w) = web_app() else { return false };
    get(&w, "isVersionAtLeast")
        .and_then(|f| f.dyn_into::<js_sys::Function>().ok())
        .and_then(|f| f.call1(&w, &"8.0".into()).ok())
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}

/// Enters or leaves Telegram's full screen; true if it handled the request.
pub fn set_fullscreen(on: bool) -> bool {
    if !can_fullscreen() {
        return false;
    }
    call(&[], if on { "requestFullscreen" } else { "exitFullscreen" }, &[]);
    true
}

/// Telegram on a phone or tablet (the player then fills the whole screen by itself).
pub fn is_mobile() -> bool {
    web_app()
        .and_then(|w| get(&w, "platform"))
        .and_then(|v| v.as_string())
        .is_some_and(|p| p == "android" || p == "ios")
}

/// Whether Telegram is showing the app full screen now.
pub fn is_fullscreen() -> bool {
    web_app()
        .and_then(|w| get(&w, "isFullscreen"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}

// --- Deep links ----------------------------------------------------------------

/// A title to open at start: `?open=tv_1396` or `startapp=tv_1396` (movie_<id> likewise).
pub fn open_target() -> Option<(kino_core::MediaKind, i64)> {
    let raw = query().get("open").cloned().or_else(start_param)?;
    let (kind, id) = raw.split_once('_')?;
    let kind = match kind {
        "movie" => kino_core::MediaKind::Movie,
        "tv" => kino_core::MediaKind::Tv,
        _ => return None,
    };
    Some((kind, id.parse().ok()?))
}

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
