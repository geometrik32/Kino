//! `cargo run -p kino-mpv --example play -- <url> [start_seconds]` — opens the player window.

use std::sync::mpsc;

use kino_mpv::{PlayOptions, PlayerEvent, Session};

fn main() {
    let mut args = std::env::args().skip(1);
    let url = args.next().expect("usage: play <url> [start]");
    let start = args.next().and_then(|s| s.parse().ok()).unwrap_or(0.0);
    let (tx, rx) = mpsc::channel();
    let _session = Session::start(
        PlayOptions {
            url,
            title: "Пример: Орловский кинопоказ".into(),
            start,
            audio: None,
            subtitle: None,
            next: Some("S01E02".into()),
            ..PlayOptions::default()
        },
        false,
        move |e| {
            let _ = tx.send(e);
        },
    )
    .expect("start");
    for e in rx {
        println!("{e:?}");
        if matches!(e, PlayerEvent::Closed { .. }) {
            break;
        }
    }
}
