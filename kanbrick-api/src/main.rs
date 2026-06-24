//! # kanbrick-api (binary)
//!
//! Boots the embedded store and serves the HTTP API (issues #15, #16). Two run
//! modes (#70): the default **control-plane** serves the public API (and, when an
//! internal token is set, the ClusterIP-only internal RPC surface for executors),
//! while **executor** mode runs guest WASM on a stateless pool, proxying graph and
//! event callbacks back to the control plane.

use std::path::PathBuf;

use chrono::Duration;
use clap::Parser;
use kanbrick_api::{
    build_executor, executor_router, internal_router, router, spawn_reconcile_loop,
    AdmissionConfig, ApiConfig, AppState, ExecutorConfig, DEFAULT_ASSET_DIR,
    DEFAULT_RECONCILE_INTERVAL,
};
use kanbrick_auth::JwtAuthenticator;
use kanbrick_store::Store;

/// Serve the Kanbrick-V1 HTTP API.
#[derive(Parser)]
#[command(name = "kanbrick-api", version, about)]
struct Cli {
    /// Port to bind (public API in control-plane mode; `/internal/invoke` +
    /// `/metrics` + `/health` in executor mode).
    #[arg(long, default_value_t = 8080)]
    port: u16,
    /// Path to the graph database directory (control-plane mode only).
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
    /// Run mode: `control-plane` (default) or `executor` (#70). Overrides
    /// `KANBRICK_MODE`.
    #[arg(long)]
    mode: Option<String>,
    /// Port for the control plane's internal RPC surface (#69/#70), served only
    /// when an internal token is set. Overrides `KANBRICK_INTERNAL_PORT`; defaults
    /// to 8090. Control-plane mode only.
    #[arg(long)]
    internal_port: Option<u16>,
}

/// Dev-only fallback signing secret used when `KANBRICK_JWT_SECRET` is unset.
const DEV_SECRET: &str = "kanbrick-v1-insecure-dev-secret-set-KANBRICK_JWT_SECRET";

/// Default per-guest concurrency, matching the mesh `Scheduler` (#63).
const DEFAULT_GUEST_CONCURRENCY: usize = 4;
/// Default per-guest queue depth before overload (`429`) sheds load (#63).
const DEFAULT_GUEST_QUEUE_LIMIT: usize = 32;
/// Default port for the control plane's internal RPC surface (#69/#70).
const DEFAULT_INTERNAL_PORT: u16 = 8090;

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

/// Read a non-empty environment variable, treating unset/empty as `None`.
fn env_nonempty(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.is_empty())
}

/// Resolve admission limits from the CLI/env (shared by both modes).
fn admission_config(cli: &Cli) -> AdmissionConfig {
    // Precedence: CLI flag > env var > default (#63).
    let guest_concurrency = cli
        .guest_concurrency
        .or_else(|| env_usize("KANBRICK_GUEST_CONCURRENCY"))
        .unwrap_or(DEFAULT_GUEST_CONCURRENCY);
    let queue_limit = env_usize("KANBRICK_GUEST_QUEUE_LIMIT").unwrap_or(DEFAULT_GUEST_QUEUE_LIMIT);
    AdmissionConfig {
        guest_concurrency,
        queue_limit,
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

    let mode = cli
        .mode
        .clone()
        .or_else(|| env_nonempty("KANBRICK_MODE"))
        .unwrap_or_else(|| "control-plane".to_string());

    match mode.as_str() {
        "control-plane" => run_control_plane(cli).await,
        "executor" => run_executor(cli).await,
        other => {
            Err(format!("unknown mode {other:?}; expected `control-plane` or `executor`").into())
        }
    }
}

/// Control-plane mode: the public API, plus the ClusterIP-only internal RPC
/// surface when an internal token is configured.
async fn run_control_plane(cli: Cli) -> Result<(), Box<dyn std::error::Error>> {
    let secret = std::env::var("KANBRICK_JWT_SECRET").unwrap_or_else(|_| {
        tracing::warn!("KANBRICK_JWT_SECRET not set — using the insecure dev secret");
        DEV_SECRET.to_string()
    });

    let admission = admission_config(&cli);
    let asset_dir = cli
        .asset_dir
        .clone()
        .or_else(|| std::env::var_os("KANBRICK_ASSET_DIR").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from(DEFAULT_ASSET_DIR));
    // Shared transport secret for the internal RPC surface (#69) and for talking
    // to the executor pool (#70). Empty/unset leaves both disabled.
    let internal_token = env_nonempty("KANBRICK_INTERNAL_TOKEN");
    // Executor pool URL (#70). When set with a token, invocations are forwarded.
    let executor_url = env_nonempty("KANBRICK_EXECUTOR_URL");

    let config = ApiConfig {
        admission,
        asset_dir,
        internal_token: internal_token.clone(),
        executor_url,
    };

    let store = Store::open(&cli.db)?;
    let jwt = JwtAuthenticator::new(secret.as_bytes(), Duration::hours(cli.ttl_hours));
    let state = AppState::with_config(store, jwt, config)?;

    // Serve the internal RPC surface on its own ClusterIP-only listener so it is
    // never reachable through the public ingress (#69/#70; enforced in #71).
    if internal_token.is_some() {
        let internal_port = cli
            .internal_port
            .or_else(|| env_usize("KANBRICK_INTERNAL_PORT").map(|p| p as u16))
            .unwrap_or(DEFAULT_INTERNAL_PORT);
        let internal_addr = format!("0.0.0.0:{internal_port}");
        let internal_listener = tokio::net::TcpListener::bind(&internal_addr).await?;
        let internal_app = internal_router(state.clone());
        tracing::info!("internal RPC surface listening on {internal_addr}");
        tokio::spawn(async move {
            if let Err(e) = axum::serve(internal_listener, internal_app).await {
                tracing::error!(error = %e, "internal RPC surface terminated");
            }
        });
    }

    let app = router(state);
    let addr = format!("0.0.0.0:{}", cli.port);
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    tracing::info!("kanbrick-api (control-plane) listening on {addr}");
    axum::serve(listener, app).await?;
    Ok(())
}

/// Executor mode: a stateless guest-execution pool. No store, no JWT, no public
/// surface — just `/internal/invoke` (transport-secret gated), `/metrics`, and
/// `/health`. Graph/event callbacks proxy to the control plane.
async fn run_executor(cli: Cli) -> Result<(), Box<dyn std::error::Error>> {
    let cp_url = env_nonempty("KANBRICK_CP_URL")
        .ok_or("executor mode requires KANBRICK_CP_URL (the control plane's internal RPC URL)")?;
    let internal_token = env_nonempty("KANBRICK_INTERNAL_TOKEN")
        .ok_or("executor mode requires KANBRICK_INTERNAL_TOKEN (the shared transport secret)")?;

    let mut config = ExecutorConfig::new(cp_url, internal_token);
    config.admission = admission_config(&cli);

    // Boot performs blocking HTTP to the control plane (registry + asset replay);
    // run it off the async runtime.
    let executor = tokio::task::spawn_blocking(move || build_executor(config)).await??;
    spawn_reconcile_loop(executor.clone(), DEFAULT_RECONCILE_INTERVAL);

    let app = executor_router(executor);
    let addr = format!("0.0.0.0:{}", cli.port);
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    tracing::info!("kanbrick-api (executor) listening on {addr}");
    axum::serve(listener, app).await?;
    Ok(())
}
