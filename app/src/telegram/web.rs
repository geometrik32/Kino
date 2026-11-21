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

/// Palette colours of the app, used where Telegram's own colours cannot be.
const DARK_BG: &str = "#0E1621";
const LIGHT_BG: &str = "#F7F2E8";
const DARK_ACCENT: (&str, &str) = ("#38C6BD", "#0B141B");
const LIGHT_ACCENT: (&str, &str) = ("#0B6B65", "#FAF7F0");

/// True only inside a real Telegram Mini App session (the WebApp JS bridge also loads on the web).
pub fn is_telegram() -> bool {
    init_data().is_some()
}

fn is_light_now() -> bool {
    let override_theme = web_sys::window()
        .and_then(|w| w.document())
        .and_then(|d| d.document_element())
        .and_then(|r| r.get_attribute("data-theme"));
    match override_theme.as_deref() {
        Some("light") => true,
        Some("dark") => false,
        _ => web_app()
            .and_then(|w| get(&w, "colorScheme"))
            .and_then(|v| v.as_string())
            .is_some_and(|s| s == "light"),
    }
}

/// Makes the page behave like part of Telegram: full height, no swipe-to-close while
/// scrolling, native bars in the page colour, and CSS classes (`tg`, `tg-dark`/`tg-light`,
/// `tg-<platform>`) that the stylesheet uses. Does nothing in a normal browser.
pub fn init() {
    if !is_telegram() {
        return;
    }
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
pub fn paint_bars() {
    if !is_telegram() {
        return;
    }
    let Some(w) = web_app() else { return };
    let override_theme = web_sys::window()
        .and_then(|w| w.document())
        .and_then(|d| d.document_element())
        .and_then(|r| r.get_attribute("data-theme"));
    let bg = match override_theme.as_deref() {
        Some("dark") => DARK_BG.into(),
        Some("light") => LIGHT_BG.into(),
        _ => {
            let params = get(&w, "themeParams");
            let dark = get(&w, "colorScheme").and_then(|v| v.as_string()).as_deref() != Some("light");
            let from_theme = |key: &str| params.as_ref().and_then(|p| get(p, key)).and_then(|v| v.as_string());
            from_theme("bg_color").unwrap_or_else(|| if dark { DARK_BG.into() } else { "#ffffff".into() })
        }
    };
    for method in ["setHeaderColor", "setBackgroundColor", "setBottomBarColor"] {
        call(&[], method, &[JsValue::from_str(&bg)]);
    }
}

/// Colors Telegram's header, background and bottom chrome black while the player is open.
pub fn paint_player_bars() {
    if !is_telegram() {
        return;
    }
    let black = JsValue::from_str("#000000");
    for method in ["setHeaderColor", "setBackgroundColor", "setBottomBarColor"] {
        call(&[], method, std::slice::from_ref(&black));
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
    static HISTORY_WIRED: Cell<bool> = const { Cell::new(false) };
    static HISTORY_DEPTH: Cell<i32> = const { Cell::new(0) };
    static SKIP_POP: Cell<i32> = const { Cell::new(0) };
    static IN_POPSTATE: Cell<bool> = const { Cell::new(false) };
}

struct PopGuard;

impl Drop for PopGuard {
    fn drop(&mut self) {
        IN_POPSTATE.with(|p| p.set(false));
    }
}

/// Hooks `window.history` (`popstate`) to the app's back stack in regular browsers.
/// Inside Telegram Mini App, Telegram's native `BackButton` owns navigation.
pub fn init_history(has_initial_trail: bool, initial_url: Option<&str>) {
    if is_telegram() || HISTORY_WIRED.with(|w| w.replace(true)) {
        return;
    }
    let Some(w) = web_sys::window() else { return };
    if let Ok(h) = w.history() {
        if has_initial_trail {
            let _ = h.replace_state_with_url(&JsValue::from_f64(0.0), "", Some("/"));
            let _ = h.push_state_with_url(&JsValue::from_f64(1.0), "", initial_url);
            HISTORY_DEPTH.with(|d| d.set(1));
        } else {
            let _ = h.replace_state_with_url(&JsValue::from_f64(0.0), "", None);
            HISTORY_DEPTH.with(|d| d.set(0));
        }
    }
    let on_pop = Closure::<dyn FnMut(web_sys::PopStateEvent)>::new(move |ev: web_sys::PopStateEvent| {
        let target = ev.state().as_f64().unwrap_or(0.0) as i32;
        let skip = SKIP_POP.with(|s| s.get());
        if skip > 0 {
            SKIP_POP.with(|s| s.set(skip - 1));
            HISTORY_DEPTH.with(|d| d.set(target));
            return;
        }
        let cur = HISTORY_DEPTH.with(|d| d.get());
        if target < cur {
            HISTORY_DEPTH.with(|d| d.set(target));
            let top = BACKS.with(|b| b.borrow().last().map(|(_, h)| h.clone()));
            if let Some(h) = top {
                IN_POPSTATE.with(|p| p.set(true));
                let _guard = PopGuard;
                (h.borrow_mut())();
            }
        } else if target > cur {
            if let Some(h) = web_sys::window().and_then(|w| w.history().ok()) {
                SKIP_POP.with(|s| s.set(s.get() + 1));
                let _ = h.back();
            }
        }
    });
    let _ = w.add_event_listener_with_callback("popstate", on_pop.as_ref().unchecked_ref());
    on_pop.forget();
}

/// Records a forward navigation step in browser history.
pub fn push_history(url: Option<&str>) {
    if is_telegram() {
        return;
    }
    let Some(h) = web_sys::window().and_then(|w| w.history().ok()) else {
        return;
    };
    let next = HISTORY_DEPTH.with(|d| {
        let v = d.get() + 1;
        d.set(v);
        v
    });
    let _ = h.push_state_with_url(&JsValue::from_f64(f64::from(next)), "", url);
}

/// Steps browser history back by one entry after a UI Back button click.
pub fn pop_history() {
    if is_telegram() || IN_POPSTATE.with(|p| p.get()) {
        return;
    }
    let cur = HISTORY_DEPTH.with(|d| d.get());
    if cur <= 0 {
        return;
    }
    if let Some(h) = web_sys::window().and_then(|w| w.history().ok()) {
        HISTORY_DEPTH.with(|d| d.set(cur - 1));
        SKIP_POP.with(|s| s.set(s.get() + 1));
        let _ = h.back();
    }
}

/// Returns browser history to the root (`Home`) entry.
pub fn reset_history() {
    if is_telegram() || IN_POPSTATE.with(|p| p.get()) {
        return;
    }
    let cur = HISTORY_DEPTH.with(|d| d.get());
    if cur <= 0 {
        return;
    }
    if let Some(h) = web_sys::window().and_then(|w| w.history().ok()) {
        HISTORY_DEPTH.with(|d| d.set(0));
        SKIP_POP.with(|s| s.set(s.get() + 1));
        let _ = h.go_with_delta(-cur);
    }
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
    let (accent, fg) = if is_light_now() { LIGHT_ACCENT } else { DARK_ACCENT };
    let params = js_sys::Object::new();
    let _ = Reflect::set(&params, &"text".into(), &text.into());
    let _ = Reflect::set(&params, &"color".into(), &accent.into());
    let _ = Reflect::set(&params, &"text_color".into(), &fg.into());
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

/// A title to open at start: `?open=tv_1396` (with optional `&play=1`) or `startapp=tv_1396`.
pub fn open_target() -> Option<super::OpenTarget> {
    let q = query();
    let start = start_param();
    super::parse_open_target(
        q.get("open").map(String::as_str),
        q.get("play").map(String::as_str),
        start.as_deref(),
    )
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

/// One-time sign-in code from `?login=<code>`: the bot's `/login` link.
pub fn login_token() -> Option<String> {
    query()
        .get("login")
        .cloned()
        .filter(|t| !t.is_empty() && t.chars().all(|c| c.is_ascii_alphanumeric()))
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
