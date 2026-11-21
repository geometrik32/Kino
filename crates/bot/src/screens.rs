//! What the bot says: texts and buttons, built from plain data so they can be
//! tested without Telegram. Texts are HTML (Telegram's subset), so every title
//! goes through [`esc`].

use kino_core::{clock, episode_code, ContinueItem, MediaKind, TitleDetails, TitleStatus, TitleSummary};
use teloxide::{
    types::{ButtonRequest, InlineKeyboardButton, InlineKeyboardMarkup, KeyboardButton, KeyboardMarkup, WebAppInfo},
    utils::html::escape as esc,
};
use url::Url;

/// How many titles one message lists.
pub const LIST_LIMIT: usize = 8;

/// How many top search results are immediately sent as full cards with posters.
pub const SEARCH_CARD_LIMIT: usize = 3;

/// Maximum plain-text characters in a Telegram photo caption (API limit is 1024).
const CAPTION_MAX_CHARS: usize = 1000;

/// Callback data of the "Мои" screen: `my:menu` or `my:<status>`.
pub const MY_CALLBACK: &str = "my:";

/// Callback data of command shortcut buttons: `cmd:<action>`.
pub const CMD_CALLBACK: &str = "cmd:";

/// Callback data to open a movie card from the remaining search results: `card:<kind>:<tmdb_id>`.
pub const CARD_CALLBACK: &str = "card:";

pub const BTN_OPEN_APP: &str = "🎬 Открыть кинопоказ";
pub const BTN_SEARCH: &str = "🔍 Поиск";
pub const BTN_CONTINUE: &str = "▶ Продолжить";
pub const BTN_MY: &str = "📚 Мои списки";
pub const BTN_LOGIN: &str = "🌐 Вход на сайт";
pub const BTN_HELP: &str = "❓ Помощь";
pub const BTN_INVITE: &str = "🎟 Приглашение";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BotAction {
    Search,
    Continue,
    My,
    Login,
    Help,
    Invite,
}

impl BotAction {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Search => "search",
            Self::Continue => "continue",
            Self::My => "my",
            Self::Login => "login",
            Self::Help => "help",
            Self::Invite => "invite",
        }
    }

    pub fn parse_button(text: &str) -> Option<Self> {
        match text.trim() {
            BTN_SEARCH => Some(Self::Search),
            BTN_CONTINUE => Some(Self::Continue),
            BTN_MY => Some(Self::My),
            BTN_LOGIN => Some(Self::Login),
            BTN_HELP => Some(Self::Help),
            BTN_INVITE => Some(Self::Invite),
            _ => None,
        }
    }

    pub fn parse_callback(suffix: &str) -> Option<Self> {
        match suffix {
            "search" => Some(Self::Search),
            "continue" => Some(Self::Continue),
            "my" => Some(Self::My),
            "login" => Some(Self::Login),
            "help" => Some(Self::Help),
            "invite" => Some(Self::Invite),
            _ => None,
        }
    }
}

pub fn parse_card_callback(suffix: &str) -> Option<(MediaKind, i64)> {
    let (kind, id) = suffix.split_once(':')?;
    Some((MediaKind::parse(kind)?, id.parse().ok()?))
}

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

fn kind_badge(kind: MediaKind) -> &'static str {
    match kind {
        MediaKind::Movie => "🎬 Фильм",
        MediaKind::Tv => "📺 Сериал",
    }
}

fn name_with_year(t: &TitleSummary) -> String {
    match t.year {
        Some(y) => format!("{} ({y})", t.title),
        None => t.title.clone(),
    }
}

fn truncate_chars(s: &str, max_chars: usize) -> String {
    let trimmed = s.trim();
    if trimmed.chars().count() <= max_chars {
        return trimmed.to_owned();
    }
    if max_chars <= 1 {
        return "…".to_owned();
    }
    let limit = max_chars - 1;
    let mut end_byte = trimmed.len();
    for (idx, (byte_pos, _)) in trimmed.char_indices().enumerate() {
        if idx == limit {
            end_byte = byte_pos;
            break;
        }
    }
    let slice = &trimmed[..end_byte];
    // Prefer breaking at a word boundary when enough text remains.
    let cut = match slice.rfind(char::is_whitespace) {
        Some(ws) if slice[..ws].chars().count() * 10 >= limit * 6 => &slice[..ws],
        _ => slice,
    };
    let clean = cut.trim_end_matches(|c: char| c.is_whitespace() || matches!(c, ',' | ';' | ':' | '-'));
    format!("{clean}…")
}

/// Persistent keyboard in private chats showing all available bot commands.
pub fn main_reply_keyboard(app_url: &Url, is_admin: bool) -> KeyboardMarkup {
    let mut rows = vec![
        vec![KeyboardButton::new(BTN_OPEN_APP).request(ButtonRequest::WebApp(WebAppInfo { url: app_url.clone() }))],
        vec![KeyboardButton::new(BTN_SEARCH), KeyboardButton::new(BTN_CONTINUE)],
        vec![KeyboardButton::new(BTN_MY), KeyboardButton::new(BTN_LOGIN)],
    ];
    let last_row = if is_admin {
        vec![KeyboardButton::new(BTN_HELP), KeyboardButton::new(BTN_INVITE)]
    } else {
        vec![KeyboardButton::new(BTN_HELP)]
    };
    rows.push(last_row);
    KeyboardMarkup::new(rows)
        .resize_keyboard()
        .persistent()
        .input_field_placeholder("Введите название фильма или сериала…")
}

fn command_inline_rows(base: &Url, is_admin: bool) -> Vec<Vec<InlineKeyboardButton>> {
    let cmd = |label: &str, action: BotAction| {
        InlineKeyboardButton::callback(label, format!("{CMD_CALLBACK}{}", action.as_str()))
    };
    let mut rows = vec![
        vec![open_app(base, BTN_OPEN_APP)],
        vec![
            cmd(BTN_SEARCH, BotAction::Search),
            cmd(BTN_CONTINUE, BotAction::Continue),
        ],
        vec![cmd(BTN_MY, BotAction::My), cmd(BTN_LOGIN, BotAction::Login)],
    ];
    if is_admin {
        rows.push(vec![cmd(BTN_INVITE, BotAction::Invite)]);
    }
    rows
}

pub fn start_welcome_text() -> &'static str {
    "Привет! Здесь можно найти фильм или сериал и смотреть его с того места, где остановились.\n\n\
     Просто напишите название фильма в чат или выберите действие на кнопках ниже."
}

pub fn start_inline(app_url: &Url, is_admin: bool) -> Screen {
    Screen::new(start_welcome_text().to_owned(), command_inline_rows(app_url, is_admin))
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
         /start — открыть кинотеатр и обновить кнопки",
    );
    if is_admin {
        text.push_str("\n/invite — создать приглашение (только админ)");
    }
    Screen::new(text, command_inline_rows(base, is_admin))
}

pub fn search_prompt() -> String {
    "Что найти? Напишите название фильма или сериала прямо в чат — например, <code>матрица</code> или <code>зеленая линия</code>.\n\
     Также можно использовать команду <code>/search название</code>."
        .into()
}

pub fn search_empty(query: &str) -> Screen {
    Screen::new(
        format!(
            "По запросу «{}» ничего не нашлось. Попробуйте другое название.",
            esc(query)
        ),
        vec![],
    )
}

fn format_card(base: &Url, t: &TitleSummary, genres: &[String], runtime_min: Option<i32>) -> Screen {
    let header = name_with_year(t);
    let mut text = format!("<b>{}</b>", esc(&header));
    let mut plain_len = header.chars().count();

    if let Some(orig) = t.original_title.as_deref().map(str::trim) {
        if !orig.is_empty() && !orig.eq_ignore_ascii_case(t.title.trim()) {
            text.push_str(&format!("\n<i>{}</i>", esc(orig)));
            plain_len += 1 + orig.chars().count();
        }
    }

    let mut meta = String::from(kind_badge(t.kind));
    if let Some(r) = t.rating.filter(|r| *r > 0.0) {
        meta.push_str(&format!(" · ★ {r:.1}"));
    }
    if let Some(mins) = runtime_min.filter(|m| *m > 0) {
        meta.push_str(&format!(" · {mins} мин"));
    }
    text.push_str(&format!("\n{meta}"));
    plain_len += 1 + meta.chars().count();

    if !genres.is_empty() {
        let joined = genres.iter().take(4).cloned().collect::<Vec<_>>().join(", ");
        text.push_str(&format!("\n<i>{}</i>", esc(&joined)));
        plain_len += 1 + joined.chars().count();
    }

    if let Some(overview) = t.overview.as_deref().map(str::trim).filter(|o| !o.is_empty()) {
        let budget = CAPTION_MAX_CHARS.saturating_sub(plain_len + 2);
        if budget >= 20 {
            let short = truncate_chars(overview, budget);
            text.push_str(&format!("\n\n{}", esc(&short)));
        }
    }

    Screen::new(
        text,
        vec![vec![open_title(BTN_OPEN_APP.into(), base, t.kind, t.tmdb_id)]],
    )
}

/// Movie or series card shown for search results (photo caption + open button).
pub fn title_card(base: &Url, t: &TitleSummary) -> Screen {
    format_card(base, t, &[], None)
}

/// Movie or series card enriched with details (genres and runtime).
pub fn title_details_card(base: &Url, d: &TitleDetails) -> Screen {
    format_card(base, &d.summary, &d.genres, d.runtime_min)
}

/// Compact list of additional search hits beyond [`SEARCH_CARD_LIMIT`], each opening its full card.
pub fn more_search_results(query: &str, offset: usize, items: &[TitleSummary]) -> Screen {
    let text = format!(
        "Ещё по запросу «{}» — выберите, чтобы показать карточку с постером и описанием:",
        esc(query)
    );
    let rows = items
        .iter()
        .enumerate()
        .map(|(i, t)| {
            let mut label = format!("{}. {}", offset + i + 1, name_with_year(t));
            if let Some(r) = t.rating.filter(|r| *r > 0.0) {
                label.push_str(&format!(" · ★ {r:.1}"));
            }
            vec![InlineKeyboardButton::callback(
                label,
                format!("{CARD_CALLBACK}{}:{}", t.kind.as_str(), t.tmdb_id),
            )]
        })
        .collect();
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
    fn title_card_shows_meta_overview_and_escapes_markup() {
        let mut t = summary(MediaKind::Movie, 603, "Матрица", Some(1999));
        t.original_title = Some("The Matrix".into());
        t.overview = Some("Хакер <b>Нео</b> & машины.".into());
        let s = title_card(&base(), &t);
        assert!(s.text.contains("<b>Матрица (1999)</b>"));
        assert!(s.text.contains("<i>The Matrix</i>"));
        assert!(s.text.contains("🎬 Фильм · ★ 8.2"));
        assert!(s.text.contains("Хакер &lt;b&gt;Нео&lt;/b&gt; &amp; машины."));
        assert_eq!(
            web_app_url(&s.keyboard.inline_keyboard[0][0]),
            "https://kino.example.com/?open=movie_603"
        );
    }

    #[test]
    fn title_card_truncates_long_overview_within_caption_limit() {
        let mut t = summary(MediaKind::Tv, 1, "Тьма", Some(2017));
        t.overview = Some("Слово ".repeat(400));
        let s = title_card(&base(), &t);
        assert!(s.text.chars().count() <= 1024);
        assert!(s.text.ends_with('…'));
    }

    #[test]
    fn search_with_nothing_found_says_so_without_buttons() {
        let s = search_empty("ыыы");
        assert!(s.text.contains("ничего не нашлось"));
        assert!(s.keyboard.inline_keyboard.is_empty());
    }

    #[test]
    fn more_search_results_build_card_callbacks() {
        let items = vec![
            summary(MediaKind::Movie, 603, "Матрица", Some(1999)),
            summary(MediaKind::Tv, 1396, "Во все тяжкие", Some(2008)),
        ];
        let s = more_search_results("<script>", 3, &items);
        assert!(s.text.contains("«&lt;script&gt;»"));
        let rows = &s.keyboard.inline_keyboard;
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0][0].text, "4. Матрица (1999) · ★ 8.2");
        match &rows[1][0].kind {
            InlineKeyboardButtonKind::CallbackData(d) => {
                assert_eq!(d, "card:tv:1396");
                assert_eq!(
                    parse_card_callback(d.strip_prefix(CARD_CALLBACK).unwrap()),
                    Some((MediaKind::Tv, 1396))
                );
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn reply_keyboard_and_button_parser_cover_all_commands() {
        let kb = main_reply_keyboard(&base(), false);
        let texts: Vec<_> = kb.keyboard.iter().flatten().map(|b| b.text.as_str()).collect();
        assert!(texts.contains(&BTN_OPEN_APP));
        assert!(texts.contains(&BTN_SEARCH));
        assert!(texts.contains(&BTN_CONTINUE));
        assert!(texts.contains(&BTN_MY));
        assert!(texts.contains(&BTN_LOGIN));
        assert!(texts.contains(&BTN_HELP));
        assert!(!texts.contains(&BTN_INVITE));

        let admin_kb = main_reply_keyboard(&base(), true);
        let admin_texts: Vec<_> = admin_kb.keyboard.iter().flatten().map(|b| b.text.as_str()).collect();
        assert!(admin_texts.contains(&BTN_INVITE));

        assert_eq!(BotAction::parse_button(BTN_SEARCH), Some(BotAction::Search));
        assert_eq!(BotAction::parse_button(BTN_CONTINUE), Some(BotAction::Continue));
        assert_eq!(BotAction::parse_button(BTN_MY), Some(BotAction::My));
        assert_eq!(BotAction::parse_button(BTN_LOGIN), Some(BotAction::Login));
        assert_eq!(BotAction::parse_button(BTN_HELP), Some(BotAction::Help));
        assert_eq!(BotAction::parse_button(BTN_INVITE), Some(BotAction::Invite));
        assert_eq!(BotAction::parse_button("Зеленая линия"), None);
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
        let user_help = help(&base(), false);
        assert!(!user_help.text.contains("/invite"));
        let user_buttons: Vec<_> = user_help
            .keyboard
            .inline_keyboard
            .iter()
            .flatten()
            .map(|b| b.text.as_str())
            .collect();
        assert!(user_buttons.contains(&BTN_SEARCH));
        assert!(user_buttons.contains(&BTN_CONTINUE));
        assert!(user_buttons.contains(&BTN_MY));
        assert!(user_buttons.contains(&BTN_LOGIN));
        assert!(!user_buttons.contains(&BTN_INVITE));

        let admin_help = help(&base(), true);
        assert!(admin_help.text.contains("/invite"));
        let admin_buttons: Vec<_> = admin_help
            .keyboard
            .inline_keyboard
            .iter()
            .flatten()
            .map(|b| b.text.as_str())
            .collect();
        assert!(admin_buttons.contains(&BTN_INVITE));
    }

    #[test]
    fn title_details_card_includes_genres_and_runtime() {
        let mut t = summary(MediaKind::Movie, 603, "Матрица", Some(1999));
        t.overview = Some("Описание фильма.".into());
        let d = TitleDetails {
            summary: t,
            genres: vec!["фантастика".into(), "боевик".into()],
            runtime_min: Some(136),
            tagline: None,
            seasons: vec![],
            imdb_id: None,
            last_episode: None,
        };
        let s = title_details_card(&base(), &d);
        assert!(s.text.contains("🎬 Фильм · ★ 8.2 · 136 мин"));
        assert!(s.text.contains("<i>фантастика, боевик</i>"));
        assert!(s.text.contains("Описание фильма."));
    }
}
