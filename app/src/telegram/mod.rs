//! Browser and Telegram glue: Mini App `initData`, URL query, Login Widget.
//! Only the website has these; the apps log in through the bot (see `login_link`).

#[cfg(target_arch = "wasm32")]
mod web;
#[cfg(target_arch = "wasm32")]
pub use web::*;

#[cfg(not(target_arch = "wasm32"))]
mod native {
    use std::collections::BTreeMap;

    pub fn init_data() -> Option<String> {
        None
    }
    pub fn ready() {}
    pub fn invite() -> Option<String> {
        None
    }
    pub fn widget_fields() -> Option<BTreeMap<String, String>> {
        None
    }
    pub fn clean_url() {}
    pub fn mount_login_widget(_container_id: &str, _bot_username: &str) {}
}
#[cfg(not(target_arch = "wasm32"))]
pub use native::*;
