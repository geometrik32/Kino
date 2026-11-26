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
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenTarget {
    pub kind: kino_core::MediaKind,
    pub tmdb_id: i64,
    pub autoplay: bool,
    pub season: Option<i32>,
    pub release: Option<String>,
}

/// Parses `?open=<kind>_<id>` (with optional `&play=1`) or `startapp=<kind>_<id>[_play]`.
#[cfg_attr(not(any(test, target_arch = "wasm32")), allow(dead_code))]
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
        season: None,
        release: None,
    })
}

/// Initial route parsed from the URL query or Telegram `start_param` (preserves screen across reloads).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InitialRoute {
    Home,
    Search(String),
    Mine(Option<String>),
    Profile,
    Title(OpenTarget),
    Person(i64),
    Shelf(String),
}

/// Parses the URL query and optional Mini App `start_param` into an initial screen.
#[cfg_attr(not(any(test, target_arch = "wasm32")), allow(dead_code))]
pub fn parse_initial_route(params: &std::collections::BTreeMap<String, String>, start: Option<&str>) -> InitialRoute {
    if let Some(mut t) = parse_open_target(
        params.get("open").map(String::as_str),
        params.get("play").map(String::as_str),
        start,
    ) {
        t.season = params
            .get("season")
            .and_then(|s| s.trim().parse::<i32>().ok())
            .filter(|&s| s > 0);
        t.release = params
            .get("release")
            .map(|s| s.trim().to_ascii_lowercase())
            .filter(|r| kino_core::is_release_key(r));
        return InitialRoute::Title(t);
    }
    if let Some(tab) = params.get("tab").map(String::as_str) {
        match tab {
            "profile" => return InitialRoute::Profile,
            "my" | "mine" => return InitialRoute::Mine(params.get("sec").cloned()),
            "search" => return InitialRoute::Search(params.get("q").cloned().unwrap_or_default()),
            _ => {}
        }
    }
    if let Some(id) = params
        .get("person")
        .and_then(|s| s.parse::<i64>().ok())
        .filter(|&id| id > 0)
    {
        return InitialRoute::Person(id);
    }
    if let Some(id) = params.get("shelf").filter(|s| !s.is_empty()) {
        return InitialRoute::Shelf(id.clone());
    }
    InitialRoute::Home
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
                season: None,
                release: None,
            })
        );
        assert_eq!(
            parse_open_target(Some("tv_1396"), Some("1"), None),
            Some(OpenTarget {
                kind: MediaKind::Tv,
                tmdb_id: 1396,
                autoplay: true,
                season: None,
                release: None,
            })
        );
        assert_eq!(
            parse_open_target(Some("movie_497"), Some("true"), None),
            Some(OpenTarget {
                kind: MediaKind::Movie,
                tmdb_id: 497,
                autoplay: true,
                season: None,
                release: None,
            })
        );
        assert_eq!(
            parse_open_target(Some("movie_497"), Some("0"), None),
            Some(OpenTarget {
                kind: MediaKind::Movie,
                tmdb_id: 497,
                autoplay: false,
                season: None,
                release: None,
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
                season: None,
                release: None,
            })
        );
        assert_eq!(
            parse_open_target(None, None, Some("tv_9687_play")),
            Some(OpenTarget {
                kind: MediaKind::Tv,
                tmdb_id: 9687,
                autoplay: true,
                season: None,
                release: None,
            })
        );
        assert_eq!(parse_open_target(None, None, Some("inv_abc")), None);
        assert_eq!(parse_open_target(Some("tv_abc"), Some("1"), None), None);
    }

    #[test]
    fn parses_initial_routes_for_tabs_and_pages() {
        let map = |pairs: &[(&str, &str)]| {
            pairs
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect::<std::collections::BTreeMap<_, _>>()
        };
        assert_eq!(parse_initial_route(&map(&[]), None), InitialRoute::Home);
        assert_eq!(
            parse_initial_route(&map(&[("tab", "profile")]), None),
            InitialRoute::Profile
        );
        assert_eq!(
            parse_initial_route(&map(&[("tab", "my"), ("sec", "calendar")]), None),
            InitialRoute::Mine(Some("calendar".into()))
        );
        assert_eq!(
            parse_initial_route(&map(&[("tab", "search"), ("q", "дюна")]), None),
            InitialRoute::Search("дюна".into())
        );
        assert_eq!(
            parse_initial_route(&map(&[("person", "42")]), None),
            InitialRoute::Person(42)
        );
        assert_eq!(
            parse_initial_route(&map(&[("shelf", "rewatch")]), None),
            InitialRoute::Shelf("rewatch".into())
        );
        let hash = "AB".repeat(20);
        assert_eq!(
            parse_initial_route(
                &map(&[("open", "tv_1396"), ("play", "1"), ("season", "2"), ("release", &hash)]),
                None
            ),
            InitialRoute::Title(OpenTarget {
                kind: MediaKind::Tv,
                tmdb_id: 1396,
                autoplay: true,
                season: Some(2),
                release: Some("ab".repeat(20)),
            })
        );
        assert_eq!(
            parse_initial_route(
                &map(&[
                    ("open", "movie_603"),
                    ("play", "1"),
                    ("season", "0"),
                    ("release", "bad")
                ]),
                None
            ),
            InitialRoute::Title(OpenTarget {
                kind: MediaKind::Movie,
                tmdb_id: 603,
                autoplay: true,
                season: None,
                release: None,
            })
        );
    }
}
