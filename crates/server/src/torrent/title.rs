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
    year: Regex,
    dv: Regex,
    hdr: Regex,
    hevc: Regex,
    avc: Regex,
    av1: Regex,
    dub: Regex,
    mvo: Regex,
    dvo: Regex,
    avo: Regex,
    sub: Regex,
    ukr_only: Regex,
}

impl Regexes {
    fn new() -> Self {
        let re = |s: &str| Regex::new(s).expect("valid regex");
        Self {
            quality: re(r"(?i)\b(2160|1440|1080|720|576|480)[pi]\b|\b(4k|uhd)\b"),
            season: re(
                r"(?i)(?:сезон[ыа]?[:\s]*(\d{1,2})(?:\s*[-–]\s*(\d{1,2}))?)|(?:\b(\d{1,2})(?:\s*[-–]\s*(\d{1,2}))?\s*сезон)|(?:\bs(\d{1,2})(?:\s*[-–]\s*s?(\d{1,2}))?(?:e\d|\b))|(?:season\s*(\d{1,2}))",
            ),
            year: re(r"\b(19[3-9]\d|20[0-4]\d)\b"),
            dv: re(r"(?i)dolby\s*vision|\bdovi\b|\bdv\b"),
            hdr: re(r"(?i)\bhdr(10\+?|10plus)?\b"),
            hevc: re(r"(?i)\b(hevc|[hx]\.?265)\b"),
            avc: re(r"(?i)\b(avc|[hx]\.?264)\b"),
            av1: re(r"(?i)\bav1\b"),
            dub: re(r"(?i)\b(dub|d|дубляж|дублированный|д|дб)\b"),
            mvo: re(r"(?i)\b(mvo|многоголосый|пм|лм)\b"),
            dvo: re(r"(?i)\b(dvo|двухголосый|пд|лд)\b"),
            avo: re(r"(?i)\b(avo|авторский|ао|ап)\b"),
            sub: re(r"(?i)\b(sub|subs|субтитры|ст)\b"),
            ukr_only: re(r"(?i)\b(ukr|укр)\b"),
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

    let studio = STUDIOS.iter().find(|(k, _)| lower.contains(k)).map(|(_, v)| *v);
    let voice = match studio {
        Some(s) => s.to_string(),
        None if re.dub.is_match(name) => "Дубляж".into(),
        None if re.mvo.is_match(name) => "Многоголосый".into(),
        None if re.dvo.is_match(name) => "Двухголосый".into(),
        None if re.avo.is_match(name) => "Авторский".into(),
        None if has_cyrillic(name) => "Русский".into(),
        None => "Оригинал".into(),
    };
    let language = if voice == "Оригинал" {
        "Английский"
    } else if re.ukr_only.is_match(name) && studio.is_none() && !lower.contains("rus") && !lower.contains("рус") {
        "Украинский"
    } else {
        "Русский"
    };

    ParsedTitle {
        quality,
        hdr: re.hdr.is_match(name) || re.dv.is_match(name),
        dolby_vision: re.dv.is_match(name),
        codec: codec.map(String::from),
        voice,
        language: language.into(),
        subtitles: re.sub.is_match(name),
        season: season.flatten(),
        year: re.year.captures(name).and_then(|c| c[1].parse().ok()),
    }
}

fn has_cyrillic(s: &str) -> bool {
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
    fn names() {
        assert_eq!(normalize("Ёлки: Part Two!"), "елкиparttwo");
    }
}
