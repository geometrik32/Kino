//! Browser and Telegram glue: Mini App `initData`, URL query, back/main buttons, haptics.
//! Only the website has these; the phone and desktop apps get inert stand-ins.

#[cfg(target_arch = "wasm32")]
mod web;
#[cfg(target_arch = "wasm32")]
pub use web::*;

#[cfg(not(target_arch = "wasm32"))]
mod native;
#[cfg(not(target_arch = "wasm32"))]
pub use native::*;

/// Deep-link target used to open a title (optionally starting playback at once).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OpenTarget {
    pub kind: kino_core::MediaKind,
    pub tmdb_id: i64,
    pub autoplay: bool,
}

/// Parses `?open=<kind>_<id>` (with optional `&play=1`) or `startapp=<kind>_<id>[_play]`.
pub fn parse_open_target(open: Option<&str>, play: Option<&str>, start: Option<&str>) -> Option<OpenTarget> {
    let from_query = open.is_some();
    let raw = open.or(start)?;
    let (kind_str, rest) = raw.split_once('_')?;
    let kind = kino_core::MediaKind::parse(kind_str)?;
    let (id_str, start_play) = match rest.strip_suffix("_play") {
        Some(id) if !from_query => (id, true),
        _ => (rest, false),
    };
    let tmdb_id = id_str.parse().ok()?;
    let query_play = play.is_some_and(|v| matches!(v.trim(), "1" | "true" | "yes"));
    Some(OpenTarget {
        kind,
        tmdb_id,
        autoplay: start_play || query_play,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use kino_core::MediaKind;

    #[test]
    fn parses_open_query_and_optional_play_flag() {
        assert_eq!(
            parse_open_target(Some("tv_1396"), None, None),
            Some(OpenTarget {
                kind: MediaKind::Tv,
                tmdb_id: 1396,
                autoplay: false,
            })
        );
        assert_eq!(
            parse_open_target(Some("tv_1396"), Some("1"), None),
            Some(OpenTarget {
                kind: MediaKind::Tv,
                tmdb_id: 1396,
                autoplay: true,
            })
        );
        assert_eq!(
            parse_open_target(Some("movie_497"), Some("true"), None),
            Some(OpenTarget {
                kind: MediaKind::Movie,
                tmdb_id: 497,
                autoplay: true,
            })
        );
        assert_eq!(
            parse_open_target(Some("movie_497"), Some("0"), None),
            Some(OpenTarget {
                kind: MediaKind::Movie,
                tmdb_id: 497,
                autoplay: false,
            })
        );
    }

    #[test]
    fn parses_startapp_with_and_without_play_suffix() {
        assert_eq!(
            parse_open_target(None, None, Some("movie_603")),
            Some(OpenTarget {
                kind: MediaKind::Movie,
                tmdb_id: 603,
                autoplay: false,
            })
        );
        assert_eq!(
            parse_open_target(None, None, Some("tv_9687_play")),
            Some(OpenTarget {
                kind: MediaKind::Tv,
                tmdb_id: 9687,
                autoplay: true,
            })
        );
        assert_eq!(parse_open_target(None, None, Some("inv_abc")), None);
        assert_eq!(parse_open_target(Some("tv_abc"), Some("1"), None), None);
    }
}
