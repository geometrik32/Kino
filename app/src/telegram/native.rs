//! Stand-ins for the apps: there is no Telegram around them.

use std::collections::BTreeMap;

pub fn init_data() -> Option<String> {
    None
}
pub fn init() {}
pub fn push_back(_handler: impl FnMut() + 'static) -> u64 {
    0
}
pub fn pop_back(_id: u64) {}
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
// Only the <video> player asks (the desktop app plays through libmpv).
#[cfg_attr(feature = "desktop", allow(dead_code))]
pub fn is_mobile() -> bool {
    cfg!(any(target_os = "android", target_os = "ios"))
}
pub fn is_fullscreen() -> bool {
    false
}
pub fn open_target() -> Option<(kino_core::MediaKind, i64)> {
    None
}
pub fn invite() -> Option<String> {
    None
}
pub fn widget_fields() -> Option<BTreeMap<String, String>> {
    None
}
pub fn clean_url() {}
