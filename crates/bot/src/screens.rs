//! What the bot says: texts and buttons, built from plain data so they can be
//! tested without Telegram. Texts are HTML (Telegram's subset), so every title
//! goes through [`esc`].

use kino_core::{clock, episode_code, ContinueItem, MediaKind, TitleStatus, TitleSummary};
use teloxide::{
    types::{InlineKeyboardButton, InlineKeyboardMarkup, WebAppInfo},
    utils::html::escape as esc,
};
use url::Url;

/// How many titles one message lists.
pub const LIST_LIMIT: usize = 8;

/// Callback data of the "Мои" screen: `my:menu` or `my:<status>`.
pub const MY_CALLBACK: &str = "my:";

/// The app opened on a title: `<site>/?open=tv_1396`.
pub fn title_url(base: &Url, kind: MediaKind, tmdb_id: i64) -> Url {
    let mut url = base.clone();
    url.query_pairs_mut()
        .append_pair("open", &format!("{}_{tmdb_id}", kind.as_str()));
    url
}

fn open_title(label: String, base: &Url, kind: MediaKind, tmdb_id: i64) -> InlineKeyboardButton {
    InlineKeyboardButton::web_app(
        label,
        WebAppInfo {
            url: title_url(base, kind, tmdb_id),
        },
    )
}

pub fn open_app(base: &Url, label: &str) -> InlineKeyboardButton {
    InlineKeyboardButton::web_app(label, WebAppInfo { url: base.clone() })
}

pub struct Screen {
    pub text: String,
    pub keyboard: InlineKeyboardMarkup,
}

impl Screen {
    fn new(text: String, rows: Vec<Vec<InlineKeyboardButton>>) -> Self {
        Self {
            text,
            keyboard: InlineKeyboardMarkup::new(rows),
        }
    }
}

fn kind_word(kind: MediaKind) -> &'static str {
    match kind {
        MediaKind::Movie => "фильм",
        MediaKind::Tv => "сериал",
    }
}

fn name_with_year(t: &TitleSummary) -> String {
    match t.year {
        Some(y) => format!("{} ({y})", t.title),
        None => t.title.clone(),
    }
}

/// Shown in an empty chat with the bot, above the "Начать" button (Telegram allows 512 characters).
pub const BOT_DESCRIPTION: &str = "Орловский кинопоказ: ваш личный кинотеатр.\n\n\
    Ищите фильмы и сериалы, смотрите в Telegram, на сайте и в приложении и продолжайте с того же места на любом устройстве.\n\n\
    Нажмите «Начать».";

/// Shown on the bot's profile and when it is shared (Telegram allows 120 characters).
pub const BOT_SHORT_DESCRIPTION: &str =
    "Личный кинотеатр: поиск, просмотр и продолжение с того же места на любом устройстве.";

pub fn help(base: &Url, is_admin: bool) -> Screen {
    let mut text = String::from(
        "<b>Орловский кинопоказ</b>\n\
         Ищите фильмы и сериалы и смотрите дальше с того места, где остановились, на любом устройстве.\n\n\
         /search <i>название</i> — найти фильм или сериал. Можно просто написать название без команды\n\
         /continue — продолжить просмотр\n\
         /my — мои списки: смотрю, хочу посмотреть, просмотрено, брошено\n\
         /login — войти на сайт в браузере\n\
         /help — эта подсказка\n\
         /start — открыть кинотеатр",
    );
    if is_admin {
        text.push_str("\n/invite — создать приглашение (только админ)");
    }
    Screen::new(text, vec![vec![open_app(base, "Открыть кинопоказ")]])
}

pub fn search_prompt() -> String {
    "Что найти? Напишите название после команды, например <code>/search матрица</code>. \
     Или просто отправьте название сообщением."
        .into()
}

pub fn search_results(base: &Url, query: &str, items: &[TitleSummary]) -> Screen {
    if items.is_empty() {
        return Screen::new(
            format!(
                "По запросу «{}» ничего не нашлось. Попробуйте другое название.",
                esc(query)
            ),
            vec![],
        );
    }
    let shown = &items[..items.len().min(LIST_LIMIT)];
    let mut text = format!("Нашлось по запросу «{}»:\n", esc(query));
    let mut rows = Vec::new();
    for (i, t) in shown.iter().enumerate() {
        text.push_str(&format!("\n{}. <b>{}</b>", i + 1, esc(&name_with_year(t))));
        text.push_str(&format!(", {}", kind_word(t.kind)));
        if let Some(r) = t.rating.filter(|r| *r > 0.0) {
            text.push_str(&format!(", ★ {r:.1}"));
        }
        rows.push(vec![open_title(
            format!("{}. {}", i + 1, name_with_year(t)),
            base,
            t.kind,
            t.tmdb_id,
        )]);
    }
    text.push_str("\n\nНажмите на название, чтобы открыть его.");
    Screen::new(text, rows)
}

/// One "Continue watching" row, with the catalog's name when it had one.
pub struct ContinueRow {
    pub item: ContinueItem,
    pub title: Option<String>,
}

fn row_title(title: &Option<String>, kind: MediaKind) -> String {
    title.clone().unwrap_or_else(|| match kind {
        MediaKind::Movie => "Фильм".into(),
        MediaKind::Tv => "Сериал".into(),
    })
}

/// Where the person stopped, or what comes next.
fn progress_line(item: &ContinueItem) -> String {
    let last = &item.last;
    match (last.kind, item.next_episode) {
        (MediaKind::Tv, Some((s, e))) => format!("дальше серия {}", episode_code(s, e)),
        (MediaKind::Tv, None) => format!(
            "{}, остановились на {}",
            episode_code(last.season, last.episode),
            clock(last.position_sec)
        ),
        (MediaKind::Movie, _) => format!(
            "остановились на {} из {}",
            clock(last.position_sec),
            clock(last.duration_sec)
        ),
    }
}

pub fn continue_list(base: &Url, rows: &[ContinueRow]) -> Screen {
    if rows.is_empty() {
        return Screen::new(
            "Пока нечего продолжать: вы ничего не начинали смотреть. Найдите что-нибудь командой /search.".into(),
            vec![vec![open_app(base, "Открыть кинопоказ")]],
        );
    }
    let mut text = String::from("<b>Продолжить просмотр</b>\n");
    let mut buttons = Vec::new();
    for r in rows.iter().take(LIST_LIMIT) {
        let name = row_title(&r.title, r.item.last.kind);
        text.push_str(&format!("\n• <b>{}</b>, {}", esc(&name), progress_line(&r.item)));
        buttons.push(vec![open_title(
            format!("▶ {name}"),
            base,
            r.item.last.kind,
            r.item.last.tmdb_id,
        )]);
    }
    Screen::new(text, buttons)
}

fn status_icon(s: TitleStatus) -> &'static str {
    match s {
        TitleStatus::Watching => "▶",
        TitleStatus::Want => "🔖",
        TitleStatus::Watched => "✅",
        TitleStatus::Dropped => "⏸",
    }
}

/// Counts of titles per status, for the "Мои" menu.
pub fn my_menu(counts: &[(TitleStatus, usize)]) -> Screen {
    let total: usize = counts.iter().map(|(_, n)| n).sum();
    let text = if total == 0 {
        "<b>Мои списки</b>\nПока пусто. Найдите фильм командой /search и отметьте его в приложении.".to_owned()
    } else {
        "<b>Мои списки</b>\nВыберите список.".to_owned()
    };
    let buttons: Vec<InlineKeyboardButton> = counts
        .iter()
        .map(|(s, n)| {
            InlineKeyboardButton::callback(
                format!("{} {} · {n}", status_icon(*s), s.label()),
                format!("{MY_CALLBACK}{}", s.as_str()),
            )
        })
        .collect();
    Screen::new(text, buttons.chunks(2).map(<[_]>::to_vec).collect())
}

/// One titles list of the "Мои" screen. `total` can be larger than `titles`.
pub fn my_list(base: &Url, status: TitleStatus, titles: &[(MediaKind, i64, String)], total: usize) -> Screen {
    let mut text = format!("<b>{} {}</b> · {total}\n", status_icon(status), status.label());
    let mut rows = Vec::new();
    for (kind, id, name) in titles {
        text.push_str(&format!("\n• {}", esc(name)));
        rows.push(vec![open_title(name.clone(), base, *kind, *id)]);
    }
    if total > titles.len() {
        text.push_str(&format!("\n\nИ ещё {}, их видно в приложении.", total - titles.len()));
    }
    rows.push(vec![InlineKeyboardButton::callback(
        "← К спискам",
        format!("{MY_CALLBACK}menu"),
    )]);
    Screen::new(text, rows)
}

pub fn not_registered(base: &Url) -> Screen {
    Screen::new(
        "Я вас пока не знаю. Откройте кинопоказ один раз, и здесь появятся ваши списки и продолжение просмотра.".into(),
        vec![vec![open_app(base, "Открыть кинопоказ")]],
    )
}

pub fn login_link(site: &Url, code: &str) -> Screen {
    let mut url = site.clone();
    url.query_pairs_mut().append_pair("login", code);
    let host = site.host_str().unwrap_or("сайт");
    Screen::new(
        format!(
            "Вход на {}.\nСсылка работает один раз и десять минут. Откройте её в браузере, где хотите смотреть, \
             и сайт сразу узнает вас. Никому её не пересылайте.",
            esc(host)
        ),
        vec![vec![InlineKeyboardButton::url("Войти на сайт", url)]],
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use kino_core::Progress;
    use teloxide::types::InlineKeyboardButtonKind;

    fn base() -> Url {
        "https://kino.example.com/".parse().unwrap()
    }

    fn summary(kind: MediaKind, id: i64, title: &str, year: Option<i32>) -> TitleSummary {
        TitleSummary {
            kind,
            tmdb_id: id,
            title: title.into(),
            original_title: None,
            year,
            poster_path: None,
            backdrop_path: None,
            rating: Some(8.24),
            overview: None,
        }
    }

    fn web_app_url(b: &InlineKeyboardButton) -> String {
        match &b.kind {
            InlineKeyboardButtonKind::WebApp(w) => w.url.to_string(),
            other => panic!("not a web app button: {other:?}"),
        }
    }

    #[test]
    fn title_link_carries_kind_and_id() {
        assert_eq!(
            title_url(&base(), MediaKind::Tv, 1396).as_str(),
            "https://kino.example.com/?open=tv_1396"
        );
    }

    #[test]
    fn search_lists_titles_and_escapes_markup() {
        let items = vec![
            summary(MediaKind::Movie, 603, "Матрица", Some(1999)),
            summary(MediaKind::Tv, 1, "<b>Тьма</b> & Ко", None),
        ];
        let s = search_results(&base(), "<script>", &items);
        assert!(s.text.contains("«&lt;script&gt;»"));
        assert!(s.text.contains("1. <b>Матрица (1999)</b>, фильм, ★ 8.2"));
        assert!(s.text.contains("&lt;b&gt;Тьма&lt;/b&gt; &amp; Ко"));
        let rows = &s.keyboard.inline_keyboard;
        assert_eq!(rows.len(), 2);
        assert_eq!(web_app_url(&rows[0][0]), "https://kino.example.com/?open=movie_603");
        assert_eq!(rows[0][0].text, "1. Матрица (1999)");
    }

    #[test]
    fn search_with_nothing_found_says_so_without_buttons() {
        let s = search_results(&base(), "ыыы", &[]);
        assert!(s.text.contains("ничего не нашлось"));
        assert!(s.keyboard.inline_keyboard.is_empty());
    }

    #[test]
    fn search_shows_at_most_the_limit() {
        let items: Vec<_> = (0..20).map(|i| summary(MediaKind::Movie, i, "Фильм", None)).collect();
        assert_eq!(
            search_results(&base(), "ф", &items).keyboard.inline_keyboard.len(),
            LIST_LIMIT
        );
    }

    fn progress(kind: MediaKind, season: i32, episode: i32, pos: f64, finished: bool) -> Progress {
        Progress {
            kind,
            tmdb_id: 7,
            season,
            episode,
            position_sec: pos,
            duration_sec: 3000.0,
            finished,
            release: None,
            updated_at: chrono::Utc::now(),
        }
    }

    #[test]
    fn continue_shows_where_you_stopped_or_the_next_episode() {
        let rows = vec![
            ContinueRow {
                item: ContinueItem {
                    last: progress(MediaKind::Tv, 2, 5, 1270.0, false),
                    next_episode: None,
                    title: None,
                    stale: false,
                },
                title: Some("Во все тяжкие".into()),
            },
            ContinueRow {
                item: ContinueItem {
                    last: progress(MediaKind::Tv, 1, 3, 2900.0, true),
                    next_episode: Some((1, 4)),
                    title: None,
                    stale: false,
                },
                title: Some("Тьма".into()),
            },
            ContinueRow {
                item: ContinueItem {
                    last: progress(MediaKind::Movie, 0, 0, 3725.0, false),
                    next_episode: None,
                    title: None,
                    stale: false,
                },
                title: None,
            },
        ];
        let s = continue_list(&base(), &rows);
        assert!(s.text.contains("<b>Во все тяжкие</b>, S02E05, остановились на 21:10"));
        assert!(s.text.contains("<b>Тьма</b>, дальше серия S01E04"));
        assert!(s.text.contains("<b>Фильм</b>, остановились на 1:02:05 из 50:00"));
        assert_eq!(s.keyboard.inline_keyboard.len(), 3);
        assert_eq!(s.keyboard.inline_keyboard[0][0].text, "▶ Во все тяжкие");
    }

    #[test]
    fn empty_continue_points_to_search() {
        assert!(continue_list(&base(), &[]).text.contains("/search"));
    }

    #[test]
    fn my_menu_has_a_button_per_status() {
        let counts: Vec<_> = TitleStatus::ALL.iter().map(|s| (*s, 2usize)).collect();
        let s = my_menu(&counts);
        let flat: Vec<_> = s.keyboard.inline_keyboard.iter().flatten().collect();
        assert_eq!(flat.len(), 4);
        assert_eq!(flat[0].text, "▶ Смотрю · 2");
        match &flat[1].kind {
            InlineKeyboardButtonKind::CallbackData(d) => assert_eq!(d, "my:want"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn my_list_counts_hidden_titles_and_goes_back() {
        let titles = vec![(MediaKind::Movie, 603, "Матрица".to_owned())];
        let s = my_list(&base(), TitleStatus::Want, &titles, 5);
        assert!(s.text.contains("🔖 Хочу посмотреть</b> · 5"));
        assert!(s.text.contains("И ещё 4"));
        let last = s.keyboard.inline_keyboard.last().unwrap();
        assert_eq!(last[0].text, "← К спискам");
    }

    #[test]
    fn login_link_is_a_one_time_url_button() {
        let s = login_link(&base(), "abc123");
        match &s.keyboard.inline_keyboard[0][0].kind {
            InlineKeyboardButtonKind::Url(u) => assert_eq!(u.as_str(), "https://kino.example.com/?login=abc123"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn profile_texts_fit_telegram_limits() {
        assert!(BOT_DESCRIPTION.chars().count() <= 512);
        assert!(BOT_SHORT_DESCRIPTION.chars().count() <= 120);
    }

    #[test]
    fn help_mentions_invite_only_for_admins() {
        assert!(!help(&base(), false).text.contains("/invite"));
        assert!(help(&base(), true).text.contains("/invite"));
    }
}
