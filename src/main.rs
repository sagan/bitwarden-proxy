use std::sync::Arc;

use anyhow::Context;
use bitwarden_proxy::{features, router, AppState, Cli, Config};
use clap::Parser;
use tokio::net::TcpListener;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::registry()
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .with(tracing_subscriber::fmt::layer())
        .init();

    let cli = Cli::parse();
    let env = std::env::vars_os()
        .filter_map(|(name, value)| Some((name.into_string().ok()?, value.into_string().ok()?)));
    let features = features::collect(env, &cli.feature)?;
    let config =
        Config::new(&cli.server, &cli.proxy, &cli.bind, cli.root_page)?.with_features(features);
    let state = AppState::new(&config)?;

    let listener = TcpListener::bind(config.bind)
        .await
        .with_context(|| format!("failed to bind {}", config.bind))?;

    tracing::info!(
        bind = %config.bind,
        server = %config.server.description(),
        proxy = %config.proxy_url,
        root_page = config.root_page,
        feature_overrides = config.features.len(),
        "bitwarden-proxy listening"
    );

    axum::serve(listener, router(Arc::clone(&state)))
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    Ok(())
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
}
