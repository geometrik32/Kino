//! Unified monoline SVG icon set (1.85px stroke) so icons render identically
//! across Web, Windows, Android and Telegram Mini App without emoji fallback.

#![allow(non_snake_case)]

use dioxus::prelude::*;

pub const HOME: &str = r#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.85" stroke-linecap="round" stroke-linejoin="round"><path d="M3 10.5 12 3l9 7.5"/><path d="M5.5 9.5V20a1 1 0 0 0 1 1h11a1 1 0 0 0 1-1V9.5"/></svg>"#;
pub const SEARCH: &str = r#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linecap="round" stroke-linejoin="round"><circle cx="11" cy="11" r="7"/><path d="m20 20-3.8-3.8"/></svg>"#;
pub const BOOKMARK: &str = r#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.85" stroke-linecap="round" stroke-linejoin="round"><path d="M6 4h12a1 1 0 0 1 1 1v16l-7-4-7 4V5a1 1 0 0 1 1-1z"/></svg>"#;
pub const PLAY: &str = r#"<svg viewBox="0 0 24 24" fill="currentColor"><path d="M6.2 5.2v13.6a1 1 0 0 0 1.53.85l10.6-6.8a1 1 0 0 0 0-1.7L7.73 4.35A1 1 0 0 0 6.2 5.2z"/></svg>"#;
pub const PAUSE: &str = r#"<svg viewBox="0 0 24 24" fill="currentColor"><rect x="6" y="4.5" width="4.2" height="15" rx="1.2"/><rect x="13.8" y="4.5" width="4.2" height="15" rx="1.2"/></svg>"#;
pub const BACK: &str = r#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="m15 18-6-6 6-6"/></svg>"#;
pub const CLOSE: &str = r#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"><path d="M18 6 6 18M6 6l12 12"/></svg>"#;
pub const SWAP: &str = r#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.85" stroke-linecap="round" stroke-linejoin="round"><path d="m16 3 4 4-4 4"/><path d="M20 7H4"/><path d="m8 21-4-4 4-4"/><path d="M4 17h16"/></svg>"#;
pub const FILTER: &str = r#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.85" stroke-linecap="round" stroke-linejoin="round"><path d="M3 6h18M6 12h12M10 18h4"/></svg>"#;
pub const SORT: &str = r#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.85" stroke-linecap="round" stroke-linejoin="round"><path d="m7 15 5 5 5-5"/><path d="m7 9 5-5 5 5"/></svg>"#;
pub const DOWNLOAD: &str = r#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linecap="round" stroke-linejoin="round"><path d="M12 3v12m0 0-4.5-4.5M12 15l4.5-4.5"/><path d="M4 19h16"/></svg>"#;
pub const BOLT: &str = r#"<svg viewBox="0 0 24 24" fill="currentColor"><path d="M13.5 2 4.5 13.5h6.5L9.8 22 19.5 10.5H13L13.5 2z"/></svg>"#;
pub const CHECK: &str = r#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.3" stroke-linecap="round" stroke-linejoin="round"><path d="m5 12.5 4.5 4.5L19 7.5"/></svg>"#;
pub const INFO: &str = r#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="9"/><path d="M12 16v-4.5"/><circle cx="12" cy="8" r=".7" fill="currentColor"/></svg>"#;
pub const MORE: &str = r#"<svg viewBox="0 0 24 24" fill="currentColor"><circle cx="5.5" cy="12" r="1.7"/><circle cx="12" cy="12" r="1.7"/><circle cx="18.5" cy="12" r="1.7"/></svg>"#;
pub const CHEV_L: &str = r#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.1" stroke-linecap="round" stroke-linejoin="round"><path d="m15 18-6-6 6-6"/></svg>"#;
pub const CHEV_R: &str = r#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.1" stroke-linecap="round" stroke-linejoin="round"><path d="m9 18 6-6-6-6"/></svg>"#;
pub const CHEV_D: &str = r#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="m6 9 6 6 6-6"/></svg>"#;
pub const BACK10: &str = r#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M3 12a9 9 0 1 0 3-6.7L3 8"/><path d="M3 3v5h5"/><text x="12" y="15.2" text-anchor="middle" font-size="7.4" font-weight="700" fill="currentColor" stroke="none" font-family="Golos Text,sans-serif">10</text></svg>"#;
pub const FWD10: &str = r#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M21 12a9 9 0 1 1-3-6.7L21 8"/><path d="M21 3v5h-5"/><text x="12" y="15.2" text-anchor="middle" font-size="7.4" font-weight="700" fill="currentColor" stroke="none" font-family="Golos Text,sans-serif">10</text></svg>"#;
pub const VOL: &str = r#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linecap="round" stroke-linejoin="round"><path d="M11 5 6 9H3v6h3l5 4V5z"/><path d="M15.5 8.5a5 5 0 0 1 0 7"/><path d="M18.8 5.8a9 9 0 0 1 0 12.4"/></svg>"#;
pub const VOL_MUTE: &str = r#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linecap="round" stroke-linejoin="round"><path d="M11 5 6 9H3v6h3l5 4V5z"/><path d="m22 9-6 6M16 9l6 6"/></svg>"#;
pub const AUDIO_CC: &str = r#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.85" stroke-linecap="round" stroke-linejoin="round"><path d="M3 17v-5a9 9 0 0 1 18 0v5"/><path d="M21 18a2 2 0 0 1-2 2h-1a2 2 0 0 1-2-2v-2.5a2 2 0 0 1 2-2h3V18zM3 18a2 2 0 0 0 2 2h1a2 2 0 0 0 2-2v-2.5a2 2 0 0 0-2-2H3V18z"/></svg>"#;
pub const LIST: &str = r#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linecap="round"><path d="M8 6h13M8 12h13M8 18h13"/><circle cx="4" cy="6" r="1" fill="currentColor"/><circle cx="4" cy="12" r="1" fill="currentColor"/><circle cx="4" cy="18" r="1" fill="currentColor"/></svg>"#;
pub const FULL: &str = r#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linecap="round" stroke-linejoin="round"><path d="M8 3H5a2 2 0 0 0-2 2v3m18 0V5a2 2 0 0 0-2-2h-3m0 18h3a2 2 0 0 0 2-2v-3M3 16v3a2 2 0 0 0 2 2h3"/></svg>"#;
pub const LOCK: &str = r#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.85" stroke-linecap="round" stroke-linejoin="round"><rect x="5" y="11" width="14" height="10" rx="2.5"/><path d="M8 11V7a4 4 0 0 1 8 0v4"/></svg>"#;
pub const UNLOCK: &str = r#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.85" stroke-linecap="round" stroke-linejoin="round"><rect x="5" y="11" width="14" height="10" rx="2.5"/><path d="M8 11V7a4 4 0 0 1 7.6-1.7"/></svg>"#;
pub const SUN: &str = r#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.85" stroke-linecap="round"><circle cx="12" cy="12" r="4"/><path d="M12 2v2.2M12 19.8V22M4.9 4.9l1.6 1.6M17.5 17.5l1.6 1.6M2 12h2.2M19.8 12H22M4.9 19.1l1.6-1.6M17.5 6.5l1.6-1.6"/></svg>"#;
pub const MOON: &str = r#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.85" stroke-linecap="round" stroke-linejoin="round"><path d="M20.5 14.5A8.5 8.5 0 1 1 9.5 3.5a6.7 6.7 0 0 0 11 11z"/></svg>"#;
pub const REFRESH: &str = r#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linecap="round" stroke-linejoin="round"><path d="M21 12a9 9 0 1 1-2.6-6.4L21 8"/><path d="M21 3v5h-5"/></svg>"#;
pub const TRASH: &str = r#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.85" stroke-linecap="round" stroke-linejoin="round"><path d="M4 7h16M9 7V4h6v3M6 7l1 13a1.5 1.5 0 0 0 1.5 1.4h7A1.5 1.5 0 0 0 17 20l1-13"/></svg>"#;
pub const EXT: &str = r#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linecap="round" stroke-linejoin="round"><path d="M14 4h6v6M10 14 20 4"/><path d="M18 13v6a1 1 0 0 1-1 1H5a1 1 0 0 1-1-1V7a1 1 0 0 1 1-1h6"/></svg>"#;
pub const CLOCK: &str = r#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linecap="round"><circle cx="12" cy="12" r="9"/><path d="M12 7v5l3 2"/></svg>"#;
pub const PHONE: &str = r#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.85" stroke-linecap="round"><rect x="7" y="2.5" width="10" height="19" rx="2.5"/><path d="M11 18h2"/></svg>"#;
pub const TV: &str = r#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.85" stroke-linecap="round"><rect x="2.5" y="4.5" width="19" height="13" rx="2"/><path d="M8 21h8"/></svg>"#;
pub const LAPTOP: &str = r#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.85" stroke-linecap="round"><rect x="4" y="4.5" width="16" height="11.5" rx="1.8"/><path d="M2 19.5h20"/></svg>"#;
pub const STAR: &str = r#"<svg viewBox="0 0 24 24" fill="currentColor"><path d="m12 2.5 2.9 6 6.6.9-4.8 4.6 1.2 6.5L12 17.4 6.1 20.5l1.2-6.5-4.8-4.6 6.6-.9L12 2.5z"/></svg>"#;
pub const SPARKLE: &str = r#"<svg viewBox="0 0 24 24" fill="currentColor"><path d="M12 2l1.8 6.2L20 10l-6.2 1.8L12 18l-1.8-6.2L4 10l6.2-1.8L12 2z"/></svg>"#;
pub const LOGOUT: &str = r#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.85" stroke-linecap="round" stroke-linejoin="round"><path d="M9 21H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h4"/><path d="m16 17 5-5-5-5M21 12H9"/></svg>"#;
pub const TG: &str = r#"<svg viewBox="0 0 24 24" fill="currentColor"><path d="M21.4 4.3 2.9 11.4c-1.2.5-1.2 1.2-.2 1.5l4.7 1.5 11-6.9c.5-.3 1-.1.6.2l-8.9 8-.3 4.9c.5 0 .7-.2 1-.5l2.4-2.3 5 3.7c.9.5 1.6.2 1.8-.9l3.3-15.4c.3-1.4-.5-2-1.9-1.4z"/></svg>"#;

const SIG_4: &str = r#"<svg class="sig" viewBox="0 0 16 14" width="14" height="12"><rect x="0" y="10" width="2.8" height="4" rx="1" fill="var(--ok)"/><rect x="4" y="7" width="2.8" height="7" rx="1" fill="var(--ok)"/><rect x="8" y="4" width="2.8" height="10" rx="1" fill="var(--ok)"/><rect x="12" y="1" width="2.8" height="13" rx="1" fill="var(--ok)"/></svg>"#;
const SIG_3: &str = r#"<svg class="sig" viewBox="0 0 16 14" width="14" height="12"><rect x="0" y="10" width="2.8" height="4" rx="1" fill="var(--ok)"/><rect x="4" y="7" width="2.8" height="7" rx="1" fill="var(--ok)"/><rect x="8" y="4" width="2.8" height="10" rx="1" fill="var(--ok)"/><rect x="12" y="1" width="2.8" height="13" rx="1" fill="var(--line)"/></svg>"#;
const SIG_2: &str = r#"<svg class="sig" viewBox="0 0 16 14" width="14" height="12"><rect x="0" y="10" width="2.8" height="4" rx="1" fill="var(--warn)"/><rect x="4" y="7" width="2.8" height="7" rx="1" fill="var(--warn)"/><rect x="8" y="4" width="2.8" height="10" rx="1" fill="var(--line)"/><rect x="12" y="1" width="2.8" height="13" rx="1" fill="var(--line)"/></svg>"#;
const SIG_1: &str = r#"<svg class="sig" viewBox="0 0 16 14" width="14" height="12"><rect x="0" y="10" width="2.8" height="4" rx="1" fill="var(--err)"/><rect x="4" y="7" width="2.8" height="7" rx="1" fill="var(--line)"/><rect x="8" y="4" width="2.8" height="10" rx="1" fill="var(--line)"/><rect x="12" y="1" width="2.8" height="13" rx="1" fill="var(--line)"/></svg>"#;

pub fn seeder_signal(seeders: u32) -> &'static str {
    if seeders >= 50 {
        SIG_4
    } else if seeders >= 20 {
        SIG_3
    } else if seeders >= 5 {
        SIG_2
    } else {
        SIG_1
    }
}

#[component]
pub fn Icon(svg: &'static str, #[props(default = "ic")] class: &'static str) -> Element {
    rsx! { span { class: "{class}", "aria-hidden": "true", dangerous_inner_html: svg } }
}
