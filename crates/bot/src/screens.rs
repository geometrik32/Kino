//! What the bot says: texts and buttons, built from plain data so they can be
//! tested without Telegram. Texts are HTML (Telegram's subset), so every title
//! goes through [`esc`].

use kino_core::{clock, episode_code, ContinueItem, MediaKind, TitleDetails, TitleStatus, TitleSummary};
use kino_server::catalog::{RandomMovieFilter, RandomMoviePick};
use teloxide::{
    types::{InlineKeyboardButton, InlineKeyboardMarkup, KeyboardButton, KeyboardMarkup, WebAppInfo},
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

/// Callback data for the random movie recommender: `rnd:<op>:<state>`.
pub const RANDOM_CALLBACK: &str = "rnd:";

/// Callback data for daily trending digest settings: `trend:<op>`.
pub const TREND_CALLBACK: &str = "trend:";

pub const BTN_OPEN_APP: &str = "🎬 Открыть кинопоказ";
pub const BTN_RANDOM: &str = "🎲 Случайный фильм";
pub const BTN_TRENDING: &str = "🔥 Сегодня в тренде";
pub const BTN_SEARCH: &str = "🔍 Поиск";
pub const BTN_CONTINUE: &str = "▶ Продолжить";
pub const BTN_MY: &str = "📚 Мои списки";
pub const BTN_LOGIN: &str = "🌐 Вход на сайт";
pub const BTN_HELP: &str = "❓ Помощь";
pub const BTN_INVITE: &str = "🎟 Пригласить";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BotAction {
    Random,
    Trending,
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
            Self::Random => "random",
            Self::Trending => "trending",
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
            BTN_RANDOM => Some(Self::Random),
            BTN_TRENDING => Some(Self::Trending),
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
            "random" => Some(Self::Random),
            "trending" => Some(Self::Trending),
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

const RANDOM_GENRES: &[(i32, &str)] = &[
    (28, "Боевик"),
    (35, "Комедия"),
    (18, "Драма"),
    (878, "Фантастика"),
    (53, "Триллер"),
    (27, "Ужасы"),
    (9648, "Детектив"),
    (10749, "Мелодрама"),
    (12, "Приключения"),
    (16, "Мультфильм"),
    (80, "Криминал"),
    (14, "Фэнтези"),
];

const RANDOM_RUNTIMES: &[(Option<i32>, &str)] = &[
    (None, "Любое"),
    (Some(80), "~80 мин"),
    (Some(105), "~105 мин"),
    (Some(135), "~135 мин"),
    (Some(170), "~170 мин"),
];

const RANDOM_RATINGS: &[(Option<f32>, &str)] = &[
    (None, "Любой"),
    (Some(6.0), "6.0+"),
    (Some(7.0), "7.0+"),
    (Some(7.5), "7.5+"),
    (Some(8.0), "8.0+"),
];

const RANDOM_DEVIATIONS: &[(u8, &str)] = &[
    (0, "0% (строго)"),
    (10, "±10%"),
    (20, "±20%"),
    (35, "±35%"),
    (50, "±50%"),
];

/// Stateless filter configuration encoded into Telegram `callback_data`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RandomState {
    pub genre: usize,
    pub runtime: usize,
    pub rating: usize,
    pub deviation: usize,
}

impl Default for RandomState {
    fn default() -> Self {
        Self {
            genre: 0,
            runtime: 0,
            rating: 0,
            deviation: 2, // ±20%
        }
    }
}

impl RandomState {
    pub fn encode(self) -> String {
        format!("{}:{}:{}:{}", self.genre, self.runtime, self.rating, self.deviation)
    }

    pub fn decode(s: &str) -> Option<Self> {
        let mut parts = s.split(':');
        let genre: usize = parts.next()?.parse().ok()?;
        let runtime: usize = parts.next()?.parse().ok()?;
        let rating: usize = parts.next()?.parse().ok()?;
        let deviation: usize = parts.next()?.parse().ok()?;
        if parts.next().is_some() {
            return None;
        }
        if genre > RANDOM_GENRES.len()
            || runtime >= RANDOM_RUNTIMES.len()
            || rating >= RANDOM_RATINGS.len()
            || deviation >= RANDOM_DEVIATIONS.len()
        {
            return None;
        }
        Some(Self {
            genre,
            runtime,
            rating,
            deviation,
        })
    }

    pub fn genre_label(self) -> &'static str {
        if self.genre == 0 {
            "Любой"
        } else {
            RANDOM_GENRES[self.genre - 1].1
        }
    }

    pub fn runtime_label(self) -> &'static str {
        RANDOM_RUNTIMES[self.runtime].1
    }

    pub fn rating_label(self) -> &'static str {
        RANDOM_RATINGS[self.rating].1
    }

    pub fn deviation_label(self) -> &'static str {
        RANDOM_DEVIATIONS[self.deviation].1
    }

    pub fn to_filter(self) -> RandomMovieFilter {
        let (genre_id, genre_name) = if self.genre == 0 {
            (None, None)
        } else {
            let (id, name) = RANDOM_GENRES[self.genre - 1];
            (Some(id), Some(name.to_lowercase()))
        };
        RandomMovieFilter {
            genre_id,
            genre_name,
            target_runtime_min: RANDOM_RUNTIMES[self.runtime].0,
            min_rating: RANDOM_RATINGS[self.rating].0,
            deviation_pct: RANDOM_DEVIATIONS[self.deviation].0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RandomSubmenu {
    Genre,
    Runtime,
    Rating,
    Deviation,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RandomCallback {
    Any,
    Go(RandomState),
    Menu(RandomState),
    Submenu(RandomSubmenu, RandomState),
}

pub fn parse_random_callback(suffix: &str) -> Option<RandomCallback> {
    if suffix == "any" {
        return Some(RandomCallback::Any);
    }
    let (op, rest) = suffix.split_once(':')?;
    let state = RandomState::decode(rest)?;
    match op {
        "go" => Some(RandomCallback::Go(state)),
        "menu" => Some(RandomCallback::Menu(state)),
        "mg" => Some(RandomCallback::Submenu(RandomSubmenu::Genre, state)),
        "mt" => Some(RandomCallback::Submenu(RandomSubmenu::Runtime, state)),
        "mr" => Some(RandomCallback::Submenu(RandomSubmenu::Rating, state)),
        "md" => Some(RandomCallback::Submenu(RandomSubmenu::Deviation, state)),
        _ => None,
    }
}

/// The app opened on a title: `<site>/?open=tv_1396`.
pub fn title_url(base: &Url, kind: MediaKind, tmdb_id: i64) -> Url {
    let mut url = base.clone();
    url.query_pairs_mut()
        .append_pair("open", &format!("{}_{tmdb_id}", kind.as_str()));
    url
}

/// The app opened on a title and started playing right away: `<site>/?open=tv_1396&play=1`.
pub fn title_play_url(base: &Url, kind: MediaKind, tmdb_id: i64) -> Url {
    let mut url = title_url(base, kind, tmdb_id);
    url.query_pairs_mut().append_pair("play", "1");
    url
}

fn open_title_url(label: String, url: Url) -> InlineKeyboardButton {
    InlineKeyboardButton::web_app(label, WebAppInfo { url })
}

fn open_title(label: String, base: &Url, kind: MediaKind, tmdb_id: i64) -> InlineKeyboardButton {
    open_title_url(label, title_url(base, kind, tmdb_id))
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

    /// Telegram forbids `WebApp` inline buttons outside private chats (`400 BUTTON_TYPE_INVALID`).
    /// Converts them to regular URL buttons when sending to a group or channel.
    pub fn for_chat(mut self, is_private: bool) -> Self {
        if !is_private {
            for row in &mut self.keyboard.inline_keyboard {
                for btn in row {
                    if let teloxide::types::InlineKeyboardButtonKind::WebApp(w) = &btn.kind {
                        btn.kind = teloxide::types::InlineKeyboardButtonKind::Url(w.url.clone());
                    }
                }
            }
        }
        self
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

/// Persistent keyboard in private chats showing bot commands without duplicating the left WebApp button.
pub fn main_reply_keyboard(is_admin: bool) -> KeyboardMarkup {
    let mut rows = vec![
        vec![KeyboardButton::new(BTN_RANDOM), KeyboardButton::new(BTN_TRENDING)],
        vec![KeyboardButton::new(BTN_SEARCH), KeyboardButton::new(BTN_CONTINUE)],
        vec![KeyboardButton::new(BTN_MY), KeyboardButton::new(BTN_LOGIN)],
        vec![KeyboardButton::new(BTN_HELP)],
    ];
    if is_admin {
        rows.last_mut().unwrap().push(KeyboardButton::new(BTN_INVITE));
    }
    KeyboardMarkup::new(rows)
        .resize_keyboard()
        .persistent()
        .input_field_placeholder("Введите название фильма или выберите кнопку…")
}

fn command_inline_rows(base: &Url, is_admin: bool) -> Vec<Vec<InlineKeyboardButton>> {
    let cmd = |label: &str, action: BotAction| {
        InlineKeyboardButton::callback(label, format!("{CMD_CALLBACK}{}", action.as_str()))
    };
    let mut rows = vec![
        vec![open_app(base, BTN_OPEN_APP)],
        vec![
            cmd(BTN_RANDOM, BotAction::Random),
            cmd(BTN_TRENDING, BotAction::Trending),
        ],
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
    "<b>Орловский кинопоказ</b>\n\
     Ваш личный кинотеатр с единой историей и прогрессом на всех устройствах.\n\n\
     • Кнопка <b>«Кино»</b> слева от поля ввода открывает кинотеатр\n\
     • Напишите название фильма в чат — пришлю карточки с постером и описанием\n\
     • Кнопки внизу запускают подбор случайного фильма, поиск, продолжение и списки"
}

pub fn invite_welcome(app_url: &Url) -> Screen {
    Screen::new(
        start_welcome_text().to_owned(),
        vec![vec![open_app(app_url, "🎬 Открыть по приглашению")]],
    )
}

pub fn start_inline(app_url: &Url, is_admin: bool) -> Screen {
    Screen::new(start_welcome_text().to_owned(), command_inline_rows(app_url, is_admin))
}

/// Shown in an empty chat with the bot, above the "Начать" button (Telegram allows 512 characters).
pub const BOT_DESCRIPTION: &str = "Орловский кинопоказ: ваш личный кинотеатр.\n\n\
    Ищите фильмы и сериалы, подбирайте случайное кино по жанру и времени, смотрите в Telegram, на сайте и в приложении с того же места.\n\n\
    Нажмите «Начать».";

/// Shown on the bot's profile and when it is shared (Telegram allows 120 characters).
pub const BOT_SHORT_DESCRIPTION: &str =
    "Личный кинотеатр: поиск, подбор случайного фильма и просмотр с любого устройства.";

pub fn help(base: &Url, is_admin: bool) -> Screen {
    let mut text = String::from(
        "<b>Орловский кинопоказ — что умеет бот</b>\n\n\
         <b>Поиск и подбор</b>\n\
         • /search <i>название</i> — поиск фильма или сериала (можно просто написать название в чат)\n\
         • /random — случайный фильм (без рамок или по жанру, длительности, рейтингу и допуску)\n\
         • /trending — настроить ежедневную рассылку постеров «Сегодня в тренде»\n\n\
         <b>Просмотр и списки</b>\n\
         • /continue — продолжить с места остановки\n\
         • /my — списки: смотрю, жду, хочу посмотреть, просмотрено, брошено\n\n\
         <b>Доступ</b>\n\
         • /login — быстрый вход на сайт в браузере\n\
         • /start — открыть кинопоказ и показать кнопки команд\n\
         • /help — эта подсказка\n\n\
         <b>Уведомления</b>\n\
         Бот автоматически сообщает о выходе новых серий, премьер из списка «Жду» и завершении загрузок на сервер",
    );
    if is_admin {
        text.push_str("\n• /invite — создать одноразовое приглашение (админ)");
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
            "По запросу «{}» ничего не нашлось. Попробуйте другое название или подберите случайный фильм.",
            esc(query)
        ),
        vec![vec![InlineKeyboardButton::callback(
            BTN_RANDOM,
            format!("{RANDOM_CALLBACK}any"),
        )]],
    )
}

fn format_card(
    base: &Url,
    t: &TitleSummary,
    genres: &[String],
    runtime_min: Option<i32>,
    note: Option<&str>,
    extra_rows: Vec<Vec<InlineKeyboardButton>>,
) -> Screen {
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

    let note_clean = note.map(str::trim).filter(|n| !n.is_empty());
    let note_reserve = note_clean.map(|n| 2 + n.chars().count()).unwrap_or(0);

    if let Some(overview) = t.overview.as_deref().map(str::trim).filter(|o| !o.is_empty()) {
        let budget = CAPTION_MAX_CHARS.saturating_sub(plain_len + note_reserve + 2);
        if budget >= 20 {
            let short = truncate_chars(overview, budget);
            text.push_str(&format!("\n\n{}", esc(&short)));
        }
    }

    if let Some(note_text) = note_clean {
        text.push_str(&format!("\n\n<i>{}</i>", esc(note_text)));
    }

    let mut rows = vec![vec![open_title(BTN_OPEN_APP.into(), base, t.kind, t.tmdb_id)]];
    rows.extend(extra_rows);
    Screen::new(text, rows)
}

/// Movie or series card shown for search results (photo caption + open button).
pub fn title_card(base: &Url, t: &TitleSummary) -> Screen {
    format_card(base, t, &[], None, None, Vec::new())
}

/// Movie or series card enriched with details (genres and runtime).
pub fn title_details_card(base: &Url, d: &TitleDetails) -> Screen {
    format_card(base, &d.summary, &d.genres, d.runtime_min, None, Vec::new())
}

pub fn random_menu(state: RandomState) -> Screen {
    let bounds = state.to_filter().effective_bounds();
    let runtime_desc = match bounds.runtime_range {
        Some((lo, hi)) => format!("{} ({lo}–{hi} мин)", state.runtime_label()),
        None => "любая".to_owned(),
    };
    let rating_desc = match (RANDOM_RATINGS[state.rating].0, bounds.min_rating) {
        (Some(target), Some(eff)) if (target - eff).abs() >= 0.05 => {
            format!("от {target:.1} (с допуском: от {eff:.1})")
        }
        (Some(target), _) => format!("от {target:.1}"),
        _ => "любой".to_owned(),
    };
    let text = format!(
        "<b>🎲 Подбор случайного фильма</b>\n\
         Задайте желаемые рамки или сразу выберите случайный фильм.\n\n\
         • <b>Жанр:</b> {}\n\
         • <b>Длительность:</b> {runtime_desc}\n\
         • <b>Рейтинг:</b> {rating_desc}\n\
         • <b>Отклонение:</b> {}",
        state.genre_label().to_lowercase(),
        state.deviation_label(),
    );
    let enc = state.encode();
    let mut rows = vec![
        vec![InlineKeyboardButton::callback(
            "🎲 Подобрать по фильтрам",
            format!("{RANDOM_CALLBACK}go:{enc}"),
        )],
        vec![InlineKeyboardButton::callback(
            "⚡ Полностью случайный",
            format!("{RANDOM_CALLBACK}any"),
        )],
        vec![
            InlineKeyboardButton::callback(
                format!("🎭 Жанр: {}", state.genre_label()),
                format!("{RANDOM_CALLBACK}mg:{enc}"),
            ),
            InlineKeyboardButton::callback(
                format!("⏱ Время: {}", state.runtime_label()),
                format!("{RANDOM_CALLBACK}mt:{enc}"),
            ),
        ],
        vec![
            InlineKeyboardButton::callback(
                format!("⭐ Рейтинг: {}", state.rating_label()),
                format!("{RANDOM_CALLBACK}mr:{enc}"),
            ),
            InlineKeyboardButton::callback(
                format!("📊 Допуск: {}", state.deviation_label()),
                format!("{RANDOM_CALLBACK}md:{enc}"),
            ),
        ],
    ];
    if state != RandomState::default() {
        rows.push(vec![InlineKeyboardButton::callback(
            "↺ Сбросить фильтры",
            format!("{RANDOM_CALLBACK}menu:{}", RandomState::default().encode()),
        )]);
    }
    Screen::new(text, rows)
}

pub fn random_submenu(sub: RandomSubmenu, state: RandomState) -> Screen {
    let back = vec![InlineKeyboardButton::callback(
        "← К фильтрам",
        format!("{RANDOM_CALLBACK}menu:{}", state.encode()),
    )];
    let mark = |selected: bool, label: &str| {
        if selected {
            format!("✓ {label}")
        } else {
            label.to_owned()
        }
    };

    match sub {
        RandomSubmenu::Genre => {
            let mut rows = vec![vec![InlineKeyboardButton::callback(
                mark(state.genre == 0, "Любой жанр"),
                format!("{RANDOM_CALLBACK}menu:{}", RandomState { genre: 0, ..state }.encode()),
            )]];
            let genre_buttons: Vec<_> = RANDOM_GENRES
                .iter()
                .enumerate()
                .map(|(idx, (_, name))| {
                    let g = idx + 1;
                    let next = RandomState { genre: g, ..state }.encode();
                    InlineKeyboardButton::callback(
                        mark(state.genre == g, name),
                        format!("{RANDOM_CALLBACK}menu:{next}"),
                    )
                })
                .collect();
            rows.extend(genre_buttons.chunks(3).map(<[_]>::to_vec));
            rows.push(back);
            Screen::new("<b>🎭 Выберите жанр</b>".to_owned(), rows)
        }
        RandomSubmenu::Runtime => {
            let buttons: Vec<_> = RANDOM_RUNTIMES
                .iter()
                .enumerate()
                .map(|(idx, (_, label))| {
                    let next = RandomState { runtime: idx, ..state }.encode();
                    InlineKeyboardButton::callback(
                        mark(state.runtime == idx, label),
                        format!("{RANDOM_CALLBACK}menu:{next}"),
                    )
                })
                .collect();
            let mut rows: Vec<_> = buttons.chunks(2).map(<[_]>::to_vec).collect();
            rows.push(back);
            Screen::new(
                "<b>⏱ Примерная длительность</b>\nИтоговый диапазон зависит от процента отклонения.".to_owned(),
                rows,
            )
        }
        RandomSubmenu::Rating => {
            let buttons: Vec<_> = RANDOM_RATINGS
                .iter()
                .enumerate()
                .map(|(idx, (_, label))| {
                    let next = RandomState { rating: idx, ..state }.encode();
                    InlineKeyboardButton::callback(
                        mark(state.rating == idx, label),
                        format!("{RANDOM_CALLBACK}menu:{next}"),
                    )
                })
                .collect();
            let mut rows: Vec<_> = buttons.chunks(2).map(<[_]>::to_vec).collect();
            rows.push(back);
            Screen::new(
                "<b>⭐ Минимальный рейтинг</b>\nПри выбранном отклонении порог может смягчаться.".to_owned(),
                rows,
            )
        }
        RandomSubmenu::Deviation => {
            let buttons: Vec<_> = RANDOM_DEVIATIONS
                .iter()
                .enumerate()
                .map(|(idx, (_, label))| {
                    let next = RandomState {
                        deviation: idx,
                        ..state
                    }
                    .encode();
                    InlineKeyboardButton::callback(
                        mark(state.deviation == idx, label),
                        format!("{RANDOM_CALLBACK}menu:{next}"),
                    )
                })
                .collect();
            let mut rows: Vec<_> = buttons.chunks(2).map(<[_]>::to_vec).collect();
            rows.push(back);
            Screen::new(
                "<b>📊 Процент отклонения от настроек</b>\nРасширяет допуск по длительности и снижает порог рейтинга на заданный процент.".to_owned(),
                rows,
            )
        }
    }
}

pub fn random_movie_card(base: &Url, pick: &RandomMoviePick, state: Option<RandomState>) -> Screen {
    let note = pick
        .relaxed
        .then_some("Точных совпадений по заданным рамкам не нашлось, поэтому фильтр был расширен.");
    let extra_rows = match state {
        Some(st) => {
            let enc = st.encode();
            vec![vec![
                InlineKeyboardButton::callback("🎲 Другой фильм", format!("{RANDOM_CALLBACK}go:{enc}")),
                InlineKeyboardButton::callback("⚙ Фильтры", format!("{RANDOM_CALLBACK}menu:{enc}")),
            ]]
        }
        None => vec![vec![
            InlineKeyboardButton::callback("🎲 Ещё случайный", format!("{RANDOM_CALLBACK}any")),
            InlineKeyboardButton::callback(
                "⚙ По фильтрам",
                format!("{RANDOM_CALLBACK}menu:{}", RandomState::default().encode()),
            ),
        ]],
    };
    format_card(
        base,
        &pick.details.summary,
        &pick.details.genres,
        pick.details.runtime_min,
        note,
        extra_rows,
    )
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
        (MediaKind::Movie, _) if last.duration_sec > 0.0 => format!(
            "остановились на {} из {}",
            clock(last.position_sec),
            clock(last.duration_sec)
        ),
        (MediaKind::Movie, _) => format!("остановились на {}", clock(last.position_sec)),
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
        buttons.push(vec![open_title_url(
            format!("▶ {name}"),
            title_play_url(base, r.item.last.kind, r.item.last.tmdb_id),
        )]);
    }
    Screen::new(text, buttons)
}

fn status_icon(s: TitleStatus) -> &'static str {
    match s {
        TitleStatus::Watching => "▶",
        TitleStatus::WaitingEpisode => "⏳",
        TitleStatus::Want => "🔖",
        TitleStatus::WaitingRelease => "🗓",
        TitleStatus::Watched => "✅",
        TitleStatus::Dropped => "⏸",
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TrendPrefs {
    pub enabled: bool,
    pub include_tv: bool,
    pub send_hour: i16,
    pub item_limit: i16,
}

impl Default for TrendPrefs {
    fn default() -> Self {
        Self {
            enabled: false,
            include_tv: false,
            send_hour: 19,
            item_limit: 5,
        }
    }
}

pub fn trend_settings(p: &TrendPrefs) -> Screen {
    let status = if p.enabled {
        "Включена"
    } else {
        "Выключена"
    };
    let kinds = if p.include_tv {
        "Фильмы и сериалы"
    } else {
        "Только фильмы"
    };
    let text = format!(
        "<b>🔥 Сегодня в тренде</b>\n\
         Бот будет присылать подборку популярных постеров за день одним сообщением.\n\n\
         • <b>Рассылка:</b> {status}\n\
         • <b>Что присылать:</b> {kinds}\n\
         • <b>Время (МСК):</b> {:02}:00\n\
         • <b>Количество постеров:</b> {}",
        p.send_hour, p.item_limit
    );
    let cb = |op: &str| format!("{TREND_CALLBACK}{op}");
    let toggle_lbl = if p.enabled {
        "🔕 Выключить рассылку"
    } else {
        "🔔 Включить рассылку"
    };
    let kind_lbl = if p.include_tv {
        "🎬 Только фильмы"
    } else {
        "📺 Добавить сериалы"
    };
    let hr_btn = |h: i16| {
        let lbl = if p.send_hour == h {
            format!("✓ {h:02}:00")
        } else {
            format!("{h:02}:00")
        };
        InlineKeyboardButton::callback(lbl, cb(&format!("hr:{h}")))
    };
    let lim_btn = |n: i16| {
        let lbl = if p.item_limit == n {
            format!("✓ {n} шт.")
        } else {
            format!("{n} шт.")
        };
        InlineKeyboardButton::callback(lbl, cb(&format!("lim:{n}")))
    };
    let rows = vec![
        vec![InlineKeyboardButton::callback(toggle_lbl, cb("toggle"))],
        vec![InlineKeyboardButton::callback(kind_lbl, cb("tv"))],
        vec![hr_btn(10), hr_btn(14), hr_btn(19), hr_btn(21)],
        vec![lim_btn(3), lim_btn(5), lim_btn(8)],
        vec![InlineKeyboardButton::callback(
            "✨ Прислать сегодня в тренде сейчас",
            cb("now"),
        )],
    ];
    Screen::new(text, rows)
}

/// HTML caption attached to the first poster of a "Сегодня в тренде" media group (no external browser links).
pub fn trending_album_caption(items: &[TitleSummary]) -> String {
    let mut text = String::from("<b>🔥 Сегодня в тренде</b>\n");
    for (idx, item) in items.iter().enumerate() {
        let icon = if item.kind == MediaKind::Tv { "📺" } else { "🎬" };
        let name = name_with_year(item);
        let rate = item
            .rating
            .filter(|r| *r > 0.0)
            .map(|r| format!(" · ★ {r:.1}"))
            .unwrap_or_default();
        let line = format!("\n{}. {icon} <b>{}</b>{}", idx + 1, esc(&name), rate);
        if text.chars().count() + line.chars().count() > CAPTION_MAX_CHARS {
            break;
        }
        text.push_str(&line);
    }
    text
}

/// Mini App inline buttons under the "Сегодня в тренде" digest so titles open right inside Telegram.
pub fn trending_buttons(base: &Url, items: &[TitleSummary], is_private: bool) -> InlineKeyboardMarkup {
    let rows: Vec<Vec<InlineKeyboardButton>> = items
        .iter()
        .enumerate()
        .map(|(idx, item)| {
            let icon = if item.kind == MediaKind::Tv { "📺" } else { "🎬" };
            let name = truncate_chars(&name_with_year(item), 38);
            let label = format!("{}. {icon} {name}", idx + 1);
            let url = title_url(base, item.kind, item.tmdb_id);
            let btn = if is_private {
                open_title_url(label, url)
            } else {
                InlineKeyboardButton::url(label, url)
            };
            vec![btn]
        })
        .collect();
    InlineKeyboardMarkup::new(rows)
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
            user_rating: None,
            user_status: None,
            peers: Vec::new(),
            preloaded: false,
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
        assert_eq!(
            title_play_url(&base(), MediaKind::Tv, 1396).as_str(),
            "https://kino.example.com/?open=tv_1396&play=1"
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
    fn search_with_nothing_found_offers_random_movie() {
        let s = search_empty("ыыы");
        assert!(s.text.contains("ничего не нашлось"));
        assert_eq!(s.keyboard.inline_keyboard[0][0].text, BTN_RANDOM);
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
        let kb = main_reply_keyboard(false);
        let texts: Vec<_> = kb.keyboard.iter().flatten().map(|b| b.text.as_str()).collect();
        assert!(!texts.contains(&BTN_OPEN_APP));
        assert!(texts.contains(&BTN_RANDOM));
        assert!(texts.contains(&BTN_SEARCH));
        assert!(texts.contains(&BTN_CONTINUE));
        assert!(texts.contains(&BTN_MY));
        assert!(texts.contains(&BTN_LOGIN));
        assert!(texts.contains(&BTN_HELP));
        assert!(!texts.contains(&BTN_INVITE));

        let admin_kb = main_reply_keyboard(true);
        let admin_texts: Vec<_> = admin_kb.keyboard.iter().flatten().map(|b| b.text.as_str()).collect();
        assert!(admin_texts.contains(&BTN_INVITE));

        assert_eq!(BotAction::parse_button(BTN_RANDOM), Some(BotAction::Random));
        assert_eq!(BotAction::parse_button(BTN_SEARCH), Some(BotAction::Search));
        assert_eq!(BotAction::parse_button(BTN_CONTINUE), Some(BotAction::Continue));
        assert_eq!(BotAction::parse_button(BTN_MY), Some(BotAction::My));
        assert_eq!(BotAction::parse_button(BTN_LOGIN), Some(BotAction::Login));
        assert_eq!(BotAction::parse_button(BTN_HELP), Some(BotAction::Help));
        assert_eq!(BotAction::parse_button(BTN_INVITE), Some(BotAction::Invite));
        assert_eq!(BotAction::parse_button("Зеленая линия"), None);
    }

    #[test]
    fn random_state_round_trip_and_callback_sizes_fit_telegram_limit() {
        let st = RandomState {
            genre: 4,
            runtime: 2,
            rating: 3,
            deviation: 2,
        };
        let encoded = st.encode();
        assert_eq!(RandomState::decode(&encoded), Some(st));
        assert_eq!(RandomState::decode("99:0:0:0"), None);
        assert_eq!(
            parse_random_callback(&format!("go:{encoded}")),
            Some(RandomCallback::Go(st))
        );
        assert_eq!(parse_random_callback("any"), Some(RandomCallback::Any));

        for screen in [
            random_menu(st),
            random_submenu(RandomSubmenu::Genre, st),
            random_submenu(RandomSubmenu::Runtime, st),
            random_submenu(RandomSubmenu::Rating, st),
            random_submenu(RandomSubmenu::Deviation, st),
        ] {
            for btn in screen.keyboard.inline_keyboard.iter().flatten() {
                if let InlineKeyboardButtonKind::CallbackData(d) = &btn.kind {
                    assert!(d.len() <= 64, "callback_data too long: {d}");
                }
            }
        }
    }

    #[test]
    fn random_movie_card_has_rerun_and_filter_buttons() {
        let mut t = summary(MediaKind::Movie, 603, "Матрица", Some(1999));
        t.overview = Some("Хакер Нео узнаёт правду.".into());
        let pick = RandomMoviePick {
            details: TitleDetails {
                summary: t,
                genres: vec!["фантастика".into()],
                runtime_min: Some(136),
                tagline: None,
                seasons: vec![],
                imdb_id: None,
                last_episode: None,
                series_status: None,
                release_date: None,
                next_episode: None,
                alternative_titles: Vec::new(),
            },
            relaxed: true,
        };
        let st = RandomState::default();
        let s = random_movie_card(&base(), &pick, Some(st));
        assert!(s.text.contains("136 мин"));
        assert!(s.text.contains("фильтр был расширен"));
        assert_eq!(s.keyboard.inline_keyboard.len(), 2);
        assert_eq!(s.keyboard.inline_keyboard[1][0].text, "🎲 Другой фильм");
        assert_eq!(s.keyboard.inline_keyboard[1][1].text, "⚙ Фильтры");
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
        assert_eq!(
            web_app_url(&s.keyboard.inline_keyboard[0][0]),
            "https://kino.example.com/?open=tv_7&play=1"
        );
        assert_eq!(
            web_app_url(&s.keyboard.inline_keyboard[2][0]),
            "https://kino.example.com/?open=movie_7&play=1"
        );
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
        assert_eq!(flat.len(), TitleStatus::ALL.len());
        assert_eq!(flat[0].text, "▶ Смотрю · 2");
        match &flat[1].kind {
            InlineKeyboardButtonKind::CallbackData(d) => assert_eq!(d, "my:waiting_episode"),
            other => panic!("{other:?}"),
        }
        assert!(flat.iter().any(|b| matches!(
            &b.kind,
            InlineKeyboardButtonKind::CallbackData(d) if d == "my:want"
        )));
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
        assert!(user_buttons.contains(&BTN_RANDOM));
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
            series_status: None,
            release_date: None,
            next_episode: None,
            alternative_titles: Vec::new(),
        };
        let s = title_details_card(&base(), &d);
        assert!(s.text.contains("🎬 Фильм · ★ 8.2 · 136 мин"));
        assert!(s.text.contains("<i>фантастика, боевик</i>"));
        assert!(s.text.contains("Описание фильма."));
    }

    #[test]
    fn trend_settings_callbacks_fit_limit_and_album_caption_links_titles() {
        let p = TrendPrefs::default();
        let s = trend_settings(&p);
        for btn in s.keyboard.inline_keyboard.iter().flatten() {
            if let InlineKeyboardButtonKind::CallbackData(d) = &btn.kind {
                assert!(d.len() <= 64, "callback too long: {d}");
            }
        }
        let items = vec![
            summary(MediaKind::Movie, 603, "Матрица", Some(1999)),
            summary(MediaKind::Movie, 155, "Тёмный рыцарь", Some(2008)),
        ];
        let cap = trending_album_caption(&items);
        assert!(cap.contains("Сегодня в тренде"));
        assert!(!cap.contains("<a href="));
        assert!(cap.contains("Матрица (1999)"));
        assert!(cap.chars().count() <= 1024);
        let kb = trending_buttons(&base(), &items, true);
        assert_eq!(kb.inline_keyboard.len(), 2);
        assert_eq!(
            web_app_url(&kb.inline_keyboard[0][0]),
            "https://kino.example.com/?open=movie_603"
        );
    }
}
