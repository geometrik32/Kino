use std::sync::Arc;

use anyhow::Context;
use kino_server::{app, catalog, config::Config, db, AppState};
use sqlx::postgres::PgPoolOptions;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info,tower_http=info".into()))
        .init();

    let config = Config::from_env()?;
    tracing::info!(?config, "starting kino-server");

    let pool = PgPoolOptions::new()
        .max_connections(10)
        .connect(&config.database_url)
        .await
        .context("connecting to PostgreSQL")?;
    db::migrate(&pool).await.context("running migrations")?;

    let listener = tokio::net::TcpListener::bind(config.bind_addr)
        .await
        .with_context(|| format!("binding {}", config.bind_addr))?;
    tracing::info!("listening on {}", config.bind_addr);

    let image_cache = config
        .image_cache_dir
        .clone()
        .map(|dir| (dir, config.image_cache_max_mb));
    let catalog = catalog::from_settings_with_image_cache(
        config.tmdb_api_key.as_deref(),
        &config.tmdb_language,
        config.tmdb_proxy.as_deref(),
        image_cache,
    )?;
    let media = Arc::new(kino_server::play::Media::new(&config.torrents));
    let telegram =
        kino_server::config::http_client(config.telegram_proxy.as_deref(), std::time::Duration::from_secs(15))
            .unwrap_or_else(|e| {
                tracing::error!(error = %e, "TELEGRAM_PROXY is wrong, talking to Telegram directly");
                reqwest::Client::new()
            });
    let state = AppState {
        media,
        sync: Arc::new(kino_server::sync::SyncHub::new()),
        pool,
        config: Arc::new(config),
        catalog,
        telegram,
    };
    kino_server::jobs::spawn(state.clone(), state.config.notify_every);
    kino_server::sync::spawn_reaper(state.clone());
    axum::serve(listener, app(state))
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        if let Ok(mut sig) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            sig.recv().await;
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        _ = ctrl_c => {}
        _ = terminate => {}
    }
}
