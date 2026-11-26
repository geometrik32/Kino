//! Torrents: finding releases (Jackett), reading `.torrent` files, streaming
//! (TorrServer + ffmpeg) and preloading (qBittorrent).

pub mod bencode;
pub mod ffmpeg;
pub mod files;
pub mod fmp4;
pub mod pause_cache;
pub mod qbit;
pub mod search_cache;
pub mod title;
pub mod token;
pub mod torrserver;
pub mod torznab;

use std::{collections::HashMap, sync::LazyLock};

pub use kino_core::is_infohash;
use kino_core::{estimate_bitrate_kbps, MediaKind, ReleaseOption, TitleDetails};
use regex::Regex;
use sha1::{Digest, Sha1};
use sqlx::PgPool;

use self::{
    title::{normalize, parse_title},
    torznab::{Jackett, TorznabItem, MOVIE_CATS, TV_CATS},
};

#[derive(Debug, thiserror::Error)]
pub enum TorrentError {
    #[error(transparent)]
    Indexer(#[from] torznab::TorznabError),
    #[error("could not download the .torrent: {0}")]
    Fetch(String),
    #[error(transparent)]
    Bencode(#[from] bencode::BencodeError),
    #[error("unknown release")]
    Unknown,
    #[error(transparent)]
    Db(#[from] sqlx::Error),
}

impl From<reqwest::Error> for TorrentError {
    fn from(e: reqwest::Error) -> Self {
        // Strip the URL so Jackett download links with `jackett_apikey` never end up in logs.
        Self::Fetch(e.without_url().to_string())
    }
}

/// The key a release is known by before its `.torrent` is downloaded.
pub fn release_key(item: &TorznabItem) -> String {
    match &item.infohash {
        Some(h) => h.clone(),
        None => {
            let src = item.link.as_deref().or(item.magnet.as_deref()).unwrap_or(&item.title);
            format!("u{}", &hex::encode(Sha1::digest(src.as_bytes()))[..39])
        }
    }
}

/// Running time of what the release holds, in minutes, when it can be worked out:
/// a film, one episode, or a season whose length is known.
fn running_minutes(p: &title::ParsedTitle, details: &TitleDetails) -> Option<f64> {
    let each = f64::from(details.runtime_min.filter(|m| *m > 0)?);
    match details.summary.kind {
        MediaKind::Movie => Some(each),
        MediaKind::Tv if p.single_episode => Some(each),
        MediaKind::Tv => {
            let season = p.season?;
            // A running season is only as long as what has aired.
            let aired = match details.last_episode {
                Some((s, e)) if s == season => Some(e),
                _ => details
                    .seasons
                    .iter()
                    .find(|s| s.number == season)
                    .map(|s| s.episode_count),
            }?;
            (aired > 0).then(|| each * f64::from(aired))
        }
    }
}

pub fn to_release(item: &TorznabItem, details: &TitleDetails) -> ReleaseOption {
    let p = parse_title(&item.title);
    let (bitrate_kbps, bitrate_approx) = match p.bitrate_kbps {
        Some(b) => (Some(b), false),
        None => (
            running_minutes(&p, details).and_then(|m| estimate_bitrate_kbps(item.size, m)),
            true,
        ),
    };
    let direct_play = p.codec.as_deref() == Some("H.264") && !p.hdr && p.quality != "2160p";
    ReleaseOption {
        infohash: release_key(item),
        title: item.title.clone(),
        quality: p.quality,
        size_bytes: item.size,
        seeders: item.seeders,
        voice: p.voice,
        language: p.language,
        tracker: if item.tracker.is_empty() {
            "—".into()
        } else {
            item.tracker.clone()
        },
        added: item.added.clone(),
        year: p.year,
        season: p.season,
        hdr: p.hdr,
        dolby_vision: p.dolby_vision,
        subtitles: p.subtitles,
        direct_play,
        codec: p.codec,
        bitrate_kbps,
        bitrate_approx,
        picked_by: Vec::new(),
        preloaded: false,
    }
}

static NON_VIDEO_KIND: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(аудиокнига|аудиоспектакль|радиоспектакль|audiobook|саундтрек|soundtrack|ost)\b").unwrap()
});
static NON_VIDEO_EXT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\b(mp3|мр3|flac|alac|ape|wav|m4a|m4b|fb2|epub|mobi|djvu)\b").unwrap());
static VIDEO_HINT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)\b(web-?dl|web-?rip|bd-?rip|bd-?remux|blu-?ray|bluray|dvd-?rip|hd-?rip|hd-?tv|tv-?rip|sat-?rip|1080[pi]?|720[pi]?|2160[pi]?|1440[pi]?|4k|uhd|hevc|[hx]\.?26[45]|avc|av1|mkv|mp4|avi|сезон)\b|\bs\d{1,2}e\d{1,3}\b",
    )
    .unwrap()
});
static TYPE_PREFIX: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^(?:[хтмд]/[фс])[\s:.-]+").unwrap());
static CUT_BOUNDARY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)(?:[\(\[]|\b(?:сезон[ыа]?|сери[яи]|season|complete|(?:2160|1440|1080|720|576|480)[pi]|4k|uhd|web-?dl(?:rip)?|web-?rip|hd-?rip|bd-?rip|bd-?remux|blu-?ray|bluray|dvd-?rip|dvd-?remux|dvd[59]|hd-?tv|tv-?rip|sat-?rip|hdr(?:10\+?|\b)|dolby|dovi|hevc|[hx]\.?26[45]|avc|av1|aac|ac3|dts|ddp?\d|atmos|10bit|multi|dub|mvo|dvo|avo|sub|subs)\b|\bs\d{1,2}(?:\s*[-–]\s*s?\d{1,2})?(?:e\d|\b))",
    )
    .unwrap()
});
static YEAR_TOKEN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\b(19[3-9]\d|20[0-4]\d)\b").unwrap());
static PAREN_GROUP: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\(([^()]+)\)").unwrap());

fn is_non_video(name: &str) -> bool {
    NON_VIDEO_KIND.is_match(name) || (NON_VIDEO_EXT.is_match(name) && !VIDEO_HINT.is_match(name))
}

fn regular_season_count(details: &TitleDetails) -> usize {
    details.seasons.iter().filter(|s| s.number > 0).count()
}

fn category_ok(item: &TorznabItem, details: &TitleDetails) -> bool {
    let std_cats: Vec<u32> = item.categories.iter().copied().filter(|&c| c < 100_000).collect();
    if std_cats.is_empty() {
        return true;
    }
    let has_movie = std_cats.iter().any(|c| (2000..3000).contains(c));
    let has_tv = std_cats.iter().any(|c| (5000..6000).contains(c));
    let has_other = std_cats.iter().any(|c| (8000..9000).contains(c));
    if !(has_movie || has_tv || has_other) {
        return false;
    }
    match details.summary.kind {
        MediaKind::Movie => has_movie || has_other,
        MediaKind::Tv if regular_season_count(details) > 1 => has_tv || has_other,
        MediaKind::Tv => true,
    }
}

fn fold_char(ch: char) -> &'static str {
    match ch {
        'ё' | 'Ё' => "е",
        'á' | 'à' | 'â' | 'ä' | 'ã' | 'å' => "a",
        'é' | 'è' | 'ê' | 'ë' => "e",
        'í' | 'ì' | 'î' | 'ï' => "i",
        'ó' | 'ò' | 'ô' | 'ö' | 'õ' | 'ø' => "o",
        'ú' | 'ù' | 'û' | 'ü' => "u",
        'ñ' => "n",
        'ç' => "c",
        'ý' | 'ÿ' => "y",
        'ž' => "z",
        'š' => "s",
        'č' => "c",
        'ř' => "r",
        'ł' => "l",
        'ß' => "ss",
        _ => "",
    }
}

/// Lowercase words separated by single spaces, folding `ё`→`е` and Latin diacritics,
/// dropping apostrophes and normalizing small Roman sequel numerals (`II`..`VI`).
fn normalize_words(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_space = true;
    for ch in s.chars() {
        if matches!(ch, '\'' | '’' | '`') {
            continue;
        }
        if ch.is_alphanumeric() {
            for lower in ch.to_lowercase() {
                let folded = fold_char(lower);
                if folded.is_empty() {
                    out.push(lower);
                } else {
                    out.push_str(folded);
                }
            }
            prev_space = false;
        } else if !prev_space {
            out.push(' ');
            prev_space = true;
        }
    }
    let words: Vec<&str> = out
        .split_whitespace()
        .map(|w| match w {
            "ii" => "2",
            "iii" => "3",
            "iv" => "4",
            "v" => "5",
            "vi" => "6",
            other => other,
        })
        .collect();
    words.join(" ")
}

fn has_ascii_alpha(s: &str) -> bool {
    s.bytes().any(|b| b.is_ascii_alphabetic())
}

fn strip_leading_bracket(s: &str) -> &str {
    let trimmed = s.trim_start();
    if trimmed.starts_with('[') {
        if let Some(end) = trimmed.find(']') {
            let rest = trimmed[end + 1..].trim_start();
            if !rest.is_empty() {
                return rest;
            }
        }
    }
    trimmed
}

fn cut_segment_prefix<'a>(seg: &'a str, title_years: &HashMap<String, usize>) -> &'a str {
    let mut end = seg.len();
    if let Some(m) = CUT_BOUNDARY.find(seg) {
        end = end.min(m.start());
    }
    let mut seen_years: HashMap<String, usize> = HashMap::new();
    for m in YEAR_TOKEN.find_iter(seg) {
        if m.start() >= end {
            break;
        }
        let y = m.as_str();
        let allowed = title_years.get(y).copied().unwrap_or(0);
        let count = seen_years.entry(y.to_owned()).or_insert(0);
        if *count < allowed {
            *count += 1;
        } else {
            end = end.min(m.start());
            break;
        }
    }
    &seg[..end]
}

fn trim_edition_suffix(seg: &str) -> &str {
    const SUFFIXES: &[&str] = &[
        "extended edition",
        "extended cut",
        "directors cut",
        "theatrical cut",
        "extended",
        "unrated",
        "theatrical",
        "remastered",
        "imax",
        "uncut",
        "complete",
        "full",
        "режиссерская версия",
        "полная версия",
        "расширенная версия",
        "театральная версия",
        "дубляж",
    ];
    let mut cur = seg.trim();
    loop {
        let mut changed = false;
        for suf in SUFFIXES {
            if let Some(rest) = cur.strip_suffix(suf) {
                let next = rest.trim();
                if !next.is_empty() {
                    cur = next;
                    changed = true;
                    break;
                }
            }
        }
        if !changed {
            return cur;
        }
    }
}

fn segment_matches(seg: &str, want: &str) -> bool {
    seg == want
        || want.strip_prefix("the ").is_some_and(|w| seg == w)
        || seg.strip_prefix("the ").is_some_and(|s| s == want)
}

fn is_ignored_latin_segment(seg: &str) -> bool {
    seg.chars().count() <= 2
        || title::is_known_studio(seg)
        || matches!(seg, "rus" | "eng" | "ukr" | "sub" | "subs" | "multi" | "original")
}

fn title_matches(name: &str, details: &TitleDetails) -> bool {
    let s = &details.summary;
    let want_ru = normalize_words(&s.title);
    let want_orig = s
        .original_title
        .as_deref()
        .map(normalize_words)
        .filter(|o| !o.is_empty());
    let mut wants: Vec<&str> = Vec::new();
    if !want_ru.is_empty() {
        wants.push(&want_ru);
    }
    if let Some(orig) = want_orig.as_deref() {
        if !wants.contains(&orig) {
            wants.push(orig);
        }
    }
    if wants.is_empty() {
        return false;
    }

    let mut title_years: HashMap<String, usize> = HashMap::new();
    for w in &wants {
        let mut per_title: HashMap<String, usize> = HashMap::new();
        for m in YEAR_TOKEN.find_iter(w) {
            *per_title.entry(m.as_str().to_owned()).or_insert(0) += 1;
        }
        for (y, count) in per_title {
            let entry = title_years.entry(y).or_insert(0);
            *entry = (*entry).max(count);
        }
    }

    // Episode releases look like `Show.S01E14.Episode.Title`: ignore text after `SxxExx`.
    let head = match title::episode_split_pos(name) {
        Some(pos) if normalize_words(&name[..pos]).chars().count() >= 2 => &name[..pos],
        _ => name,
    };
    let head = strip_leading_bracket(head);
    let head_no_prefix = TYPE_PREFIX.replace(head, "");

    let mut primary_segs: Vec<String> = Vec::new();
    let mut paren_segs: Vec<String> = Vec::new();
    for raw in head_no_prefix.split(['/', '|']) {
        let raw_trimmed = raw.trim();
        for cap in PAREN_GROUP.captures_iter(raw_trimmed) {
            let alt = normalize_words(&cap[1]);
            if !alt.is_empty() {
                paren_segs.push(alt);
            }
        }
        let cleaned = normalize_words(cut_segment_prefix(raw_trimmed, &title_years));
        if cleaned.is_empty() {
            if !primary_segs.is_empty() {
                break;
            }
            continue;
        }
        primary_segs.push(cleaned);
    }

    let any_matches = primary_segs.iter().any(|seg| {
        let trimmed = trim_edition_suffix(seg);
        wants
            .iter()
            .any(|want| segment_matches(seg, want) || segment_matches(trimmed, want))
    }) || paren_segs
        .iter()
        .any(|seg| wants.iter().any(|want| segment_matches(seg, want)));

    if !any_matches {
        return false;
    }

    // Reject localized renamings (`Сверхъестественное / First Light`) when a Latin title
    // segment contradicts a known Latin original title.
    if let Some(orig) = want_orig
        .as_deref()
        .filter(|o| has_ascii_alpha(o) && !title::has_cyrillic(o))
    {
        let latin_segs: Vec<&str> = primary_segs
            .iter()
            .map(String::as_str)
            .filter(|seg| has_ascii_alpha(seg) && !title::has_cyrillic(seg) && !is_ignored_latin_segment(seg))
            .collect();
        if !latin_segs.is_empty()
            && !latin_segs.iter().any(|seg| {
                let trimmed = trim_edition_suffix(seg);
                segment_matches(seg, orig) || segment_matches(trimmed, orig)
            })
        {
            return false;
        }
    }

    true
}

fn year_matches_target(target: i32, p: &title::ParsedTitle) -> bool {
    match (p.year, p.year_end) {
        (None, _) => true,
        (Some(start), Some(end)) => start - 1 <= target && target <= end + 1,
        (Some(have), None) => (target - have).abs() <= 1,
    }
}

fn year_and_kind_ok(details: &TitleDetails, p: &title::ParsedTitle) -> bool {
    let s = &details.summary;
    if s.kind == MediaKind::Movie {
        if p.has_series_marker {
            return false;
        }
        return match s.year {
            Some(want) => year_matches_target(want, p),
            None => true,
        };
    }

    let regular: Vec<_> = details.seasons.iter().filter(|st| st.number > 0).collect();
    let series_year = s.year;
    let season_years: Vec<i32> = regular.iter().filter_map(|st| st.year).collect();
    let min_year = series_year.into_iter().chain(season_years.iter().copied()).min();
    let max_year = series_year.into_iter().chain(season_years.iter().copied()).max();

    if let Some(season_num) = p.season {
        let season_year = regular.iter().find(|st| st.number == season_num).and_then(|st| st.year);
        if let Some(sy) = season_year {
            return year_matches_target(sy, p) || series_year.is_some_and(|y| year_matches_target(y, p));
        }
        let Some(have) = p.year else {
            return true;
        };
        let Some(lo) = min_year else {
            return true;
        };
        if have < lo - 1 {
            return false;
        }
        if !details.seasons.is_empty() {
            if let Some(hi) = max_year {
                let max_s = regular.iter().map(|st| st.number).max().unwrap_or(1);
                let upper = if season_num > max_s {
                    hi + 2 + (season_num - max_s) * 3
                } else {
                    hi + 2
                };
                return have <= upper;
            }
        }
        if let Some(y) = series_year {
            let upper = y + (season_num - 1).max(0) * 4 + 2;
            return have <= upper;
        }
        return true;
    }

    // `p.season.is_none()`: miniseries without `S01`, multi-season pack, or a wrong movie.
    let single_season = regular.len() <= 1 && !details.seasons.is_empty();
    if single_season {
        let target = series_year.or_else(|| regular.first().and_then(|st| st.year));
        return target.is_none_or(|y| year_matches_target(y, p));
    }

    if regular.len() > 1 {
        if !p.has_series_marker {
            return series_year.is_none_or(|y| p.year.is_some_and(|_| year_matches_target(y, p)));
        }
        if let (Some(lo), Some(hi), Some(have)) = (min_year, max_year, p.year) {
            let end = p.year_end.unwrap_or(have);
            return end >= lo - 1 && have <= hi + 2;
        }
        return true;
    }

    // `details.seasons` is empty (synthetic tests or partial metadata).
    match (series_year, p.year) {
        (Some(y), Some(_)) if !p.has_series_marker => year_matches_target(y, p),
        (Some(y), Some(have)) => have >= y - 1,
        _ => true,
    }
}

/// Whether a release name is about this title: it matches the name, looks like
/// video (filtering out OSTs/audiobooks from category `Other`), fits the
/// media kind, and matches the title's release or season years.
pub fn is_about(name: &str, details: &TitleDetails) -> bool {
    if is_non_video(name) {
        return false;
    }
    let parsed = parse_title(name);
    year_and_kind_ok(details, &parsed) && title_matches(name, details)
}

fn title_queries(details: &TitleDetails, year: Option<i32>) -> Vec<String> {
    let s = &details.summary;
    let format_q = |name: &str| match year {
        Some(y) => {
            let ys = y.to_string();
            let already = name.split(|c: char| !c.is_ascii_digit()).any(|part| part == ys);
            if already {
                name.to_owned()
            } else {
                format!("{name} {ys}")
            }
        }
        None => name.to_owned(),
    };
    let mut queries = vec![format_q(&s.title)];
    if let Some(o) = s
        .original_title
        .as_ref()
        .filter(|o| normalize(o) != normalize(&s.title))
    {
        queries.push(format_q(o));
    }
    queries
}

fn search_plan(details: &TitleDetails) -> (&'static str, Vec<String>, Option<Vec<String>>) {
    let s = &details.summary;
    let regular_seasons = regular_season_count(details);
    let primary_year = s
        .year
        .or_else(|| details.seasons.iter().find(|st| st.number > 0).and_then(|st| st.year));
    match s.kind {
        MediaKind::Movie => (MOVIE_CATS, title_queries(details, primary_year), None),
        MediaKind::Tv if regular_seasons == 1 => (
            TV_CATS,
            title_queries(details, primary_year),
            primary_year.map(|_| title_queries(details, None)),
        ),
        MediaKind::Tv if regular_seasons > 1 => (
            TV_CATS,
            title_queries(details, None),
            primary_year.map(|y| title_queries(details, Some(y))),
        ),
        MediaKind::Tv => (TV_CATS, title_queries(details, None), None),
    }
}

async fn run_queries(
    indexers: &[Jackett],
    queries: &[String],
    cats: &str,
    details: &TitleDetails,
) -> (HashMap<String, TorznabItem>, bool, Option<torznab::TorznabError>) {
    let tasks = indexers
        .iter()
        .flat_map(|idx| queries.iter().map(move |q| idx.search(q, cats)));
    let results = futures_util::future::join_all(tasks).await;
    let mut seen: HashMap<String, TorznabItem> = HashMap::new();
    let mut any_ok = false;
    let mut first_err = None;
    for r in results {
        match r {
            Ok(items) => {
                any_ok = true;
                for it in items
                    .into_iter()
                    .filter(|it| category_ok(it, details) && is_about(&it.title, details))
                {
                    seen.entry(release_key(&it))
                        .and_modify(|existing| {
                            existing.seeders = existing.seeders.max(it.seeders);
                            if existing.link.is_none() {
                                existing.link = it.link.clone();
                            }
                            if existing.magnet.is_none() {
                                existing.magnet = it.magnet.clone();
                            }
                        })
                        .or_insert(it);
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "indexer search failed");
                first_err = first_err.or(Some(e));
            }
        }
    }
    (seen, any_ok, first_err)
}

/// Searches every configured indexer for the title, by its Russian and original names.
pub async fn search(indexers: &[Jackett], details: &TitleDetails) -> Result<Vec<TorznabItem>, TorrentError> {
    if indexers.is_empty() {
        return Ok(Vec::new());
    }
    let (cats, queries, fallback) = search_plan(details);
    let (mut seen, mut any_ok, mut first_err) = run_queries(indexers, &queries, cats, details).await;

    // For TV shows: if a single-season query with year found nothing (e.g. releases omit year),
    // or a multi-season yearless query was crowded out by a namesake film, retry the fallback queries.
    if seen.is_empty() && any_ok {
        if let Some(fb_queries) = fallback {
            let (fb_seen, fb_ok, fb_err) = run_queries(indexers, &fb_queries, cats, details).await;
            seen = fb_seen;
            any_ok |= fb_ok;
            first_err = first_err.or(fb_err);
        }
    }

    if seen.is_empty() && !any_ok {
        if let Some(e) = first_err {
            return Err(e.into());
        }
    }
    let mut items: Vec<_> = seen.into_values().collect();
    items.sort_by_key(|it| std::cmp::Reverse(it.seeders));
    Ok(items)
}

/// Searches for the title unless that was done recently; the search also records
/// where each release can be downloaded from. Several callers at once share one search.
pub async fn search_cached(
    cache: &search_cache::SearchCache,
    pool: &PgPool,
    indexers: &[Jackett],
    details: &TitleDetails,
) -> Result<std::sync::Arc<Vec<TorznabItem>>, TorrentError> {
    let key = (details.summary.kind, details.summary.tmdb_id);
    cache
        .get_or_search(key, || async {
            let items = search(indexers, details).await?;
            save_sources(pool, &items).await?;
            Ok(std::sync::Arc::new(items))
        })
        .await
}

/// Remembers where each release can be downloaded from using a single batch upsert.
pub async fn save_sources(pool: &PgPool, items: &[TorznabItem]) -> Result<(), sqlx::Error> {
    if items.is_empty() {
        return Ok(());
    }
    let mut seen = std::collections::HashMap::<String, usize>::with_capacity(items.len());
    let mut keys: Vec<String> = Vec::with_capacity(items.len());
    let mut titles: Vec<String> = Vec::with_capacity(items.len());
    let mut links: Vec<Option<String>> = Vec::with_capacity(items.len());
    let mut magnets: Vec<Option<String>> = Vec::with_capacity(items.len());
    let mut hashes: Vec<Option<String>> = Vec::with_capacity(items.len());
    for it in items {
        let key = release_key(it);
        if let Some(&idx) = seen.get(&key) {
            titles[idx] = it.title.clone();
            links[idx] = it.link.clone();
            if it.magnet.is_some() {
                magnets[idx] = it.magnet.clone();
            }
            if it.infohash.is_some() {
                hashes[idx] = it.infohash.clone();
            }
        } else {
            let idx = keys.len();
            seen.insert(key.clone(), idx);
            keys.push(key);
            titles.push(it.title.clone());
            links.push(it.link.clone());
            magnets.push(it.magnet.clone());
            hashes.push(it.infohash.clone());
        }
    }
    sqlx::query(
        "INSERT INTO torrent_source (key, title, link, magnet, infohash)
         SELECT * FROM UNNEST($1::text[], $2::text[], $3::text[], $4::text[], $5::text[])
         ON CONFLICT (key) DO UPDATE SET title = EXCLUDED.title, link = EXCLUDED.link,
             magnet = COALESCE(EXCLUDED.magnet, torrent_source.magnet), updated_at = now()",
    )
    .bind(&keys)
    .bind(&titles)
    .bind(&links)
    .bind(&magnets)
    .bind(&hashes)
    .execute(pool)
    .await?;
    Ok(())
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Source {
    pub key: String,
    pub title: String,
    pub link: Option<String>,
    pub magnet: Option<String>,
    pub infohash: Option<String>,
    pub torrent: Option<Vec<u8>>,
}

pub async fn source(pool: &PgPool, key: &str) -> Result<Option<Source>, sqlx::Error> {
    sqlx::query_as("SELECT key, title, link, magnet, infohash, torrent FROM torrent_source WHERE key = $1")
        .bind(key)
        .fetch_optional(pool)
        .await
}

/// Resolves a list of release keys via `torrent_source`, returning `key -> real infohash`.
pub async fn known_infohashes(pool: &PgPool, keys: &[String]) -> Result<HashMap<String, String>, sqlx::Error> {
    if keys.is_empty() {
        return Ok(HashMap::new());
    }
    let rows: Vec<(String, String)> =
        sqlx::query_as("SELECT key, infohash FROM torrent_source WHERE key = ANY($1) AND infohash IS NOT NULL")
            .bind(keys)
            .fetch_all(pool)
            .await?;
    Ok(rows.into_iter().collect())
}

/// Upgrades any stored `release_choice` rows pointing to a synthetic `u...` key to its real infohash.
pub async fn rebind_synthetic_hash(pool: &PgPool, key: &str, hash: &str) {
    if key.is_empty() || hash.is_empty() || key.eq_ignore_ascii_case(hash) {
        return;
    }
    let _ = sqlx::query("UPDATE release_choice SET infohash = $2 WHERE infohash = $1")
        .bind(key)
        .bind(hash)
        .execute(pool)
        .await;
}

/// Turns a release key into its info-hash, downloading the `.torrent` when the
/// indexer did not give the hash. Also keeps the `.torrent` for TorrServer.
pub async fn resolve(
    pool: &PgPool,
    http: &reqwest::Client,
    key: &str,
    indexers: &[Jackett],
) -> Result<Source, TorrentError> {
    let src = source(pool, key).await?;
    if let Some(s) = src.as_ref().filter(|s| s.infohash.is_some() && s.torrent.is_some()) {
        return Ok(s.clone());
    }
    let Some(mut src) = src else {
        // Never seen by the indexer (demo data, or a magnet typed by hand).
        return if is_infohash(key) {
            Ok(Source {
                key: key.into(),
                title: String::new(),
                link: None,
                magnet: Some(format!("magnet:?xt=urn:btih:{key}")),
                infohash: Some(key.into()),
                torrent: None,
            })
        } else {
            Err(TorrentError::Unknown)
        };
    };
    if let Some(link) = src.link.clone() {
        if let Some(j) = indexers.iter().find(|j| j.owns_link(&link)) {
            j.ensure_ready().await;
        }
        match fetch(http, &link).await {
            Ok(Fetched::Torrent(bytes)) => {
                src.infohash = Some(bencode::parse_torrent(&bytes)?.infohash);
                src.torrent = Some(bytes);
            }
            Ok(Fetched::Magnet(m)) => {
                src.infohash = bencode::magnet_infohash(&m);
                src.magnet = Some(m);
            }
            // The magnet is enough when the indexer gave one.
            Err(e) if src.infohash.is_none() => return Err(e),
            Err(e) => tracing::warn!(key, error = %e, "no .torrent, using the magnet"),
        }
    }
    let hash = src.infohash.clone().ok_or(TorrentError::Unknown)?;
    for k in [key, hash.as_str()] {
        sqlx::query(
            "INSERT INTO torrent_source (key, title, link, magnet, infohash, torrent) VALUES ($1, $2, $3, $4, $5, $6)
             ON CONFLICT (key) DO UPDATE SET magnet = EXCLUDED.magnet, infohash = EXCLUDED.infohash,
                 torrent = EXCLUDED.torrent, updated_at = now()",
        )
        .bind(k)
        .bind(&src.title)
        .bind(&src.link)
        .bind(&src.magnet)
        .bind(&hash)
        .bind(&src.torrent)
        .execute(pool)
        .await?;
    }
    Ok(src)
}

enum Fetched {
    Torrent(Vec<u8>),
    Magnet(String),
}

/// Maximum `.torrent` file size accepted to prevent OOM from malformed responses.
const MAX_TORRENT_BYTES: usize = 2 * 1024 * 1024;

fn is_non_public_ip(ip: std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(v4) => {
            let oct = v4.octets();
            v4.is_private()
                || v4.is_loopback()
                || v4.is_link_local()
                || v4.is_broadcast()
                || v4.is_unspecified()
                || v4.is_multicast()
                || (oct[0] == 100 && (oct[1] & 0xC0) == 64) // CGNAT 100.64.0.0/10
        }
        std::net::IpAddr::V6(v6) => {
            v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || (v6.segments()[0] & 0xfe00) == 0xfc00 // ULA fc00::/7
                || (v6.segments()[0] & 0xffc0) == 0xfe80 // link-local fe80::/10
                || v6.to_ipv4_mapped().is_some_and(|v4| is_non_public_ip(v4.into()))
        }
    }
}

/// Rejects redirects to loopback, private, link-local, or internal Docker hosts (SSRF guard).
async fn validate_redirect_target(next: &reqwest::Url, initial_host: Option<&str>) -> Result<(), TorrentError> {
    if !matches!(next.scheme(), "http" | "https") {
        return Err(TorrentError::Unknown);
    }
    let Some(host) = next.host_str() else {
        return Err(TorrentError::Unknown);
    };
    // Allow staying on the same trusted host as the initial link (e.g., internal Jackett).
    if initial_host.is_some_and(|h| h.eq_ignore_ascii_case(host)) {
        return Ok(());
    }
    let lower = host.to_ascii_lowercase();
    if lower == "localhost" || lower.ends_with(".local") || lower.ends_with(".internal") || !lower.contains('.') {
        return Err(TorrentError::Fetch("redirect to internal host blocked".into()));
    }
    if let Ok(ip) = host.trim_matches(['[', ']']).parse::<std::net::IpAddr>() {
        if is_non_public_ip(ip) {
            return Err(TorrentError::Fetch("redirect to non-public IP blocked".into()));
        }
        return Ok(());
    }
    let port = next.port_or_known_default().unwrap_or(80);
    let addrs = tokio::net::lookup_host((host, port))
        .await
        .map_err(|_| TorrentError::Fetch("redirect host resolution failed".into()))?;
    let mut any = false;
    for addr in addrs {
        any = true;
        if is_non_public_ip(addr.ip()) {
            return Err(TorrentError::Fetch("redirect to non-public IP blocked".into()));
        }
    }
    if !any {
        return Err(TorrentError::Fetch("redirect host resolved to no addresses".into()));
    }
    Ok(())
}

/// Downloads a `.torrent`. Jackett answers some links with a redirect to a magnet.
async fn fetch(http: &reqwest::Client, link: &str) -> Result<Fetched, TorrentError> {
    use futures_util::StreamExt;
    let initial_host = reqwest::Url::parse(link)
        .ok()
        .and_then(|u| u.host_str().map(str::to_owned));
    let mut url = link.to_owned();
    for _ in 0..5 {
        let resp = http
            .get(&url)
            .timeout(std::time::Duration::from_secs(30))
            .send()
            .await?;
        if !resp.status().is_redirection() {
            let resp = resp.error_for_status()?;
            if resp.content_length().is_some_and(|n| n > MAX_TORRENT_BYTES as u64) {
                return Err(TorrentError::Fetch("torrent file exceeded size limit".into()));
            }
            let mut bytes = Vec::with_capacity(resp.content_length().unwrap_or(0).min(128 * 1024) as usize);
            let mut stream = resp.bytes_stream();
            while let Some(chunk) = stream.next().await {
                let chunk = chunk?;
                if bytes.len().saturating_add(chunk.len()) > MAX_TORRENT_BYTES {
                    return Err(TorrentError::Fetch("torrent file exceeded size limit".into()));
                }
                bytes.extend_from_slice(&chunk);
            }
            return Ok(Fetched::Torrent(bytes));
        }
        let to = resp
            .headers()
            .get(reqwest::header::LOCATION)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default();
        if to.starts_with("magnet:") {
            return Ok(Fetched::Magnet(to.to_owned()));
        }
        let next = resp.url().join(to).map_err(|_| TorrentError::Unknown)?;
        validate_redirect_target(&next, initial_host.as_deref()).await?;
        url = next.to_string();
    }
    Err(TorrentError::Unknown)
}

/// HTTP client that does not follow redirects, so magnet redirects can be read.
pub fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .user_agent("kino")
        .build()
        .expect("http client")
}

#[cfg(test)]
mod tests {
    use kino_core::{SeasonSummary, TitleSummary};

    use super::*;

    fn details(kind: MediaKind, title: &str, original: &str, year: Option<i32>) -> TitleDetails {
        TitleDetails {
            summary: TitleSummary {
                kind,
                tmdb_id: 1,
                title: title.into(),
                original_title: Some(original.into()),
                year,
                poster_path: None,
                backdrop_path: None,
                rating: None,
                overview: None,
                user_rating: None,
                user_status: None,
                peers: Vec::new(),
                preloaded: false,
            },
            genres: vec![],
            runtime_min: None,
            tagline: None,
            seasons: vec![],
            imdb_id: None,
            last_episode: None,
            series_status: None,
            release_date: None,
            next_episode: None,
        }
    }

    fn season(number: i32, year: i32) -> SeasonSummary {
        SeasonSummary {
            number,
            name: format!("Сезон {number}"),
            episode_count: 10,
            year: Some(year),
        }
    }

    #[test]
    fn filters_non_video_from_other_category() {
        let movie = details(MediaKind::Movie, "Зелёная миля", "The Green Mile", Some(1999));
        assert!(is_about("Зеленая миля / The Green Mile (1999) BDRip 1080p", &movie));
        assert!(is_about("Зеленая миля / The Green Mile (1999) DVDRip XviD MP3", &movie));
        assert!(!is_about(
            "OST - Зеленая миля / The Green Mile Soundtrack [Thomas Newman] (1999) MP3",
            &movie
        ));
        assert!(!is_about("Зелёная миля (1999) Аудиокнига MP3", &movie));

        let tv = details(
            MediaKind::Tv,
            "Декстер: Воскрешение",
            "Dexter: Resurrection",
            Some(2025),
        );
        assert!(is_about(
            "Декстер: Воскрешение / Dexter: Resurrection [S02E01-04 of 10] (2026) WEBRip от Kerob | L2",
            &tv
        ));
        assert!(!is_about("Dexter: Resurrection Official Soundtrack FLAC", &tv));
    }

    #[test]
    fn odyssey_1997_rejects_nolan_2026_and_unrelated_titles() {
        let mut odyssey_1997 = details(MediaKind::Tv, "Одиссей", "The Odyssey", Some(1997));
        odyssey_1997.seasons = vec![season(1, 1997)];

        assert!(is_about(
            "Одиссей / Одиссея / The Odyssey (1997) DVDRemux | P, L1 | Полная версия",
            &odyssey_1997
        ));
        assert!(is_about("Одиссей 1997 [scarabey org]", &odyssey_1997));
        assert!(is_about("The.Odyssey.1997.S01.DVDRip.x264", &odyssey_1997));
        assert!(is_about(
            "The Odyssey 1997 Part 1 Upscaled dvdrip 2160p sdr hevc",
            &odyssey_1997
        ));

        // Nolan's 2026 movie, other Odyssey shows/films, episode titles and audiobooks must be rejected.
        for bad in [
            "Одиссея / The Odyssey (2026) BDRip 1080p | D, P, A",
            "The.Odyssey.2026.2160p.Hybrid.UHD.BluRay.REMUX.DV.HDR10+.H265-BTM",
            "Доктор Одиссей / Doctor Odyssey [S1] (2024) WEB-DL 1080p | P-TVShows",
            "Одиссея / The Odyssey [S01] (1992) WEBRip 1080p | P",
            "Одиссей / Odysseus, S1E1-12 of 12 (2013) WEB-DL",
            "Где ты был, Одиссей? [01-03 из 03] (1978) DVDRip-Киномагия",
            "Звездный путь: Одиссея / Star Quest: The Odyssey / Space Trek (2009) DVDRip",
            "Arrow.S01E14.The.Odyssey.480p.WEB-DL.x264-mSD",
            "Krapopolis S02E06 National Lampoons The Odyssey 1080p HULU WEB-DL",
            "Карелин Антон Одиссей Фокс 6 Звёздный зверь [Амир Шакиров]",
            "Василий Звягинцев Одиссей покидает Итаку 20 Фазовый переход 2019",
        ] {
            assert!(!is_about(bad, &odyssey_1997), "should reject `{bad}`");
        }
    }

    #[test]
    fn odyssey_2026_movie_rejects_1997_miniseries_and_series_releases() {
        let odyssey_2026 = details(MediaKind::Movie, "Одиссея", "The Odyssey", Some(2026));
        assert!(is_about(
            "Одиссея / The Odyssey (2026) BDRip 1080p | D-Продубляж",
            &odyssey_2026
        ));
        assert!(is_about(
            "The.Odyssey.2026.1080p.Bluray.10Bit.x265-NeoNoir",
            &odyssey_2026
        ));
        assert!(!is_about(
            "Одиссей / Одиссея / The Odyssey (1997) DVDRemux",
            &odyssey_2026
        ));
        assert!(!is_about("Космические одиссеи [S1] (2026) WEBRip 1080p", &odyssey_2026));
        assert!(!is_about("The.Odyssey.2026.S01.1080p.WEB-DL", &odyssey_2026));
    }

    #[test]
    fn supernatural_tv_rejects_homonym_movies_and_keeps_all_seasons() {
        let mut supernatural = details(MediaKind::Tv, "Сверхъестественное", "Supernatural", Some(2005));
        supernatural.seasons = (1..=15).map(|n| season(n, 2004 + n)).collect();

        for ok in [
            "Сверхъестественное / Supernatural [S01-15] (2005-2020) BDRip-HEVC 1080p | LostFilm",
            "Сверхъестественное / Supernatural, S1-15E1-327 of 327 (2005-2020) HDRip, WEB-DL",
            "Сверхъестественное / Supernatural [S12] (2016) WEB-DL 1080p-LostFilm",
            "Сверхъестественное / Supernatural, S12E1-23 of 23 (2016-2017) WEB-DL-LostFilm",
            "Supernatural 2005 Season 13 S13 Extras 1080p BluRay x265",
            "Supernatural S07E05 Shut Up Dr Phil 1080p BluRay",
            "Supernatural (Сверхъестественное) s15e01 04",
        ] {
            assert!(is_about(ok, &supernatural), "should accept `{ok}`");
        }

        for bad in [
            "Сверхъестественное. Разлом времени / Taklee Genesis x Worlds Collide (2024) WEB-DL 1080p | D",
            "Сверхъестественное. Знаки / Something in the Dirt (2022) WEB-DL 1080p",
            "Сверхъестественное. Прибытие / Cosmic Dawn (2022) BDRip 720p | D",
            "Сверхъестественное / Первый свет / First Light (2018) BDRip от Twister & ExKinoRay-iTunes",
            "Сверхъестественное / Заблудившиеся / Stray (2019) WEB-DL 1080p от ELEKTRI4KA | IVI",
            "Сверхъестественное: Анимация / SUPERNATURAL: THE ANIMATION [S01] (2011) BDRip",
        ] {
            assert!(!is_about(bad, &supernatural), "should reject `{bad}`");
        }
    }

    #[test]
    fn titles_with_years_diacritics_and_short_words() {
        let space_odyssey = details(
            MediaKind::Movie,
            "2001 год: Космическая одиссея",
            "2001: A Space Odyssey",
            Some(1968),
        );
        assert!(is_about(
            "2001: Космическая одиссея / 2001: A Space Odyssey (1968) BDRip 1080p",
            &space_odyssey
        ));
        assert!(is_about("2001.A.Space.Odyssey.1968.1080p.BluRay", &space_odyssey));
        assert!(!is_about("2001.A.Space.Odyssey.2026.1080p.BluRay", &space_odyssey));

        let it = details(MediaKind::Movie, "Оно", "It", Some(2017));
        assert!(is_about("Оно / It (2017) BDRip 1080p", &it));
        assert!(!is_about("Movie with subtitles 10bit (2017) BDRip", &it));
        assert!(!is_about("Хроники (2017) BDRip", &it));

        let leon = details(MediaKind::Movie, "Леон", "Léon: The Professional", Some(1994));
        assert!(is_about("Леон / Leon: The Professional (1994) Extended 1080p", &leon));
    }

    #[test]
    fn category_filter_respects_torznab_categories() {
        let mut tv_multi = details(MediaKind::Tv, "Сверхъестественное", "Supernatural", Some(2005));
        tv_multi.seasons = vec![season(1, 2005), season(2, 2006)];
        let mut tv_single = details(MediaKind::Tv, "Одиссей", "The Odyssey", Some(1997));
        tv_single.seasons = vec![season(1, 1997)];
        let movie = details(MediaKind::Movie, "Матрица", "The Matrix", Some(1999));

        let item = |cats: Vec<u32>| TorznabItem {
            title: "t".into(),
            categories: cats,
            ..TorznabItem::default()
        };
        assert!(category_ok(&item(vec![]), &tv_multi));
        assert!(category_ok(&item(vec![8000, 100003]), &tv_multi));
        assert!(!category_ok(&item(vec![3000]), &movie));
        assert!(!category_ok(&item(vec![7000]), &tv_single));
        assert!(!category_ok(&item(vec![2000]), &tv_multi));
        assert!(category_ok(&item(vec![2000]), &tv_single));
        assert!(!category_ok(&item(vec![5000]), &movie));
    }

    #[test]
    fn search_plan_includes_year_for_movies_and_single_season_tv() {
        let movie = details(MediaKind::Movie, "Одиссея", "The Odyssey", Some(2026));
        let (cats, q, fb) = search_plan(&movie);
        assert_eq!(cats, MOVIE_CATS);
        assert_eq!(q, vec!["Одиссея 2026", "The Odyssey 2026"]);
        assert_eq!(fb, None);

        let mut single_tv = details(MediaKind::Tv, "Одиссей", "The Odyssey", Some(1997));
        single_tv.seasons = vec![season(1, 1997)];
        let (cats, q, fb) = search_plan(&single_tv);
        assert_eq!(cats, TV_CATS);
        assert_eq!(q, vec!["Одиссей 1997", "The Odyssey 1997"]);
        assert_eq!(fb, Some(vec!["Одиссей".to_owned(), "The Odyssey".to_owned()]));

        let mut multi_tv = details(MediaKind::Tv, "Сверхъестественное", "Supernatural", Some(2005));
        multi_tv.seasons = vec![season(1, 2005), season(2, 2006)];
        let (cats, q, fb) = search_plan(&multi_tv);
        assert_eq!(cats, TV_CATS);
        assert_eq!(q, vec!["Сверхъестественное", "Supernatural"]);
        assert_eq!(
            fb,
            Some(vec![
                "Сверхъестественное 2005".to_owned(),
                "Supernatural 2005".to_owned()
            ])
        );
    }

    #[test]
    fn categories_include_other_for_russian_trackers() {
        assert!(MOVIE_CATS.split(',').any(|c| c == "8000"));
        assert!(MOVIE_CATS.split(',').any(|c| c == "8010"));
        assert!(TV_CATS.split(',').any(|c| c == "8000"));
        assert!(TV_CATS.split(',').any(|c| c == "8010"));
    }
}
