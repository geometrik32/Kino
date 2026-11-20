use std::sync::Arc;

use anyhow::Context;
use kino_server::{
    app,
    catalog::{Catalog, DemoCatalog, TmdbCatalog},
    config::Config,
    db, AppState,
};
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

    let catalog: Arc<dyn Catalog> = match &config.tmdb_api_key {
        Some(key) => Arc::new(TmdbCatalog::new(
            key.clone(),
            config.tmdb_language.clone(),
            config.tmdb_proxy.as_deref(),
        )?),
        None => {
            tracing::warn!("TMDB_API_KEY is not set: serving the built-in demo catalog");
            Arc::new(DemoCatalog)
        }
    };
    let media = Arc::new(kino_server::play::Media::new(&config.torrents));
    let state = AppState {
        media,
        pool,
        config: Arc::new(config),
        catalog,
    };
    kino_server::jobs::spawn(state.clone(), state.config.notify_every);
    axum::serve(listener, app(state))
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}
