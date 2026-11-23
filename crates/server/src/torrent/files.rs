//! Which file of a torrent to play: the film itself, or a given episode.

use std::sync::LazyLock;

use regex::Regex;

const VIDEO: &[&str] = &["mkv", "mp4", "m4v", "avi", "ts", "m2ts", "mov", "webm", "wmv", "mpg"];

pub fn is_video(path: &str) -> bool {
    path.rsplit_once('.')
        .is_some_and(|(_, ext)| VIDEO.contains(&ext.to_ascii_lowercase().as_str()))
}

static SE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)(?:^|[^a-z0-9])s(\d{1,2})[ ._-]?e(\d{1,3})(?:$|[^a-z0-9])|(?:^|[^a-z0-9])(\d{1,2})x(\d{2,3})(?:$|[^a-z0-9])",
    )
    .unwrap()
});
static EP: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(?:(?:^|[^a-z0-9])e|серия[ ._-]?|episode[ ._-]?|эпизод[ ._-]?)(\d{1,3})(?:$|[^a-z0-9])|^(\d{1,3})(?:$|[^a-z0-9])")
        .unwrap()
});
static SEASON_DIR: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(?:^|[^a-z0-9])s(\d{1,2})(?:$|[^a-z0-9])|(?:season|сезон)[ ._-]?(\d{1,2})|(\d{1,2})[ ._-]?сезон")
        .unwrap()
});

/// `(season, episode)` from a file path, when it says.
pub fn episode_of(path: &str) -> (Option<i32>, Option<i32>) {
    let (dirs, file) = path.rsplit_once('/').unwrap_or(("", path));
    let num = |m: Option<regex::Match>| m.and_then(|m| m.as_str().parse::<i32>().ok());
    if let Some(c) = SE.captures(file) {
        return (num(c.get(1).or(c.get(3))), num(c.get(2).or(c.get(4))));
    }
    let season = SEASON_DIR
        .captures_iter(dirs)
        .last()
        .and_then(|c| num(c.get(1).or(c.get(2)).or(c.get(3))));
    let episode = EP.captures(file).and_then(|c| num(c.get(1).or(c.get(2))));
    (season, episode)
}

/// A file of a torrent: its 1-based index (as TorrServer counts), path and size.
#[derive(Debug, Clone, PartialEq)]
pub struct FileEntry {
    pub index: i32,
    pub path: String,
    pub length: u64,
}

/// The file to play: the largest video for a film, or the episode of a series.
/// Files with no season in their name count as `season`, as packs of one season
/// rarely repeat it in every file name.
pub fn pick(files: &[FileEntry], episode: Option<(i32, i32)>) -> Option<&FileEntry> {
    let videos = files.iter().filter(|f| is_video(&f.path));
    match episode {
        None => videos.max_by_key(|f| f.length),
        Some((season, ep)) => {
            let videos: Vec<_> = videos.collect();
            let exact = videos.iter().find(|f| {
                let (s, e) = episode_of(&f.path);
                e == Some(ep) && s.is_none_or(|s| s == season)
            });
            // Unnumbered files sorted by name, as a last resort.
            exact.copied().or_else(|| {
                let mut sorted = videos.clone();
                sorted.sort_by(|a, b| a.path.cmp(&b.path));
                let numbered = sorted.iter().any(|f| episode_of(&f.path).1.is_some());
                (!numbered)
                    .then(|| sorted.get((ep - 1).max(0) as usize).copied())
                    .flatten()
            })
        }
    }
}

/// Distinct `(season, episode)` pairs actually present among video files of a torrent.
/// Season-less packs are placed under `default_season`.
pub fn available_episodes(files: &[FileEntry], default_season: i32) -> Vec<(i32, i32)> {
    let mut out: Vec<(i32, i32)> = files
        .iter()
        .filter(|f| is_video(&f.path))
        .filter_map(|f| {
            let (s, e) = episode_of(&f.path);
            let ep = e?;
            Some((s.unwrap_or(default_season.max(1)), ep))
        })
        .collect();
    out.sort_unstable();
    out.dedup();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn files(paths: &[&str]) -> Vec<FileEntry> {
        paths
            .iter()
            .enumerate()
            .map(|(i, p)| FileEntry {
                index: i as i32 + 1,
                path: p.to_string(),
                length: 100 + i as u64,
            })
            .collect()
    }

    #[test]
    fn episodes() {
        assert_eq!(episode_of("Breaking.Bad.S02E05.1080p.mkv"), (Some(2), Some(5)));
        assert_eq!(episode_of("BB/Season 3/03. Pilot.mkv"), (Some(3), Some(3)));
        assert_eq!(episode_of("Сезон 1/Серия 7.avi"), (Some(1), Some(7)));
        assert_eq!(episode_of("show.2x10.mp4"), (Some(2), Some(10)));
        assert_eq!(episode_of("Season_01/s01e01_Pilot.avi"), (Some(1), Some(1)));
        assert_eq!(episode_of("Season_02/Show_S02E05_720p.mkv"), (Some(2), Some(5)));
        assert_eq!(episode_of("Film.2024.mkv"), (None, None));
    }

    #[test]
    fn picks() {
        let f = files(&[
            "Show/S01/Show.S01E01.mkv",
            "Show/S01/Show.S01E02.mkv",
            "Show/S02/Show.S02E01.mkv",
            "Show/sample.txt",
        ]);
        assert_eq!(pick(&f, Some((2, 1))).unwrap().index, 3);
        assert_eq!(pick(&f, Some((1, 2))).unwrap().index, 2);
        assert!(pick(&f, Some((3, 1))).is_none());
        let film = files(&["Film/sample.mkv", "Film/Film.2024.mkv", "Film/Film.srt"]);
        assert_eq!(pick(&film, None).unwrap().index, 2);
        let plain = files(&["a.mkv", "b.mkv", "c.mkv"]);
        assert_eq!(pick(&plain, Some((1, 3))).unwrap().path, "c.mkv");
        assert_eq!(available_episodes(&f, 1), vec![(1, 1), (1, 2), (2, 1)]);
    }
}
