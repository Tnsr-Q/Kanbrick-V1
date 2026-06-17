//! # kanbrick-guest-sdk-example
//!
//! The reference WASM guest for the guest SDK (issue #39). It is intentionally
//! tiny but touches **every** SDK capability, so it doubles as living
//! documentation and as the integration-test fixture that proves the SDK works
//! end-to-end through the mesh (auth → mesh → guest → store).
//!
//! Given a request `{ "query": "<cypher>" }`, it:
//! 1. reads its caller's host-authoritative [`FirmContext`](kanbrick_guest_sdk::FirmContext),
//! 2. runs that query through the host (clearance-filtered by `GuardedStore`),
//! 3. emits an `example.completed` event carrying the row count,
//! 4. logs a line, and
//! 5. returns `{ "caller": <email>, "clearance": <level>, "row_count": <n> }`.

use kanbrick_guest_sdk as sdk;
use sdk::serde_json::json;
use sdk::{GraphQuery, GuestRequest, GuestResponse, LogLevel, Result};

/// The guest's single request handler. Wired to `kbk_run` by the SDK macro.
fn handle(request: GuestRequest) -> Result<GuestResponse> {
    sdk::log(LogLevel::Info, "sdk-example: started");

    let ctx = sdk::firm_context()?;

    // The caller chooses the query; default to counting visible companies.
    let cypher = request
        .payload
        .get("query")
        .and_then(|v| v.as_str())
        .unwrap_or("MATCH (c:Company) RETURN c.company_id, c.name");
    let rows = sdk::query_graph(&GraphQuery::new(cypher))?;

    sdk::emit(&sdk::Event::with_payload(
        "example.completed",
        json!({ "caller": ctx.email, "row_count": rows.len() }),
    ))?;

    Ok(GuestResponse::new(json!({
        "caller": ctx.email,
        "clearance": ctx.clearance,
        "row_count": rows.len(),
    })))
}

sdk::guest_entrypoint!(handle);
