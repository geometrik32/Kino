# Kino

Свой домашний кинотеатр: каталог фильмов и сериалов, просмотр прямо из торрентов и
**продолжение с того же места на любом устройстве** — та же раздача, та же серия, та же секунда.
Работает как сайт, как Telegram Mini App, а позже как приложение для компьютера и телефона.

Архитектура и план: [дизайн-документ](https://claude.ai/artifact/KWsEZ3i2LRduhX3bd29x94).

## Что уже есть (этап 1: каркас)

- `crates/server` — API на Rust (axum + PostgreSQL):
  - вход через Telegram: Mini App (`initData`) и Login Widget для сайта, с проверкой подписи;
  - доступ только по приглашениям; админы задаются в `ADMIN_TELEGRAM_IDS`;
  - прогресс просмотра: позиция, серия, выбранная раздача и файл; «Продолжить просмотр»;
  - список «Хочу посмотреть»;
  - раздаёт собранный веб-интерфейс.
- `crates/bot` — Telegram-бот (teloxide): `/start` открывает приложение, `/invite` выдаёт приглашение.
- `crates/core` — общие типы для сервера и клиентов.
- `app` — интерфейс на Dioxus (чистый Rust): вход, «Продолжить просмотр», «Хочу посмотреть».

Дальше: каталог из TMDB, поиск раздач через Prowlarr, плеер через TorrServer, предзагрузка.

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

## Установка на TrueNAS

1. CI собирает образ `ghcr.io/geometrik32/kino` при каждом пуше в `main`
   (для приватного репозитория на сервере нужен `docker login ghcr.io`).
2. Скопируйте `deploy/` в папку с compose-проектами, `.env.example` → `.env`, заполните.
3. В @BotFather: `/setdomain` → ваш `KINO_HOST` (для входа на сайте).
4. `docker compose up -d`. Сайт откроется на `https://KINO_HOST` через уже работающий Traefik.
