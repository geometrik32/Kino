//! Plays what the server prepared in the native libmpv window.
//!
//! The browser needs the server to re-encode HEVC/AC3/DTS/MKV; libmpv plays the original
//! file straight from the stream link, with every audio track and embedded subtitle.

use kino_core::PlayInfo;
use kino_mpv::PlayOptions;

/// Makes a server path like `/api/stream/<token>` a full URL.
pub fn absolute(base: &str, path: &str) -> String {
    if path.starts_with("http://") || path.starts_with("https://") {
        path.to_owned()
    } else {
        format!("{}/{}", base.trim_end_matches('/'), path.trim_start_matches('/'))
    }
}

/// Window title: "Во все тяжкие · S02E06".
pub fn window_title(title: &str, episode: Option<String>) -> String {
    match episode {
        Some(e) => format!("{title} · {e}"),
        None => title.to_owned(),
    }
}

/// How to open the file described by [`PlayInfo`]: the original stream, resumed where the viewer stopped,
/// with the remembered audio and subtitle tracks.
pub fn play_options(base: &str, info: &PlayInfo, title: String, next: Option<String>) -> PlayOptions {
    PlayOptions {
        url: absolute(base, &info.stream),
        title,
        start: f64::from(info.start_at),
        audio: info.audio_track.and_then(|i| usize::try_from(i).ok()),
        subtitle: info.subtitle_track.and_then(|i| usize::try_from(i).ok()),
        next,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info() -> PlayInfo {
        PlayInfo {
            infohash: "abc".into(),
            file_index: 2,
            file_name: "Show.S02E05.mkv".into(),
            stream: "/api/stream/tok".into(),
            direct: false,
            duration: Some(2700.0),
            audio: vec![],
            subtitles: vec![],
            audio_track: Some(1),
            subtitle_track: None,
            start_at: 754,
            next: Some((2, 6)),
            preloaded: false,
        }
    }

    #[test]
    fn stream_link_becomes_absolute_and_untranscoded() {
        let o = play_options(
            "https://kino.example/",
            &info(),
            "Шоу · S02E05".into(),
            Some("S02E06".into()),
        );
        assert_eq!(o.url, "https://kino.example/api/stream/tok");
        assert_eq!(o.start, 754.0);
        assert_eq!(o.audio, Some(1));
        assert_eq!(o.subtitle, None);
        assert_eq!(o.next.as_deref(), Some("S02E06"));
    }

    #[test]
    fn full_urls_are_kept() {
        assert_eq!(absolute("https://a", "https://b/x"), "https://b/x");
        assert_eq!(absolute("https://a/", "/x"), "https://a/x");
    }

    #[test]
    fn title_with_episode() {
        assert_eq!(window_title("Шоу", Some("S01E02".into())), "Шоу · S01E02");
        assert_eq!(window_title("Фильм", None), "Фильм");
    }
}
