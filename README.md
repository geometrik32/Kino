# Kino

Свой домашний кинотеатр: каталог фильмов и сериалов, просмотр прямо из торрентов и
**продолжение с того же места на любом устройстве** — та же раздача, та же серия, та же секунда.
Работает как сайт, как Telegram Mini App, а позже как приложение для компьютера и телефона.

Архитектура и план: [дизайн-документ](https://claude.ai/artifact/KWsEZ3i2LRduhX3bd29x94).

## Что уже есть (этапы 1–2: каркас и каталог)

- `crates/server` — API на Rust (axum + PostgreSQL):
  - вход через Telegram: Mini App (`initData`) и Login Widget для сайта, с проверкой подписи;
  - доступ только по приглашениям; админы задаются в `ADMIN_TELEGRAM_IDS`;
  - прогресс просмотра: позиция, серия, выбранная раздача и файл; «Продолжить просмотр»;
  - список «Хочу посмотреть»;
  - раздаёт собранный веб-интерфейс.
- `crates/bot` — Telegram-бот (teloxide): `/start` открывает приложение, `/invite` выдаёт приглашение.
- каталог фильмов и сериалов из TMDB на русском (подборки, поиск, страница тайтла, сезоны и серии,
  прокси для постеров); без `TMDB_API_KEY` — встроенный демо-каталог.
- `crates/core` — общие типы для сервера и клиентов.
- `crates/ui` — экраны на Dioxus (чистый Rust): главная, поиск, «Хочу посмотреть», страница фильма
  и сериала с прогрессом по сериям, выбор раздачи (пока макет), вход.
- `app` — веб-клиент: подключает экраны из `crates/ui` к API.
- `crates/preview` — рендер всех экранов на демо-данных в HTML (для скриншотов без сборки WebAssembly):
  `cargo run -p kino-preview -- preview/`.

Этап 3 — раздачи и плеер (`crates/server/src/torrent`, `play.rs`):

- поиск раздач по всем трекерам через Jackett (Torznab); из названия раздачи читаются качество,
  перевод, HDR/Dolby Vision, субтитры, сезон, год и кодек; `.torrent` скачивается при выборе;
- выбор файла серии внутри раздачи (S02E05, «Серия 5», папки сезонов, паки);
- просмотр через TorrServer: сервер проксирует поток с перемоткой (Range) по подписанной ссылке на 12 часов;
- ffmpeg на лету делает из HEVC/AC3/DTS/MKV то, что играет браузер (H.264 + AAC в MP4), на Intel GPU
  (VA-API/Quick Sync); перемотка в этом режиме перезапускает кодирование с нужной секунды;
- звуковые дорожки и субтитры (в WebVTT) из файла; выбор запоминается для тайтла, иначе берутся
  настройки профиля;
- плеер: продолжение с места, сохранение позиции каждые 10 секунд с именем устройства, следующая серия;
- «Скачать заранее» через qBittorrent на HDD: прогресс в профиле, после загрузки играет с диска,
  после просмотра удаляется вместе с файлами.

## API

| Метод | Путь | Что делает |
| --- | --- | --- |
| POST | `/api/auth/webapp` | Вход из Mini App: `{init_data, invite?}` → `{token, me}` |
| POST | `/api/auth/widget` | Вход с сайта: поля Login Widget |
| GET | `/api/me` | Текущий пользователь |
| GET | `/api/continue` | «Продолжить просмотр» |
| PUT | `/api/progress` | Сохранить позицию (плеер шлёт каждые ~10 с) |
| GET | `/api/progress/{movie\|tv}/{tmdb_id}` | Позиции по всем сериям тайтла |
| GET | `/api/watchlist` | «Хочу посмотреть» |
| PUT/DELETE | `/api/watchlist/{movie\|tv}/{tmdb_id}` | Добавить / убрать |
| POST | `/api/invites` | Создать приглашение (админ) |
| GET | `/api/catalog/home` | Подборки для главной |
| GET | `/api/catalog/search?q=` | Поиск фильмов и сериалов |
| GET | `/api/catalog/{movie\|tv}/{tmdb_id}` | Карточка тайтла |
| GET | `/api/catalog/{movie\|tv}/{tmdb_id}/extras` | Актёры, «Рекомендации» и «Похожие» (TMDB) |
| GET | `/api/catalog/tv/{tmdb_id}/season/{n}` | Серии сезона |
| GET | `/api/img/{size}/{file}` | Постер через сервер (без токена, кешируется) |
| GET | `/api/releases/{movie\|tv}/{tmdb_id}?season=` | Раздачи с трекеров (или демо без Jackett) |
| GET | `/api/play/progress/{release}` | Сколько подгружено до старта: фаза, процент, раздающие, скорость |
| POST | `/api/play` | Начать просмотр: файл, дорожки, место продолжения, ссылка на поток |
| GET | `/api/stream/{token}[?audio=&start=]` | Видео (без токена в заголовке, ссылка подписана) |
| GET | `/api/stream/{token}/sub/{n}.vtt` | Субтитры в WebVTT |

Авторизация: `Authorization: Bearer <token>`.

## Разработка

Нужны Rust (stable) и PostgreSQL.

```sh
cargo test --workspace            # тестам нужен DATABASE_URL, например postgres://kino:kino@localhost/kino
cargo run -p kino-server          # переменные окружения — в deploy/.env.example
```

Интерфейс собирается [Dioxus CLI](https://dioxuslabs.com/) 0.7:

```sh
rustup target add wasm32-unknown-unknown
cd app && dx serve                # разработка
cd app && dx bundle --web --release
```

## Telegram Mini App

Тот же сайт открывается внутри Telegram и ведёт себя как часть мессенджера (`app/src/telegram.rs`, `app/src/native.rs`):

- вход без пароля по подписанным данным Telegram (`initData`), как и раньше;
- цвета берутся из темы Telegram пользователя (тёмная, чёрная, светлая); красный акцент остаётся фирменным;
- отступы под вырез и «чёлку» (`--tg-safe-area-inset-*`), высота экрана без «прыжков» (`--tg-viewport-stable-height`);
- кнопка «Назад» Telegram (и жест «назад» на Android) возвращает с тайтла на предыдущий экран, закрывает выбор раздачи и плеер;
- на странице тайтла вместо нижней панели — большая кнопка Telegram «▶ Смотреть S01E01» / «Продолжить»;
- лёгкая вибрация при смене вкладки, статуса, оценки, старте просмотра;
- на телефоне плеер открывается на весь экран, шапка Telegram прячется;
- ссылка на тайтл: `https://<сайт>/?open=tv_1396` (или `movie_550`); так открывается сериал из уведомления о новой серии;
- бот ставит синюю кнопку «Кино» рядом с полем ввода сам (при каждом запуске), настраивать в @BotFather ничего не нужно.

Прямую ссылку `t.me/<бот>/<имя>` можно получить в @BotFather: `/newapp` → выбрать бота → адрес `https://KINO_HOST`.

### Приложение для телефона (Android)

Тот же интерфейс на Dioxus, в системном WebView; запросы к серверу идут из Rust (`reqwest`),
вход — через бота (приложение показывает код, пользователь нажимает «Запустить» в Telegram).
Всё, что отличается от сайта, лежит в `app/src/platform/`.

```sh
rustup target add aarch64-linux-android
# нужны Android SDK (platforms;android-34, build-tools;34.0.0) и NDK 27: ANDROID_HOME, ANDROID_NDK_HOME
cd app && dx bundle --android --release --features mobile --no-default-features
# адрес сервера по умолчанию — KINO_SERVER_URL=https://kino.example.com (иначе https://kino.aat-terra.ru)
```

## Установка на TrueNAS

1. CI собирает образ `ghcr.io/geometrik32/kino` при каждом пуше в `main`
   (для приватного репозитория на сервере нужен `docker login ghcr.io`).
2. Скопируйте `deploy/` в папку с compose-проектами, `.env.example` → `.env`, заполните.
3. В @BotFather: `/setdomain` → ваш `KINO_HOST` (для входа на сайте).
4. `docker compose up -d`. Сайт откроется на `https://KINO_HOST` через уже работающий Traefik.
5. Jackett (`http://<сервер>:9117`, только в локальной сети): добавить трекеры, API-ключ — в `.env`.
   TorrServer свой, внутри стека `orel-kino` (Lampa и её TorrServer не затрагиваются).
   qBittorrent (`http://<сервер>:8085`): пароль из логов первого запуска сменить и записать в `.env`.
