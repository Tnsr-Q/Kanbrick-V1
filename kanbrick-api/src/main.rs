//! # kanbrick-api (binary)
//!
//! Boots the embedded store and serves the HTTP API (issues #15, #16).

use chrono::Duration;
use clap::Parser;
use kanbrick_api::{router, AppState};
use kanbrick_auth::JwtAuthenticator;
use kanbrick_store::Store;

/// Serve the Kanbrick-V1 HTTP API.
#[derive(Parser)]
#[command(name = "kanbrick-api", version, about)]
struct Cli {
    /// Port to bind.
    #[arg(long, default_value_t = 8080)]
    port: u16,
    /// Path to the graph database directory.
    #[arg(long, default_value = "graph/firm.db")]
    db: String,
    /// Session TTL in hours.
    #[arg(long, default_value_t = 8)]
    ttl_hours: i64,
}

/// Dev-only fallback signing secret used when `KANBRICK_JWT_SECRET` is unset.
const DEV_SECRET: &str = "kanbrick-v1-insecure-dev-secret-set-KANBRICK_JWT_SECRET";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let cli = Cli::parse();

    let secret = std::env::var("KANBRICK_JWT_SECRET").unwrap_or_else(|_| {
        tracing::warn!("KANBRICK_JWT_SECRET not set — using the insecure dev secret");
        DEV_SECRET.to_string()
    });

    let store = Store::open(&cli.db)?;
    let jwt = JwtAuthenticator::new(secret.as_bytes(), Duration::hours(cli.ttl_hours));
    let app = router(AppState::new(store, jwt)?);

    let addr = format!("0.0.0.0:{}", cli.port);
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    tracing::info!("kanbrick-api listening on {addr}");
    axum::serve(listener, app).await?;
    Ok(())
}
