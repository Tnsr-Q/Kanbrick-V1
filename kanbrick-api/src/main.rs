//! # kanbrick-api (binary)
//!
//! Boots the embedded store and serves the HTTP API (issues #15, #16).

use std::path::PathBuf;

use chrono::Duration;
use clap::Parser;
use kanbrick_api::{router, AdmissionConfig, ApiConfig, AppState, DEFAULT_ASSET_DIR};
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
    /// Max guest invocations running concurrently, per guest (#63). Overrides
    /// `KANBRICK_GUEST_CONCURRENCY`; both default to 4.
    #[arg(long)]
    guest_concurrency: Option<usize>,
    /// Root of the content-addressed guest asset volume (#64). Overrides
    /// `KANBRICK_ASSET_DIR`; both default to `/var/lib/kanbrick/assets`.
    #[arg(long)]
    asset_dir: Option<PathBuf>,
}

/// Dev-only fallback signing secret used when `KANBRICK_JWT_SECRET` is unset.
const DEV_SECRET: &str = "kanbrick-v1-insecure-dev-secret-set-KANBRICK_JWT_SECRET";

/// Default per-guest concurrency, matching the mesh `Scheduler` (#63).
const DEFAULT_GUEST_CONCURRENCY: usize = 4;
/// Default per-guest queue depth before overload (`429`) sheds load (#63).
const DEFAULT_GUEST_QUEUE_LIMIT: usize = 32;

/// Resolve a `usize` setting from an environment variable, warning (and falling
/// back) if it is set but unparseable.
fn env_usize(key: &str) -> Option<usize> {
    match std::env::var(key) {
        Ok(raw) => match raw.parse::<usize>() {
            Ok(v) => Some(v),
            Err(_) => {
                tracing::warn!("{key}={raw:?} is not a valid integer — ignoring");
                None
            }
        },
        Err(_) => None,
    }
}

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

    // Precedence: CLI flag > env var > default (#63).
    let guest_concurrency = cli
        .guest_concurrency
        .or_else(|| env_usize("KANBRICK_GUEST_CONCURRENCY"))
        .unwrap_or(DEFAULT_GUEST_CONCURRENCY);
    let queue_limit = env_usize("KANBRICK_GUEST_QUEUE_LIMIT").unwrap_or(DEFAULT_GUEST_QUEUE_LIMIT);
    let asset_dir = cli
        .asset_dir
        .or_else(|| std::env::var_os("KANBRICK_ASSET_DIR").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from(DEFAULT_ASSET_DIR));
    // Shared transport secret for the internal RPC surface (#69). The internal
    // listener itself is wired in the executor split (#70); until then this just
    // populates the config. Empty/unset leaves the internal surface disabled.
    let internal_token = std::env::var("KANBRICK_INTERNAL_TOKEN")
        .ok()
        .filter(|t| !t.is_empty());
    let config = ApiConfig {
        admission: AdmissionConfig {
            guest_concurrency,
            queue_limit,
        },
        asset_dir,
        internal_token,
    };

    let store = Store::open(&cli.db)?;
    let jwt = JwtAuthenticator::new(secret.as_bytes(), Duration::hours(cli.ttl_hours));
    let app = router(AppState::with_config(store, jwt, config)?);

    let addr = format!("0.0.0.0:{}", cli.port);
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    tracing::info!("kanbrick-api listening on {addr}");
    axum::serve(listener, app).await?;
    Ok(())
}
