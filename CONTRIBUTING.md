# Contributing

## Workflow

- One vertical slice per change; build → test → lint → format green at each step.
- The CI gate (and what you should run locally):

  ```bash
  cargo fmt --all --check
  cargo clippy --workspace --all-targets --all-features -- -D warnings
  cargo build --workspace --all-features
  cargo test  --workspace --all-features
  scripts/build-guests.sh        # the guests compile to wasm and stay < 10 MiB
  ```

- Record one-way-door decisions as an ADR in `docs/adr/`.
- The toolchain is pinned (`rust-toolchain.toml`, Rust 1.94.1 + `wasm32-wasip1`).
  Only `crates/sparrowdb` is required to build.

## Writing a new WASM guest

A guest is a small crate that compiles to **both** a native `rlib` (so its *pure
logic* is unit-tested fast) and a `wasm32-wasip1` `cdylib` (the sandboxed module
the mesh runs). It talks to the host only through `kanbrick-guest-sdk`.

### 1. The crate

`guests/greeter/Cargo.toml`:

```toml
[package]
name = "kanbrick-guest-greeter"
version.workspace = true
edition.workspace = true
rust-version.workspace = true

[lib]
crate-type = ["cdylib", "rlib"]

[dependencies]
kanbrick-core.workspace = true
kanbrick-guest-sdk.workspace = true
serde.workspace = true
serde_json.workspace = true
```

Add `"guests/greeter"` to the workspace `members` in the root `Cargo.toml`.

### 2. Pure logic + the entrypoint

`guests/greeter/src/lib.rs`:

```rust
use kanbrick_core::ClearanceLevel;

/// This guest needs at least Execution clearance.
pub const REQUIRED_CLEARANCE: ClearanceLevel = ClearanceLevel::L2;

/// Pure, native-testable logic: count visible companies for a greeting.
pub fn greeting(caller: &str, visible_companies: usize) -> String {
    format!("Hello {caller} — you can see {visible_companies} companies.")
}

// The wasm entrypoint: only compiled for wasm32, so native unit tests of the
// pure logic above never touch the host bindings.
#[cfg(target_arch = "wasm32")]
mod entrypoint {
    use super::*;
    use kanbrick_guest_sdk as sdk;
    use sdk::{GraphQuery, GuestRequest, GuestResponse, LogLevel};

    fn handle(_req: GuestRequest) -> sdk::Result<GuestResponse> {
        sdk::log(LogLevel::Info, "greeter: started");

        // Identity is host-authoritative — read it, never trust the payload.
        let ctx = sdk::firm_context()?;
        if ctx.clearance < REQUIRED_CLEARANCE {
            return Err(sdk::Error::AccessDenied {
                required: REQUIRED_CLEARANCE,
                actual: ctx.clearance,
            });
        }

        // query_graph is clearance-filtered by the host (you only get rows your
        // caller may see). The roster fields are public; detail is gated.
        let rows = sdk::query_graph(&GraphQuery::new(
            "MATCH (c:Company) RETURN c.company_id, c.name, c.segment",
        ))?;

        Ok(GuestResponse::new(sdk::serde_json::json!({
            "message": greeting(&ctx.email, rows.len()),
        })))
    }

    sdk::guest_entrypoint!(handle);
}

#[cfg(test)]
mod tests {
    #[test]
    fn greeting_reads_well() {
        assert_eq!(
            super::greeting("a@x.com", 9),
            "Hello a@x.com — you can see 9 companies."
        );
    }
}
```

### 3. Rules of the road

- **Identity** comes from `sdk::firm_context()` (host-authoritative). Never accept
  a caller-supplied identity in the request body.
- **Enforce your clearance** in the handler and return `Err(Error::AccessDenied
  {..})`; the SDK turns any handler error into a *structured* error response —
  guests never trap or panic on bad input.
- **All graph reads** go through `sdk::query_graph` and come back
  clearance-filtered. Project company `company_id`/`name`/`segment` for the public
  roster; any other field is gated.
- **Events**: `sdk::emit(&Event::with_payload("greeter.done", payload))` publishes
  onto the host bus for other guests to react to.

### 4. Wire it in

- **Tests / standalone**: the mesh build script (`kanbrick-mesh/build.rs`) and the
  API build script (`kanbrick-api/build.rs`) build guests to wasm; add your guest
  to their `GUESTS` lists to have it embedded and available to integration tests.
- **HTTP**: to serve it at `POST /guests/greeter`, register it in
  `kanbrick-api`'s `build_mesh` and add its minimum clearance to
  `guest_min_clearance`.
- **Native logic tests** run with `cargo test -p kanbrick-guest-greeter`; the
  wasm build with `cargo build -p kanbrick-guest-greeter --target wasm32-wasip1`.

See `guests/sdk-example` for the smallest end-to-end reference, and
`guests/{valuation,reporting,compliance}` for full business guests.
