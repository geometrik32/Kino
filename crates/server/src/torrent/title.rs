//! Reads quality, voice-over, season and the rest from a release name, the way
//! Russian trackers write them, e.g.
//! `Во все тяжкие / Breaking Bad / Сезон: 2 / Серии: 1-13 из 13 [2009, США, BDRip 1080p] MVO (LostFilm) + Sub`.

use std::sync::LazyLock;

use regex::Regex;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ParsedTitle {
    pub quality: String,
    pub hdr: bool,
    pub dolby_vision: bool,
    pub codec: Option<String>,
    pub voice: String,
    pub language: String,
    pub subtitles: bool,
    /// Single season; `None` for films and multi-season packs.
    pub season: Option<i32>,
    pub year: Option<i32>,
    /// Upper bound of a year span such as `(2005-2020)` when stated.
    pub year_end: Option<i32>,
    /// Bitrate in kbit/s when the name states it, e.g. "25.4 Mbps".
    pub bitrate_kbps: Option<u32>,
    /// One episode of a series, not a whole season.
    pub single_episode: bool,
    /// True when the name mentions seasons or episodes (`S01`, `S01-S05`, `Сезон`, `S01E01`).
    pub has_series_marker: bool,
}

/// Studios recognised by name, in the spelling shown to the user.
const STUDIOS: &[(&str, &str)] = &[
    ("lostfilm", "LostFilm"),
    ("newstudio", "NewStudio"),
    ("кубик в кубе", "Кубик в Кубе"),
    ("кубиквкубе", "Кубик в Кубе"),
    ("hdrezka", "HDRezka"),
    ("rezka", "HDRezka"),
    ("jaskier", "Jaskier"),
    ("alexfilm", "AlexFilm"),
    ("baibako", "BaibaKo"),
    ("tvshows", "TVShows"),
    ("кураж-бамбей", "Кураж-Бамбей"),
    ("amedia", "Amedia"),
    ("coldfilm", "ColdFilm"),
    ("пифагор", "Пифагор"),
    ("red head sound", "Red Head Sound"),
    ("ideafilm", "IdeaFilm"),
    ("novamedia", "Novamedia"),
    ("сыендук", "Сыендук"),
    ("гоблин", "Гоблин"),
    ("кравец", "Кравец"),
    ("сербин", "Сербин"),
    ("дохалов", "Дохалов"),
    ("живов", "Живов"),
];

static RE: LazyLock<Regexes> = LazyLock::new(Regexes::new);

struct Regexes {
    quality: Regex,
    season: Regex,
    brackets: Regex,
    year: Regex,
    year_range: Regex,
    dv: Regex,
    hdr: Regex,
    hevc: Regex,
    avc: Regex,
    av1: Regex,
    dub: Regex,
    dub_cyr: Regex,
    mvo: Regex,
    dvo: Regex,
    avo: Regex,
    rus: Regex,
    orig: Regex,
    sub_lang: Regex,
    sub: Regex,
    ukr_only: Regex,
    mbps: Regex,
    kbps: Regex,
    episode: Regex,
    x_episode: Regex,
    sxx_exx: Regex,
    complete_series: Regex,
}

impl Regexes {
    fn new() -> Self {
        let re = |s: &str| Regex::new(s).expect("valid regex");
        Self {
            quality: re(r"(?i)\b(2160|1440|1080|720|576|480)[pi]\b|\b(4k|uhd)\b"),
            season: re(
                r"(?i)(?:сезон[ыа]?[:\s]*(\d{1,2})(?:\s*[-–]\s*(\d{1,2}))?)|(?:\b(\d{1,2})(?:\s*[-–]\s*(\d{1,2}))?\s*сезон)|(?:\bs(\d{1,2})(?:\s*[-–]\s*s?(\d{1,2}))?(?:e\d|\b))|(?:season\s*(\d{1,2}))",
            ),
            brackets: re(r"[\(\[]([^\)\]]+)[\)\]]"),
            year: re(r"\b(19[3-9]\d|20[0-4]\d)\b"),
            year_range: re(r"\b(19[3-9]\d|20[0-4]\d)\s*[-–]\s*(19[3-9]\d|20[0-4]\d)\b"),
            dv: re(r"(?i)dolby\s*vision|\bdovi\b|\bdv\b"),
            hdr: re(r"(?i)\bhdr(10\+?|10plus)?\b"),
            hevc: re(r"(?i)\b(hevc|[hx]\.?265)\b"),
            avc: re(r"(?i)\b(avc|[hx]\.?264)\b"),
            av1: re(r"(?i)\bav1\b"),
            dub: re(r"(?i)\b(dub|dubbed|дубляж|дублированный|дб)\b"),
            dub_cyr: re(r"(?i)(?:^|[\s|,/\[\(])[дd](?:$|[\s|,\]\)])"),
            mvo: re(r"(?i)\b(mvo|многоголосый|пм|лм)\b"),
            dvo: re(r"(?i)\b(dvo|двухголосый|пд|лд)\b"),
            avo: re(r"(?i)\b(avo|авторский|ао|ап)\b"),
            rus: re(r"(?i)\b(rus|russian|рус|русск(?:ий|ая|ое))\b"),
            orig: re(r"(?i)\b(orig|original|оригинал)\b"),
            sub_lang: re(
                r"(?i)\b(?:sub|subs|subtitles?|субтитры?|ст)[\s.:]*(?:\([^)]*\)|\[[^\]]*\]|(?:rus|russian|рус|eng)\b)|\b(?:rus|russian|рус)[\s.:]+(?:sub|subs|subtitles?|суб)\b",
            ),
            sub: re(r"(?i)\b(sub|subs|субтитры|ст)\b"),
            ukr_only: re(r"(?i)\b(ukr|укр)\b"),
            mbps: re(r"(?i)(\d{1,3}(?:[.,]\d{1,2})?)\s*(?:mbps|mbit/s|mbit|мбит/с|мбит/сек|мбит)"),
            kbps: re(r"(?i)(\d{3,6})\s*(?:kbps|kbit/s|kbit|кбит/с|кбит/сек|кбит)"),
            episode: re(
                r"(?i)(?:\bs\d{1,2}e\d{1,3}(\s*[-–]\s*e?\d{1,3})?)|(?:\bсери[яи][:\s]*\d{1,3}(\s*[-–]\s*\d{1,3})?)",
            ),
            x_episode: re(r"(?i)\b(?:[1-9]|[12]\d)x\d{2,3}\b"),
            sxx_exx: re(
                r"(?i)(?:\bs\d{1,2}(?:\s*[-–]\s*s?\d{1,2})?\s*[._-]?\s*e\d{1,3}\b|\b(?:[1-9]|[12]\d)x\d{2,3}\b)",
            ),
            complete_series: re(r"(?i)\bcomplete\s+series\b|\bсезоны\b|\bвсе\s+сезоны\b"),
        }
    }
}

pub fn parse_title(name: &str) -> ParsedTitle {
    let re = &*RE;
    let lower = name.to_lowercase();

    let quality = re
        .quality
        .captures_iter(name)
        .filter_map(|c| match c.get(1) {
            Some(n) => n.as_str().parse::<u32>().ok(),
            None => Some(2160),
        })
        .max()
        .map(|q| format!("{q}p"))
        .unwrap_or_else(|| "SD".into());

    let season = re.season.captures(name).and_then(|c| {
        let groups = [(1, 2), (3, 4), (5, 6), (7, 0)];
        groups.iter().find_map(|&(a, b)| {
            let first = c.get(a)?.as_str().parse::<i32>().ok()?;
            match c.get(b).and_then(|m| m.as_str().parse::<i32>().ok()) {
                // A range of seasons is a pack, not one season.
                Some(last) if last != first => Some(None),
                _ => Some(Some(first)),
            }
        })
    });

    let codec = if re.hevc.is_match(name) {
        Some("HEVC")
    } else if re.av1.is_match(name) {
        Some("AV1")
    } else if re.avc.is_match(name) {
        Some("H.264")
    } else {
        None
    };

    let has_cyr = has_cyrillic(name);
    let audio_text = re.sub_lang.replace_all(name, " ");
    let studio = STUDIOS.iter().find(|(k, _)| lower.contains(k)).map(|(_, v)| *v);
    let voice = match studio {
        Some(s) => s.to_string(),
        None if re.dub.is_match(name) => "Дубляж".into(),
        None if re.mvo.is_match(name) => "Многоголосый".into(),
        None if re.dvo.is_match(name) => "Двухголосый".into(),
        None if re.avo.is_match(name) => "Авторский".into(),
        None if has_cyr && re.dub_cyr.is_match(name) => "Дубляж".into(),
        None if re.rus.is_match(&audio_text) => "Русский".into(),
        None if re.orig.is_match(&audio_text) => "Оригинал".into(),
        None if has_cyr => "Русский".into(),
        None => "Оригинал".into(),
    };
    let language = if voice == "Оригинал" {
        "Английский"
    } else if re.ukr_only.is_match(name)
        && studio.is_none()
        && !re.rus.is_match(&audio_text)
        && !lower.contains("rus")
        && !lower.contains("рус")
    {
        "Украинский"
    } else {
        "Русский"
    };

    let (year, year_end) = extract_years(name, re);
    let has_series_marker = re.season.is_match(name)
        || re.episode.is_match(name)
        || re.x_episode.is_match(name)
        || re.complete_series.is_match(name);

    ParsedTitle {
        quality,
        hdr: re.hdr.is_match(name) || re.dv.is_match(name),
        dolby_vision: re.dv.is_match(name),
        codec: codec.map(String::from),
        voice,
        language: language.into(),
        subtitles: re.sub.is_match(name),
        season: season.flatten(),
        year,
        year_end,
        bitrate_kbps: bitrate_in(name),
        single_episode: re
            .episode
            .captures(name)
            .is_some_and(|c| c.get(1).is_none() && c.get(2).is_none()),
        has_series_marker,
    }
}

fn is_rate_unit(rest: &str) -> bool {
    let lower = rest.trim_start().to_lowercase();
    ["kbps", "kbit", "кбит", "mbps", "mbit", "мбит", "fps"]
        .iter()
        .any(|u| lower.starts_with(u))
}

fn years_in_slice(s: &str, re: &Regexes) -> Vec<i32> {
    re.year
        .find_iter(s)
        .filter(|m| !is_rate_unit(&s[m.end()..]))
        .filter_map(|m| m.as_str().parse::<i32>().ok())
        .collect()
}

fn year_range_in(s: &str, re: &Regexes) -> Option<(i32, i32)> {
    re.year_range.captures(s).and_then(|c| {
        let a = c[1].parse::<i32>().ok()?;
        let b = c[2].parse::<i32>().ok()?;
        (a < b).then_some((a, b))
    })
}

/// Extracts the release year (and optional upper year for series spans such as `(2005-2020)`).
/// Bracketed years take precedence so title numbers like `2001` in `2001: A Space Odyssey (1968)`
/// are not mistaken for the release year.
fn extract_years(name: &str, re: &Regexes) -> (Option<i32>, Option<i32>) {
    for cap in re.brackets.captures_iter(name) {
        let inner = &cap[1];
        if let Some((a, b)) = year_range_in(inner, re) {
            return (Some(a), Some(b));
        }
        let ys = years_in_slice(inner, re);
        if let Some(&first) = ys.first() {
            let end = ys.get(1).copied().filter(|&b| b > first);
            return (Some(first), end);
        }
    }
    if let Some((a, b)) = year_range_in(name, re) {
        return (Some(a), Some(b));
    }
    let ys = years_in_slice(name, re);
    (ys.last().copied(), None)
}

/// Byte offset of an episode code (`S01E14`, `1x05`) when present, so title matching
/// checks only the show name before it and ignores episode titles after it.
pub(crate) fn episode_split_pos(name: &str) -> Option<usize> {
    RE.sxx_exx.find(name).map(|m| m.start())
}

pub(crate) fn is_known_studio(norm_words: &str) -> bool {
    STUDIOS.iter().any(|(k, _)| *k == norm_words)
        || matches!(
            norm_words,
            "kerob" | "rhs" | "dreamrecords" | "sunshinestudio" | "new team" | "gears media" | "generalfilm"
        )
}

/// A bitrate written in the name, as kbit/s. Values outside what a video can
/// plausibly have (0.3–400 Мбит/с) are ignored.
fn bitrate_in(name: &str) -> Option<u32> {
    let re = &*RE;
    let kbps = re
        .mbps
        .captures(name)
        .and_then(|c| c[1].replace(',', ".").parse::<f64>().ok())
        .map(|m| m * 1000.0)
        .or_else(|| re.kbps.captures(name).and_then(|c| c[1].parse::<f64>().ok()))?;
    (300.0..=400_000.0).contains(&kbps).then_some(kbps.round() as u32)
}

pub(crate) fn has_cyrillic(s: &str) -> bool {
    s.chars().any(|c| matches!(c, 'а'..='я' | 'А'..='Я' | 'ё' | 'Ё'))
}

/// Lowercase letters and digits only, for comparing names: "Dune: Part Two" → "dunepart two".
pub fn normalize(s: &str) -> String {
    s.to_lowercase()
        .replace('ё', "е")
        .chars()
        .filter(|c| c.is_alphanumeric())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rutracker_series() {
        let p = parse_title(
            "Во все тяжкие / Breaking Bad / Сезон: 2 / Серии: 1-13 из 13 (Винс Гиллиган) [2009, США, драма, BDRip 1080p] MVO (LostFilm) + Original + Sub (Rus, Eng)",
        );
        assert_eq!(p.quality, "1080p");
        assert_eq!(p.season, Some(2));
        assert_eq!(p.year, Some(2009));
        assert_eq!(p.voice, "LostFilm");
        assert_eq!(p.language, "Русский");
        assert!(p.subtitles);
        assert!(!p.hdr);
    }

    #[test]
    fn kinozal_film() {
        let p = parse_title(
            "Дюна: Часть вторая / Dune: Part Two / 2024 / ДБ, СТ / WEB-DL (2160p) / HDR10, Dolby Vision / HEVC",
        );
        assert_eq!(p.quality, "2160p");
        assert!(p.hdr && p.dolby_vision);
        assert_eq!(p.codec.as_deref(), Some("HEVC"));
        assert_eq!(p.voice, "Дубляж");
        assert!(p.subtitles);
        assert_eq!(p.season, None);
        assert_eq!(p.year, Some(2024));
    }

    #[test]
    fn packs_and_originals() {
        let p = parse_title("Breaking Bad S01-S05 Complete 720p BluRay x264");
        assert_eq!(p.season, None);
        assert_eq!(p.quality, "720p");
        assert_eq!(p.codec.as_deref(), Some("H.264"));
        assert_eq!(p.voice, "Оригинал");
        assert_eq!(p.language, "Английский");

        // Standalone Latin D in English release names must not match Russian dub.
        let p = parse_title("Avatar Aang The Last Airbender 2026 2160p AMZN WEB DL DD 5 1 Atmos H 265 E N D");
        assert_eq!(p.voice, "Оригинал");
        assert_eq!(p.language, "Английский");

        let p = parse_title("Властелин колец: Кольца власти [S02] (2024) WEB-DL 1080p | D | RHS");
        assert_eq!(p.language, "Русский");

        let p = parse_title("The.Green.Mile.1999.1080p.BluRay.RUS.ENG.x264");
        assert_eq!(p.language, "Русский");

        let p = parse_title("The.Green.Mile.1999.1080p.BluRay.ENG.Sub.Rus");
        assert_eq!(p.language, "Английский");

        let p = parse_title("Зелёная миля / The Green Mile (1999) BDRip 1080p [Original, Sub Rus]");
        assert_eq!(p.voice, "Оригинал");
        assert_eq!(p.language, "Английский");

        let p = parse_title("Breaking.Bad.S03E01.1080p.WEB-DL");
        assert_eq!(p.season, Some(3));
        let p = parse_title("Тед Лассо (3 сезон) 4K UHD");
        assert_eq!(p.season, Some(3));
        assert_eq!(p.quality, "2160p");
        let p = parse_title("Сериал / Сезоны: 1-3 [2020] DVDRip");
        assert_eq!(p.season, None);
        assert_eq!(p.quality, "SD");
    }

    #[test]
    fn bitrate_from_the_name() {
        let b = |n: &str| parse_title(n).bitrate_kbps;
        assert_eq!(b("Дюна / 2024 / WEB-DL (2160p) / HEVC / 25,4 Mbps"), Some(25_400));
        assert_eq!(b("Фильм [2020, BDRip 1080p, 8500 kbps]"), Some(8_500));
        assert_eq!(b("Фильм [2020, 1080p, 12 Мбит/с]"), Some(12_000));
        assert_eq!(b("Фильм [2020, 1080p]"), None);
        assert_eq!(b("Фильм 2020 999 Mbps"), None, "not a believable bitrate");
    }

    #[test]
    fn single_episodes_and_packs() {
        assert!(parse_title("Breaking.Bad.S03E01.1080p.WEB-DL").single_episode);
        assert!(!parse_title("Breaking.Bad.S03E01-E13.1080p").single_episode);
        assert!(!parse_title("Во все тяжкие / Сезон: 2 / Серии: 1-13 из 13 [BDRip 1080p]").single_episode);
        assert!(!parse_title("Тед Лассо (3 сезон) 4K UHD").single_episode);
    }

    #[test]
    fn names() {
        assert_eq!(normalize("Ёлки: Part Two!"), "елкиparttwo");
    }

    #[test]
    fn year_extraction_and_series_markers() {
        let p = parse_title("2001: Космическая одиссея / 2001: A Space Odyssey (1968) BDRip 1080p");
        assert_eq!((p.year, p.year_end, p.has_series_marker), (Some(1968), None, false));

        let p = parse_title("Blade.Runner.2049.2017.1080p.BluRay");
        assert_eq!((p.year, p.year_end, p.has_series_marker), (Some(2017), None, false));

        let p = parse_title("Чудо-женщина: 1984 / Wonder Woman 1984 (2020) WEB-DL 1080p");
        assert_eq!(p.year, Some(2020));

        let p = parse_title("Сверхъестественное / Supernatural [S01-15] (2005-2020) BDRip-HEVC 1080p");
        assert_eq!(
            (p.year, p.year_end, p.season, p.has_series_marker),
            (Some(2005), Some(2020), None, true)
        );

        let p = parse_title("Фильм 1999 2000 kbps");
        assert_eq!((p.year, p.bitrate_kbps), (Some(1999), Some(2000)));

        let p =
            parse_title("Сверхъестественное. Разлом времени / Taklee Genesis x Worlds Collide (2024) WEB-DL 1080p | D");
        assert_eq!((p.year, p.season, p.has_series_marker), (Some(2024), None, false));
    }
}
