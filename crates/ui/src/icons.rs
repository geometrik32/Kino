//! Material You (Rounded) SVG icon set — friendly rounded strokes and filled
//! active variants so icons render crisply offline across Web, Windows,
//! Android and Telegram Mini App.

#![allow(non_snake_case)]

use dioxus::prelude::*;

pub const HOME: &str = r#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M4.2 10.6 10.8 4.9a1.9 1.9 0 0 1 2.4 0l6.6 5.7a1.5 1.5 0 0 1 .5 1.13V18.5a2.2 2.2 0 0 1-2.2 2.2h-3.6v-5.2a1.5 1.5 0 0 0-1.5-1.5h-2a1.5 1.5 0 0 0-1.5 1.5v5.2H5.9a2.2 2.2 0 0 1-2.2-2.2v-6.77a1.5 1.5 0 0 1 .5-1.13z"/></svg>"#;
pub const HOME_FILLED: &str = r#"<svg viewBox="0 0 24 24" fill="currentColor"><path d="M10.7 3.85a2.1 2.1 0 0 1 2.6 0l6.75 5.6A2.2 2.2 0 0 1 20.8 11.15v7.65a2.4 2.4 0 0 1-2.4 2.4h-3.4a1.2 1.2 0 0 1-1.2-1.2v-4.6a1.3 1.3 0 0 0-1.3-1.3h-1a1.3 1.3 0 0 0-1.3 1.3V20a1.2 1.2 0 0 1-1.2 1.2H5.6a2.4 2.4 0 0 1-2.4-2.4v-7.65a2.2 2.2 0 0 1 .75-1.7l6.75-5.6z"/></svg>"#;
pub const SEARCH: &str = r#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="11" cy="11" r="6.8"/><path d="m19.8 19.8-3.9-3.9"/></svg>"#;
pub const SEARCH_ACTIVE: &str = r#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.55" stroke-linecap="round" stroke-linejoin="round"><circle cx="10.8" cy="10.8" r="6.6" fill="currentColor" fill-opacity=".18"/><path d="m20 20-4.1-4.1"/></svg>"#;
pub const BOOKMARK: &str = r#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M6.5 4.2h11a2 2 0 0 1 2 2v13.9a.7.7 0 0 1-1.06.6L12 16.9l-6.44 3.8a.7.7 0 0 1-1.06-.6V6.2a2 2 0 0 1 2-2z"/></svg>"#;
pub const BOOKMARK_FILLED: &str = r#"<svg viewBox="0 0 24 24" fill="currentColor"><path d="M6.8 3.5h10.4a2.3 2.3 0 0 1 2.3 2.3v14.45a.9.9 0 0 1-1.37.77L12 17.25l-6.13 3.77a.9.9 0 0 1-1.37-.77V5.8a2.3 2.3 0 0 1 2.3-2.3z"/></svg>"#;
pub const PLAY: &str = r#"<svg viewBox="0 0 24 24" fill="currentColor"><path d="M6.5 5.4v13.2a1.3 1.3 0 0 0 1.98 1.1l10.4-6.6a1.3 1.3 0 0 0 0-2.2L8.48 4.3A1.3 1.3 0 0 0 6.5 5.4z"/></svg>"#;
pub const PAUSE: &str = r#"<svg viewBox="0 0 24 24" fill="currentColor"><rect x="6" y="4.5" width="4.4" height="15" rx="2"/><rect x="13.6" y="4.5" width="4.4" height="15" rx="2"/></svg>"#;
pub const BACK: &str = r#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.1" stroke-linecap="round" stroke-linejoin="round"><path d="M19 12H5.5"/><path d="m11.5 18-6-6 6-6"/></svg>"#;
pub const CLOSE: &str = r#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.15" stroke-linecap="round"><path d="M18 6 6 18M6 6l12 12"/></svg>"#;
pub const SWAP: &str = r#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="m16 3.5 3.8 3.8L16 11"/><path d="M19.5 7.3H4.5"/><path d="m8 20.5-3.8-3.8L8 13"/><path d="M4.5 16.7h15"/></svg>"#;
pub const FILTER: &str = r#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M4 6.5h16M7 12h10M10 17.5h4"/></svg>"#;
pub const SORT: &str = r#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="m7.5 15 4.5 4.5L16.5 15"/><path d="M7.5 9 12 4.5 16.5 9"/></svg>"#;
pub const DOWNLOAD: &str = r#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M12 3.5v11.3m0 0-4.3-4.3M12 14.8l4.3-4.3"/><path d="M4.5 19.2h15"/></svg>"#;
pub const BOLT: &str = r#"<svg viewBox="0 0 24 24" fill="currentColor"><path d="M13.6 2.3a.8.8 0 0 0-1.44-.42L4.8 12.1a.9.9 0 0 0 .73 1.42H11l-1.1 8.15a.8.8 0 0 0 1.44.45l7.86-10.22a.9.9 0 0 0-.72-1.45H13l.6-8.15z"/></svg>"#;
pub const CHECK: &str = r#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.4" stroke-linecap="round" stroke-linejoin="round"><path d="m5 12.5 4.5 4.5L19 7.2"/></svg>"#;
pub const INFO: &str = r#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="9"/><path d="M12 16v-4.2"/><circle cx="12" cy="8.2" r=".9" fill="currentColor" stroke="none"/></svg>"#;
pub const MORE: &str = r#"<svg viewBox="0 0 24 24" fill="currentColor"><circle cx="5.5" cy="12" r="1.85"/><circle cx="12" cy="12" r="1.85"/><circle cx="18.5" cy="12" r="1.85"/></svg>"#;
pub const CHEV_L: &str = r#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round"><path d="m14.8 18-5.8-6 5.8-6"/></svg>"#;
pub const CHEV_R: &str = r#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round"><path d="m9.2 18 5.8-6-5.8-6"/></svg>"#;
pub const CHEV_D: &str = r#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round"><path d="m6.5 9.2 5.5 5.6 5.5-5.6"/></svg>"#;
pub const BACK10: &str = r#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.95" stroke-linecap="round" stroke-linejoin="round"><path d="M3.2 12.2a8.8 8.8 0 1 0 2.9-6.5L3.2 8.2"/><path d="M3.2 3.2v5h5"/><text x="12" y="15.3" text-anchor="middle" font-size="7.4" font-weight="700" fill="currentColor" stroke="none" font-family="Golos Text,sans-serif">10</text></svg>"#;
pub const FWD10: &str = r#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.95" stroke-linecap="round" stroke-linejoin="round"><path d="M20.8 12.2a8.8 8.8 0 1 1-2.9-6.5l2.9 2.5"/><path d="M20.8 3.2v5h-5"/><text x="12" y="15.3" text-anchor="middle" font-size="7.4" font-weight="700" fill="currentColor" stroke="none" font-family="Golos Text,sans-serif">10</text></svg>"#;
pub const VOL: &str = r#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M11 5.2 6.2 9H3.5a1 1 0 0 0-1 1v4a1 1 0 0 0 1 1h2.7L11 18.8V5.2z"/><path d="M15.4 8.6a4.8 4.8 0 0 1 0 6.8"/><path d="M18.6 6a8.6 8.6 0 0 1 0 12"/></svg>"#;
pub const VOL_MUTE: &str = r#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M11 5.2 6.2 9H3.5a1 1 0 0 0-1 1v4a1 1 0 0 0 1 1h2.7L11 18.8V5.2z"/><path d="m21.5 9.2-5.6 5.6M15.9 9.2l5.6 5.6"/></svg>"#;
pub const AUDIO_CC: &str = r#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M3.5 16.8V12a8.5 8.5 0 0 1 17 0v4.8"/><path d="M20.5 17.6a2.2 2.2 0 0 1-2.2 2.2h-.8a2 2 0 0 1-2-2v-2.4a2 2 0 0 1 2-2h3v4.2zM3.5 17.6a2.2 2.2 0 0 0 2.2 2.2h.8a2 2 0 0 0 2-2v-2.4a2 2 0 0 0-2-2h-3v4.2z"/></svg>"#;
pub const SETTINGS: &str = r#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.95" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="3.1"/><path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 0 1-2.83 2.83l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 0 1-4 0v-.09a1.65 1.65 0 0 0-1-1.51 1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 0 1-2.83-2.83l.06-.06a1.65 1.65 0 0 0 .33-1.82 1.65 1.65 0 0 0-1.51-1H3a2 2 0 0 1 0-4h.09a1.65 1.65 0 0 0 1.51-1 1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 0 1 2.83-2.83l.06.06a1.65 1.65 0 0 0 1.82.33H9a1.65 1.65 0 0 0 1-1.51V3a2 2 0 0 1 4 0v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 0 1 2.83 2.83l-.06.06a1.65 1.65 0 0 0-.33 1.82V9c.26.6.85 1 1.51 1H21a2 2 0 0 1 0 4h-.09a1.65 1.65 0 0 0-1.51 1z"/></svg>"#;
pub const LIST: &str = r#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"><path d="M8.5 6.5h12M8.5 12h12M8.5 17.5h12"/><circle cx="4.2" cy="6.5" r="1.2" fill="currentColor" stroke="none"/><circle cx="4.2" cy="12" r="1.2" fill="currentColor" stroke="none"/><circle cx="4.2" cy="17.5" r="1.2" fill="currentColor" stroke="none"/></svg>"#;
pub const FULL: &str = r#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M8.2 3.5H5.5a2 2 0 0 0-2 2v2.7m17 0V5.5a2 2 0 0 0-2-2h-2.7m0 17h2.7a2 2 0 0 0 2-2v-2.7M3.5 15.8v2.7a2 2 0 0 0 2 2h2.7"/></svg>"#;
pub const EXIT_FULL: &str = r#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M8.2 3.5v2.7a2 2 0 0 1-2 2H3.5m17 0h-2.7a2 2 0 0 1-2-2V3.5m0 17v-2.7a2 2 0 0 1 2-2h2.7M3.5 15.8h2.7a2 2 0 0 1 2 2v2.7"/></svg>"#;
pub const FIT_WIDTH: &str = r#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.95" stroke-linecap="round" stroke-linejoin="round"><rect x="2.5" y="4.5" width="19" height="15" rx="3"/><path d="M5.8 12h12.4"/><path d="M8.6 9.2 5.8 12l2.8 2.8M15.4 9.2 18.2 12l-2.8 2.8"/></svg>"#;
pub const FIT_HEIGHT: &str = r#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.95" stroke-linecap="round" stroke-linejoin="round"><rect x="2.5" y="4.5" width="19" height="15" rx="3"/><path d="M12 7.2v9.6"/><path d="M9.3 9.8 12 7.2l2.7 2.6M9.3 14.2 12 16.8l2.7-2.6"/></svg>"#;
pub const LOCK: &str = r#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><rect x="5" y="10.8" width="14" height="10" rx="3"/><path d="M8.2 10.8V7.2a3.8 3.8 0 0 1 7.6 0v3.6"/></svg>"#;
pub const UNLOCK: &str = r#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><rect x="5" y="10.8" width="14" height="10" rx="3"/><path d="M8.2 10.8V7.2a3.8 3.8 0 0 1 7.3-1.5"/></svg>"#;
pub const SUN: &str = r#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"><circle cx="12" cy="12" r="4"/><path d="M12 2.2v2.1M12 19.7v2.1M5.1 5.1l1.5 1.5M17.4 17.4l1.5 1.5M2.2 12h2.1M19.7 12h2.1M5.1 18.9l1.5-1.5M17.4 6.6l1.5-1.5"/></svg>"#;
pub const SUN_FILLED: &str = r#"<svg viewBox="0 0 24 24" fill="currentColor" stroke="currentColor" stroke-width="2" stroke-linecap="round"><circle cx="12" cy="12" r="4.3"/><path d="M12 2.2v2M12 19.8v2M5.1 5.1l1.4 1.4M17.5 17.5l1.4 1.4M2.2 12h2M19.8 12h2M5.1 18.9l1.4-1.4M17.5 6.5l1.4-1.4"/></svg>"#;
pub const MOON: &str = r#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M20.3 14.5A8.4 8.4 0 1 1 9.5 3.7a6.6 6.6 0 0 0 10.8 10.8z"/></svg>"#;
pub const MOON_FILLED: &str = r#"<svg viewBox="0 0 24 24" fill="currentColor"><path d="M20.4 14.6a.9.9 0 0 0-1-.28A6.5 6.5 0 0 1 9.68 4.6a.9.9 0 0 0-.28-1 .95.95 0 0 0-.96-.17A9.1 9.1 0 1 0 20.57 15.56a.95.95 0 0 0-.17-.96z"/></svg>"#;
pub const REFRESH: &str = r#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M20.6 12a8.6 8.6 0 1 1-2.5-6.1L20.6 8.2"/><path d="M20.6 3.5v4.8h-4.8"/></svg>"#;
pub const TRASH: &str = r#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.95" stroke-linecap="round" stroke-linejoin="round"><path d="M4.5 7h15M9.2 7V4.6a.8.8 0 0 1 .8-.8h4a.8.8 0 0 1 .8.8V7M6.3 7l.9 12.6a2 2 0 0 0 2 1.8h5.6a2 2 0 0 0 2-1.8L17.7 7"/></svg>"#;
pub const EXT: &str = r#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M14.5 4.2h5.3v5.3M10.2 13.8 19.6 4.4"/><path d="M18 13.5v4.5a2 2 0 0 1-2 2H6a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2h4.5"/></svg>"#;
pub const CLOCK: &str = r#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="8.8"/><path d="M12 7.2V12l3.2 2"/></svg>"#;
pub const PHONE: &str = r#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.95" stroke-linecap="round"><rect x="6.8" y="2.5" width="10.4" height="19" rx="3"/><path d="M10.8 18h2.4"/></svg>"#;
pub const TV: &str = r#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.95" stroke-linecap="round"><rect x="2.5" y="4.5" width="19" height="13" rx="3"/><path d="M8.2 21h7.6"/></svg>"#;
pub const LAPTOP: &str = r#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.95" stroke-linecap="round"><rect x="4" y="4.5" width="16" height="11.5" rx="2.5"/><path d="M2.2 19.5h19.6"/></svg>"#;
pub const STAR: &str = r#"<svg viewBox="0 0 24 24" fill="currentColor"><path d="M11.1 3.1a1 1 0 0 1 1.8 0l2.45 5.1 5.6.75a1 1 0 0 1 .56 1.72l-4.1 3.9 1.02 5.56a1 1 0 0 1-1.46 1.06L12 18.5l-4.97 2.69a1 1 0 0 1-1.46-1.06l1.02-5.56-4.1-3.9a1 1 0 0 1 .56-1.72l5.6-.75 2.45-5.1z"/></svg>"#;
pub const SPARKLE: &str = r#"<svg viewBox="0 0 24 24" fill="currentColor"><path d="M12 2l2 6.2L20.2 10 14 12l-2 6.2L10 12 3.8 10 10 8.2 12 2z"/></svg>"#;
pub const LOGOUT: &str = r#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M9.5 20.5H6a2.5 2.5 0 0 1-2.5-2.5V6A2.5 2.5 0 0 1 6 3.5h3.5"/><path d="m16 16.8 4.8-4.8L16 7.2M20.5 12H9.5"/></svg>"#;
pub const CALENDAR: &str = r#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.95" stroke-linecap="round" stroke-linejoin="round"><rect x="3.2" y="4.8" width="17.6" height="16" rx="3"/><path d="M16 2.8v3.8M8 2.8v3.8M3.4 9.8h17.2"/></svg>"#;
pub const USERS: &str = r#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.95" stroke-linecap="round" stroke-linejoin="round"><path d="M16 20.5v-1.8a3.8 3.8 0 0 0-3.8-3.8H6.5a3.8 3.8 0 0 0-3.8 3.8v1.8"/><circle cx="9.3" cy="7.2" r="3.8"/><path d="M21.3 20.5v-1.8a3.6 3.6 0 0 0-2.8-3.5"/><path d="M15.8 3.6a3.7 3.7 0 0 1 0 7.2"/></svg>"#;
pub const TG: &str = r#"<svg viewBox="0 0 24 24" fill="currentColor"><path d="M21.4 4.3 2.9 11.4c-1.2.5-1.2 1.2-.2 1.5l4.7 1.5 11-6.9c.5-.3 1-.1.6.2l-8.9 8-.3 4.9c.5 0 .7-.2 1-.5l2.4-2.3 5 3.7c.9.5 1.6.2 1.8-.9l3.3-15.4c.3-1.4-.5-2-1.9-1.4z"/></svg>"#;
pub const WIN_MIN: &str = r#"<svg viewBox="0 0 12 12" fill="none" stroke="currentColor" stroke-width="1.35" stroke-linecap="round"><path d="M2 6h8"/></svg>"#;
pub const WIN_MAX: &str = r#"<svg viewBox="0 0 12 12" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linejoin="round"><rect x="2" y="2" width="8" height="8" rx="1.8"/></svg>"#;
pub const WIN_RESTORE: &str = r#"<svg viewBox="0 0 12 12" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linejoin="round"><rect x="2" y="3.5" width="6.5" height="6.5" rx="1.5"/><path d="M3.8 3.5V2.6a1.1 1.1 0 0 1 1.1-1.1h4.5a1.1 1.1 0 0 1 1.1 1.1v4.5a1.1 1.1 0 0 1-1.1 1.1H8.5"/></svg>"#;
pub const WIN_CLOSE: &str = r#"<svg viewBox="0 0 12 12" fill="none" stroke="currentColor" stroke-width="1.35" stroke-linecap="round"><path d="M2.5 2.5l7 7M9.5 2.5l-7 7"/></svg>"#;

const SIG_4: &str = r#"<svg class="sig" viewBox="0 0 16 14" width="14" height="12"><rect x="0" y="10" width="2.8" height="4" rx="1.4" fill="var(--ok)"/><rect x="4" y="7" width="2.8" height="7" rx="1.4" fill="var(--ok)"/><rect x="8" y="4" width="2.8" height="10" rx="1.4" fill="var(--ok)"/><rect x="12" y="1" width="2.8" height="13" rx="1.4" fill="var(--ok)"/></svg>"#;
const SIG_3: &str = r#"<svg class="sig" viewBox="0 0 16 14" width="14" height="12"><rect x="0" y="10" width="2.8" height="4" rx="1.4" fill="var(--ok)"/><rect x="4" y="7" width="2.8" height="7" rx="1.4" fill="var(--ok)"/><rect x="8" y="4" width="2.8" height="10" rx="1.4" fill="var(--ok)"/><rect x="12" y="1" width="2.8" height="13" rx="1.4" fill="var(--line)"/></svg>"#;
const SIG_2: &str = r#"<svg class="sig" viewBox="0 0 16 14" width="14" height="12"><rect x="0" y="10" width="2.8" height="4" rx="1.4" fill="var(--warn)"/><rect x="4" y="7" width="2.8" height="7" rx="1.4" fill="var(--warn)"/><rect x="8" y="4" width="2.8" height="10" rx="1.4" fill="var(--line)"/><rect x="12" y="1" width="2.8" height="13" rx="1.4" fill="var(--line)"/></svg>"#;
const SIG_1: &str = r#"<svg class="sig" viewBox="0 0 16 14" width="14" height="12"><rect x="0" y="10" width="2.8" height="4" rx="1.4" fill="var(--err)"/><rect x="4" y="7" width="2.8" height="7" rx="1.4" fill="var(--line)"/><rect x="8" y="4" width="2.8" height="10" rx="1.4" fill="var(--line)"/><rect x="12" y="1" width="2.8" height="13" rx="1.4" fill="var(--line)"/></svg>"#;

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
