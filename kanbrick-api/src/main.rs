//! # kanbrick-api
//!
//! HTTP API entry point. Phase 0 scaffold: prints a banner and exits 0 so the
//! workspace builds and runs end-to-end. The Axum server, routes, and auth
//! middleware are added in later phases.

fn main() {
    println!(
        "kanbrick-api {} — scaffold (firm: {})",
        env!("CARGO_PKG_VERSION"),
        kanbrick_core::FIRM_ID
    );
}
