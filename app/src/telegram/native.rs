//! Stand-ins for the apps: there is no Telegram around them.

use std::{
    cell::RefCell,
    collections::BTreeMap,
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
};

type BackHandler = Box<dyn FnMut()>;

static NEXT_BACK_ID: AtomicU64 = AtomicU64::new(1);
static BACK_BRIDGE_STARTED: AtomicBool = AtomicBool::new(false);

thread_local! {
    static BACK_STACK: RefCell<Vec<(u64, BackHandler)>> = RefCell::new(Vec::new());
}

fn sync_android_back_enabled(enabled: bool) {
    #[cfg(target_os = "android")]
    crate::platform::android::set_back_enabled(enabled);
    #[cfg(not(target_os = "android"))]
    let _ = enabled;
}

fn ensure_back_bridge() {
    #[cfg(target_os = "android")]
    if !BACK_BRIDGE_STARTED.swap(true, Ordering::AcqRel) {
        dioxus::core::spawn_forever(async {
            let mut bridge = dioxus::document::eval("window.__kinoNativeBack = function() { dioxus.send(1); };");
            while bridge.recv::<i32>().await.is_ok() {
                let top = BACK_STACK.with(|s| s.borrow_mut().pop());
                if let Some((id, mut cb)) = top {
                    let has_more = BACK_STACK.with(|s| !s.borrow().is_empty());
                    sync_android_back_enabled(has_more);
                    cb();
                    // Re-check in case the handler didn't pop itself and wants to remain active.
                    let _ = id;
                } else {
                    sync_android_back_enabled(false);
                }
            }
        });
    }
    #[cfg(not(target_os = "android"))]
    let _ = &BACK_BRIDGE_STARTED;
}

pub fn init_data() -> Option<String> {
    None
}
pub fn is_telegram() -> bool {
    false
}
pub fn init() {}
pub fn paint_bars() {}
pub fn paint_player_bars() {}
pub fn push_back(handler: impl FnMut() + 'static) -> u64 {
    ensure_back_bridge();
    let id = NEXT_BACK_ID.fetch_add(1, Ordering::Relaxed);
    BACK_STACK.with(|s| s.borrow_mut().push((id, Box::new(handler))));
    sync_android_back_enabled(true);
    id
}
pub fn pop_back(id: u64) {
    let has_more = BACK_STACK.with(|s| {
        let mut stack = s.borrow_mut();
        if let Some(pos) = stack.iter().rposition(|(entry_id, _)| *entry_id == id) {
            let _ = stack.remove(pos);
        }
        !stack.is_empty()
    });
    sync_android_back_enabled(has_more);
}
pub fn init_history(_has_initial_trail: bool, _initial_url: Option<&str>) {}
pub fn push_history(_url: Option<&str>) {}
pub fn pop_history() {}
pub fn reset_history() {}
pub fn in_popstate() -> bool {
    false
}
pub fn show_main_button(_text: &str, _on_click: impl FnMut() + 'static) {}
pub fn hide_main_button() {}

#[derive(Clone, Copy)]
pub enum Haptic {
    Tap,
    Select,
    Success,
}

pub fn haptic(_kind: Haptic) {}
pub fn set_fullscreen(_on: bool) -> bool {
    false
}
pub fn is_mobile() -> bool {
    cfg!(any(target_os = "android", target_os = "ios"))
}
pub fn is_fullscreen() -> bool {
    false
}
pub fn open_target() -> Option<super::OpenTarget> {
    None
}
pub fn invite() -> Option<String> {
    None
}
pub fn login_token() -> Option<String> {
    None
}
pub fn widget_fields() -> Option<BTreeMap<String, String>> {
    None
}
pub fn clean_url() {}
