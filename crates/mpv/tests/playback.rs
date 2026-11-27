//! Plays a generated file without a window. Skipped when libmpv or ffmpeg is missing.

use std::process::Command;
use std::sync::mpsc;
use std::time::Duration;

use kino_mpv::{Mpv, PlayOptions, PlayerEvent, Session};

fn make_video(dir: &std::path::Path) -> Option<String> {
    let path = dir.join("test.mkv");
    let status = Command::new("ffmpeg")
        .args([
            "-v",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            "testsrc=size=320x180:rate=25:duration=6",
        ])
        .args(["-f", "lavfi", "-i", "sine=frequency=440:duration=6"])
        .args(["-f", "lavfi", "-i", "sine=frequency=880:duration=6"])
        .args(["-map", "0:v", "-map", "1:a", "-map", "2:a"])
        .args(["-c:v", "libx264", "-c:a:0", "ac3", "-c:a:1", "aac"])
        .args(["-metadata:s:a:0", "language=rus", "-metadata:s:a:1", "language=eng"])
        .arg(&path)
        .status()
        .ok()?;
    status.success().then(|| path.to_string_lossy().into_owned())
}

#[test]
fn plays_mkv_with_two_audio_tracks_and_reports_events() {
    if Mpv::new(&[("vo", "null"), ("ao", "null")]).is_err() {
        eprintln!("libmpv not installed: skipped");
        return;
    }
    let dir = std::env::temp_dir().join(format!("kino-mpv-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let Some(url) = make_video(&dir) else {
        eprintln!("ffmpeg not installed: skipped");
        return;
    };

    let (tx, rx) = mpsc::channel();
    let session = Session::start(
        PlayOptions {
            url,
            title: "Тест".into(),
            start: 2.0,
            audio: Some(1),
            subtitle: None,
            next: Some("S01E02".into()),
            ..PlayOptions::default()
        },
        true,
        move |e| {
            let _ = tx.send(e);
        },
    )
    .expect("start");

    let mut loaded = false;
    let mut first_position = None;
    let mut ended = false;
    let mut tracks_events = 0;
    while let Ok(e) = rx.recv_timeout(Duration::from_secs(20)) {
        match e {
            PlayerEvent::Loaded { duration, audio, .. } => {
                loaded = true;
                assert!(duration.unwrap() > 5.9);
                assert_eq!(audio.len(), 2);
                assert_eq!(audio[0].language.as_deref(), Some("rus"));
                assert_eq!(audio[1].language.as_deref(), Some("eng"));
            }
            PlayerEvent::Position { position, .. } => {
                first_position.get_or_insert(position);
            }
            PlayerEvent::Tracks { .. } => tracks_events += 1,
            PlayerEvent::Ended => {
                ended = true;
                break;
            }
            PlayerEvent::Error(e) => panic!("{e}"),
            _ => {}
        }
    }
    assert!(loaded, "file was not opened");
    // Resumed near the requested second, not from the beginning.
    assert!(first_position.unwrap() >= 1.9, "started at {first_position:?}");
    assert!(ended, "never reached the end");
    // Tracks we set ourselves are not reported back as the viewer's choice.
    assert_eq!(tracks_events, 0);
    drop(session);
    let _ = std::fs::remove_dir_all(&dir);
}
