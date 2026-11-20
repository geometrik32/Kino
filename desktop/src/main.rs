// No console window in release builds on Windows.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod config;
#[allow(dead_code)] // wired to the player screen once the shared app layer lands
mod native;

use dioxus::desktop::{tao::window::Icon, Config as WindowConfig, LogicalSize, WindowBuilder};
use dioxus::prelude::*;
use kino_ui::STYLE;

/// The window icon: 64x64 RGBA pixels (the .ico for the exe and installer is `assets/icon.ico`).
fn icon() -> Option<Icon> {
    Icon::from_rgba(include_bytes!("../assets/icon-64.rgba").to_vec(), 64, 64).ok()
}

fn main() {
    let window = WindowBuilder::new()
        .with_title("Орловский кинопоказ")
        .with_inner_size(LogicalSize::new(1280.0, 800.0))
        .with_min_inner_size(LogicalSize::new(480.0, 600.0))
        .with_window_icon(icon());
    dioxus::LaunchBuilder::desktop()
        .with_cfg(WindowConfig::new().with_window(window).with_menu(None))
        .launch(App);
}

#[component]
fn App() -> Element {
    let mut server = use_signal(|| config::Config::load().server);
    let mut saved = use_signal(|| false);
    let player = match kino_mpv::Mpv::new(&[("vo", "null"), ("ao", "null")]) {
        Ok(m) => format!("mpv {}", m.property("mpv-version").unwrap_or_default()),
        Err(e) => e.to_string(),
    };
    rsx! {
        style { "{STYLE}" }
        div { class: "content",
            h1 { "Орловский кинопоказ" }
            p { class: "muted", "Проигрыватель: {player}" }
            p { class: "muted", "Адрес сервера" }
            input {
                value: "{server}",
                oninput: move |e| {
                    server.set(e.value());
                    saved.set(false);
                },
            }
            button {
                class: "btn",
                onclick: move |_| {
                    let cfg = config::Config { server: server().trim().trim_end_matches('/').to_owned() };
                    saved.set(cfg.save().is_ok());
                },
                "Сохранить"
            }
            if saved() {
                p { class: "muted", "Сохранено" }
            }
        }
    }
}
